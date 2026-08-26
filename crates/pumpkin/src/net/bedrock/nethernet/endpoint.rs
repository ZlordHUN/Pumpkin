use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CONTENT_TYPE, HOST},
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pumpkin_util::{jwt::Jwks, p384::ecdsa::SigningKey};
use pumpkin_world::{CURRENT_BEDROCK_MC_PROTOCOL, CURRENT_BEDROCK_MC_VERSION};
use serde::Serialize;
use tokio::{
    net::TcpListener,
    sync::{Mutex, OnceCell, mpsc},
};
use tracing::{debug, info, trace, warn};

use crate::{STOP_INTERRUPT, net::bedrock::status::IceSocket, server::Server};

use super::state::NetherNetState;
use super::{ice_router::IceRouter, peer::negotiate_direct, session::IncomingSession};

const MAX_SDP_SIZE: usize = 1 << 20;

/// Accepts Bedrock `NetherNet` connections negotiated through Mojang's HTTP endpoint.
pub struct NetherNetListener {
    incoming: Mutex<mpsc::Receiver<IncomingSession>>,
    local_addr: SocketAddr,
    state: NetherNetState,
}

impl NetherNetListener {
    pub async fn bind(
        server: &Arc<Server>,
        ice_socket: IceSocket,
        identity_key: Arc<SigningKey>,
        oidc_verifier: Option<Arc<OnceCell<(String, Jwks)>>>,
    ) -> std::io::Result<Self> {
        let config = &server.advanced_config.networking.bedrock.nethernet;
        let address = config.address;
        let listener = TcpListener::bind(address).await?;
        let local_addr = listener.local_addr()?;
        let ice_router = Arc::new(IceRouter::bind(ice_socket).await?);
        let ice_local_addr = ice_router.public_addr();
        let (incoming, receiver) = mpsc::channel(128);
        let state = NetherNetState {
            server: Arc::downgrade(server),
            incoming,
            identity_key,
            require_client_identity: server.advanced_config.networking.bedrock.online_mode,
            oidc_verifier,
            stun_servers: config.stun_servers.clone().into(),
            ice_local_addr,
            external_ip: config.external_ip,
            ice_router,
        };
        let router = Router::new()
            .route("/v1/join", get(status))
            .route("/v1/join/{network_id}", post(join))
            .layer(DefaultBodyLimit::max(MAX_SDP_SIZE))
            .with_state(state.clone());

        tokio::spawn(async move {
            let result = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(STOP_INTERRUPT.clone().cancelled_owned())
            .await;
            if let Err(error) = result {
                warn!("NetherNet signaling server stopped: {error}");
            }
        });

        info!("Bedrock NetherNet signaling is listening on {local_addr}");
        info!("Bedrock NetherNet ICE is listening on {ice_local_addr}");
        Ok(Self {
            incoming: Mutex::new(receiver),
            local_addr,
            state,
        })
    }

    pub async fn accept(&self) -> Option<IncomingSession> {
        self.incoming.lock().await.recv().await
    }

    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub(super) fn state(&self) -> NetherNetState {
        self.state.clone()
    }
}

/// Server-list status read from `GET /v1/join` by Bedrock 26.50 and newer.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NetherNetStatus<'a> {
    name: &'a str,
    protocol: u32,
    version: &'static str,
    level: &'a str,
    players: u32,
    max_players: u32,
    game_type: u8,
}

async fn status(
    State(state): State<NetherNetState>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
) -> Response {
    let Some(server) = state.server.upgrade() else {
        warn!(%address, "Rejected NetherNet status request because the server is stopping");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let players = server
        .get_status()
        .lock()
        .await
        .status_response
        .players
        .as_ref()
        .map_or(0, |players| players.online);
    let game_type = server.defaultgamemode.lock().await.gamemode as u8;
    let status = NetherNetStatus {
        name: &server.advanced_config.networking.bedrock.motd,
        protocol: CURRENT_BEDROCK_MC_PROTOCOL,
        version: CURRENT_BEDROCK_MC_VERSION,
        level: &server.basic_config.default_level_name,
        players,
        max_players: server.advanced_config.networking.bedrock.max_players,
        game_type,
    };
    let Ok(body) = serde_json::to_vec(&status) else {
        warn!(%address, "Failed to encode NetherNet server status");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let mut response = (StatusCode::OK, body).into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    trace!(
        %address,
        players,
        max_players = status.max_players,
        protocol = status.protocol,
        "Sent NetherNet server status"
    );
    response
}

async fn join(
    State(state): State<NetherNetState>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    Path(network_id): Path<String>,
    headers: HeaderMap,
    offer: Bytes,
) -> Response {
    trace!(%address, %network_id, length = offer.len(), "Received NetherNet SDP offer");
    if offer.is_empty() {
        debug!(%address, %network_id, "Rejected empty NetherNet SDP offer");
        return (StatusCode::BAD_REQUEST, "Missing SDP offer").into_response();
    }
    let Ok(offer) = String::from_utf8(offer.to_vec()) else {
        debug!(%address, %network_id, "Rejected non-UTF-8 NetherNet SDP offer");
        return (StatusCode::BAD_REQUEST, "SDP offer must be UTF-8").into_response();
    };

    let advertised_ip = headers
        .get(HOST)
        .and_then(|host| host.to_str().ok())
        .and_then(|host| host.parse::<axum::http::uri::Authority>().ok())
        .and_then(|authority| authority.host().parse().ok());
    match Box::pin(negotiate_direct(&state, address, &offer, advertised_ip)).await {
        Ok((answer, _session)) => {
            trace!(%address, %network_id, length = answer.len(), "Returning NetherNet SDP answer");
            let mut response = (StatusCode::OK, answer).into_response();
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/sdp"));
            response
        }
        Err(error) => {
            warn!("NetherNet negotiation with {address} failed: {error}");
            (StatusCode::BAD_REQUEST, error).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn encodes_nethernet_server_status() {
        let status = NetherNetStatus {
            name: "Pumpkin",
            protocol: 2192,
            version: "1.26.50",
            level: "world",
            players: 3,
            max_players: 20,
            game_type: 1,
        };

        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!({
                "name": "Pumpkin",
                "protocol": 2192,
                "version": "1.26.50",
                "level": "world",
                "players": 3,
                "maxPlayers": 20,
                "gameType": 1,
            })
        );
    }
}

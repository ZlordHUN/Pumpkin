//! Exercises player pairing and the real Bedrock FIFO, not a complete network login.

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::item::{BedrockItem, Item};
use pumpkin_data::item_stack::ItemStack;
use pumpkin_protocol::bedrock::client::remove_actor::CRemoveActor;
use pumpkin_protocol::bedrock::client::{CAddPlayer, CMobEquipment, CPlayerList, Skin};
use pumpkin_protocol::bedrock::network_item::NetworkItemStackDescriptor;
use pumpkin_protocol::bedrock::packet_decoder::BedrockBatchDecoder;
use pumpkin_protocol::bedrock::server::SSetLocalPlayerAsInitialized;
use pumpkin_protocol::codec::{var_long::VarLong, var_uint::VarUInt, var_ulong::VarULong};
use pumpkin_protocol::serial::PacketRead;
use pumpkin_protocol::{Packet, RawPacket};
use pumpkin_util::math::vector3::Vector3;
use uuid::Uuid;

use crate::entity::EntityBase;
use crate::entity::player::Player;
use crate::net::bedrock::BedrockClient;
use crate::test_support::TestServer;

async fn decode_packets(client: &BedrockClient) -> Vec<RawPacket> {
    let mut decoder = BedrockBatchDecoder::new();
    let mut packets = Vec::new();
    for data in client.drain_outgoing_packets_for_test().await {
        let payload = decoder.get_packet_payload(data.to_vec()).await.unwrap();
        let mut reader = Cursor::new(payload);
        packets.push(decoder.get_game_packet(&mut reader).unwrap());
        assert_eq!(reader.position() as usize, reader.get_ref().len());
    }
    packets
}

fn initialize_player(fixture: &TestServer, player: &Arc<Player>) {
    if let Some(client) = player.client.bedrock() {
        client.handle_set_local_player_as_initialized(
            player,
            &SSetLocalPlayerAsInitialized {
                player_id: VarULong(player.entity_id() as u64),
            },
        );
    } else {
        // This is the same activation called after Java's initial spawn setup.
        fixture.world.start_bedrock_player_tracking(player);
    }
}

fn assert_player_spawn(packets: &[RawPacket], subject: &Player, expected_skin: &Skin) {
    assert_eq!(
        packets.iter().map(|packet| packet.id).collect::<Vec<_>>(),
        [
            CPlayerList::PACKET_ID,
            CAddPlayer::PACKET_ID,
            CMobEquipment::PACKET_ID
        ],
        "one skin registration, one player actor, then held equipment"
    );

    let mut list = Cursor::new(&packets[0].payload);
    assert_eq!(VarUInt::read(&mut list).unwrap().0, 1);
    assert_eq!(VarUInt::read(&mut list).unwrap().0, 1);
    assert_eq!(u8::read(&mut list).unwrap(), CPlayerList::ACTION_ADD);
    assert_eq!(Uuid::read(&mut list).unwrap(), subject.gameprofile.id);
    assert_eq!(
        VarLong::read(&mut list).unwrap().0,
        i64::from(subject.entity_id())
    );
    assert_eq!(String::read(&mut list).unwrap(), subject.gameprofile.name);
    assert!(String::read(&mut list).unwrap().is_empty());
    assert!(String::read(&mut list).unwrap().is_empty());
    assert_eq!(i32::read(&mut list).unwrap(), -1);
    let skin = Skin::read(&mut list).unwrap();
    assert_eq!(skin.skin_id, expected_skin.skin_id);
    assert_eq!(skin.skin_data, expected_skin.skin_data);
    assert_eq!((skin.image_width, skin.image_height), (64, 64));
    assert_eq!(skin.arm_size, "slim");
    assert!(skin.is_trusted);

    let mut actor = Cursor::new(&packets[1].payload);
    assert_eq!(Uuid::read(&mut actor).unwrap(), subject.gameprofile.id);
    assert_eq!(String::read(&mut actor).unwrap(), subject.gameprofile.name);
    assert_eq!(
        VarULong::read(&mut actor).unwrap().0,
        subject.entity_id() as u64
    );
    assert!(String::read(&mut actor).unwrap().is_empty());
    let position = Vector3::new(
        f32::read(&mut actor).unwrap(),
        f32::read(&mut actor).unwrap(),
        f32::read(&mut actor).unwrap(),
    );
    assert_eq!(position, Vector3::new(5.5, 72.0, 5.5));

    let mut equipment = Cursor::new(&packets[2].payload);
    assert_eq!(
        VarULong::read(&mut equipment).unwrap().0,
        subject.entity_id() as u64
    );
    let item = NetworkItemStackDescriptor::read(&mut equipment).unwrap();
    assert_eq!(item.id, BedrockItem::DIAMOND_SWORD.id);
    assert_eq!(item.stack_size, 1);
}

async fn player_pairing_flow(bedrock_joins_first: bool) {
    let fixture = TestServer::new().await;
    let first = if bedrock_joins_first {
        fixture.new_bedrock_player().await
    } else {
        fixture.new_java_player().await
    };
    initialize_player(&fixture, &first);
    let second = if bedrock_joins_first {
        fixture.new_java_player().await
    } else {
        fixture.new_bedrock_player().await
    };
    let (subject, viewer) = if bedrock_joins_first {
        (&second, &first)
    } else {
        (&first, &second)
    };
    let client = viewer.client.bedrock().unwrap();
    let tracked = fixture
        .world
        .entity_tracker
        .get_tracked_entity(subject.entity_id())
        .unwrap();

    // The second player was registered by World::add_player, but is not initialized.
    assert!(!tracked.seen_by.contains(&viewer.gameprofile.id));
    assert!(decode_packets(client).await.is_empty());
    let mut skin = Skin::steve();
    skin.skin_id = "test:custom-java-skin".to_string();
    skin.full_id.clone_from(&skin.skin_id);
    skin.set_slim(true);
    skin.skin_data = [30, 60, 90, 255].repeat(64 * 64);
    subject.bedrock_skin.store(Arc::new(skin.clone()));
    subject
        .inventory
        .set_held_item(ItemStack::new(1, &Item::DIAMOND_SWORD));
    subject.get_entity().set_pos(Vector3::new(5.5, 72.0, 5.5));
    viewer.get_entity().set_pos(Vector3::new(5.5, 72.0, 6.5));
    fixture
        .world
        .entity_tracker
        .update_player_position(subject, &fixture.world);
    assert!(!tracked.seen_by.contains(&viewer.gameprofile.id));
    assert!(decode_packets(client).await.is_empty());

    initialize_player(&fixture, &second);
    assert!(tracked.seen_by.contains(&viewer.gameprofile.id));
    assert_player_spawn(&decode_packets(client).await, subject, &skin);
    initialize_player(&fixture, &first);
    initialize_player(&fixture, &second);
    fixture
        .world
        .entity_tracker
        .update_player_position(subject, &fixture.world);
    assert!(
        decode_packets(client).await.is_empty(),
        "initialization is idempotent"
    );

    // Subsequent terrain loading must not reset initial pairing readiness.
    subject.set_client_loaded(false);
    assert!(
        subject
            .bedrock_player_tracking_ready
            .load(Ordering::Acquire)
    );
    subject
        .get_entity()
        .set_pos(Vector3::new(1024.0, 72.0, 1024.0));
    fixture
        .world
        .entity_tracker
        .update_player_position(subject, &fixture.world);
    let removed = decode_packets(client).await;
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].id, CRemoveActor::PACKET_ID);
    assert!(!tracked.seen_by.contains(&viewer.gameprofile.id));

    subject.get_entity().set_pos(Vector3::new(5.5, 72.0, 5.5));
    fixture
        .world
        .entity_tracker
        .update_player_position(subject, &fixture.world);
    assert_player_spawn(&decode_packets(client).await, subject, &skin);
    assert!(tracked.seen_by.contains(&viewer.gameprofile.id));
    drop(tracked);
    drop(first);
    drop(second);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bedrock_first_waits_for_java_spawn_and_pairs_with_custom_skin() {
    player_pairing_flow(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn java_first_waits_for_bedrock_initialization_and_pairs_with_custom_skin() {
    player_pairing_flow(false).await;
}

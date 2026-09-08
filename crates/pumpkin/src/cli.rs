#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LaunchMode {
    #[default]
    Auto,
    Gui,
    Headless,
    Help,
    Version,
}

impl LaunchMode {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut mode = Self::Auto;
        for arg in args {
            let next = match arg.as_str() {
                "--gui" => Self::Gui,
                "--nogui" | "nogui" => Self::Headless,
                "--help" | "-h" => return Ok(Self::Help),
                "--version" | "-V" => return Ok(Self::Version),
                _ => return Err(format!("Unknown argument: {arg}. Use --help for options.")),
            };
            if mode != Self::Auto && mode != next {
                return Err("--gui and --nogui cannot be used together.".to_owned());
            }
            mode = next;
        }
        Ok(mode)
    }

    pub fn use_gui(self, supported: bool, display_available: bool) -> Result<bool, String> {
        match self {
            Self::Gui if !supported => Err(
                "This build has no desktop GUI. Build on Linux, Windows or macOS with --features gui."
                    .to_owned(),
            ),
            Self::Gui if !display_available => {
                Err("No desktop display is available. Use --nogui for a headless server.".to_owned())
            }
            Self::Gui => Ok(true),
            Self::Auto => Ok(supported && display_available),
            Self::Headless | Self::Help | Self::Version => Ok(false),
        }
    }
}

pub fn display_available() -> bool {
    if cfg!(target_os = "linux") {
        ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
    } else {
        cfg!(any(target_os = "windows", target_os = "macos"))
    }
}

#[cfg(test)]
mod tests {
    use super::LaunchMode;

    fn parse(args: &[&str]) -> Result<LaunchMode, String> {
        LaunchMode::parse(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn accepts_vanilla_nogui_and_explicit_modes() {
        assert_eq!(parse(&[]).unwrap(), LaunchMode::Auto);
        assert_eq!(parse(&["nogui"]).unwrap(), LaunchMode::Headless);
        assert_eq!(parse(&["--nogui"]).unwrap(), LaunchMode::Headless);
        assert_eq!(parse(&["--gui"]).unwrap(), LaunchMode::Gui);
        assert_eq!(parse(&["--help"]).unwrap(), LaunchMode::Help);
        assert_eq!(parse(&["--version"]).unwrap(), LaunchMode::Version);
    }

    #[test]
    fn rejects_conflicting_and_unknown_arguments() {
        assert!(parse(&["--gui", "--nogui"]).is_err());
        assert!(parse(&["--nogui", "--gui"]).is_err());
        assert!(parse(&["--unknown"]).is_err());
    }

    #[test]
    fn automatic_mode_keeps_headless_builds_and_hosts_headless() {
        for (supported, display, expected) in [
            (false, false, false),
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            assert_eq!(LaunchMode::Auto.use_gui(supported, display), Ok(expected));
            assert_eq!(LaunchMode::Headless.use_gui(supported, display), Ok(false));
        }
    }

    #[test]
    fn explicit_gui_reports_unsupported_or_missing_display() {
        assert!(LaunchMode::Gui.use_gui(false, true).is_err());
        assert!(LaunchMode::Gui.use_gui(true, false).is_err());
        assert_eq!(LaunchMode::Gui.use_gui(true, true), Ok(true));
    }
}

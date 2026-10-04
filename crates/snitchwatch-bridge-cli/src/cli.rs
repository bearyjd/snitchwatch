//! Command-line classification for `snitchwatch-bridge-cli`.
//!
//! The bridge takes no arguments: configuration is environment-only, and every
//! other argument is ignored (that has always been the behavior and is kept so
//! existing launchers and unit files keep working). The one exception is the
//! pair of informational flags below, which must be recognised *before* the
//! binary does any I/O — a bare `--help` used to start a full bridge, replacing
//! a running bridge's socket and token file before failing on the gRPC bind.
//!
//! Everything here is pure (no I/O, no globals) so it can be unit tested; the
//! binary's `main` decides what to do with the result.

use std::ffi::OsString;

/// An informational flag that makes the binary print something and exit
/// without starting the bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarlyExit {
    /// `--help` / `-h`: print [`usage`].
    Help,
    /// `--version` / `-V`: print [`version_line`].
    Version,
}

/// Classify `args` (the full argv, including argv[0], which is skipped).
///
/// Help wins over version when both are present, regardless of order. Any
/// other argument — including unknown flags — is ignored and yields `None`,
/// exactly as before these flags existed. Never panics on non-UTF-8 arguments.
pub fn early_exit<I: IntoIterator<Item = OsString>>(args: I) -> Option<EarlyExit> {
    let mut version = false;
    for arg in args.into_iter().skip(1) {
        if arg == "--help" || arg == "-h" {
            return Some(EarlyExit::Help);
        }
        if arg == "--version" || arg == "-V" {
            version = true;
        }
    }
    version.then_some(EarlyExit::Version)
}

/// The `--version` output: `<crate name> <crate version>`, no trailing newline.
pub fn version_line() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

/// The `--help` output, newline-terminated.
pub fn usage() -> String {
    format!(
        "\
{version}
Bridge between opensnitchd (gRPC) and the Snitchwatch GUI (WebSocket).

USAGE:
    snitchwatch-bridge-cli [-h | --help] [-V | --version]

With no flags the bridge starts and runs until SIGINT or SIGTERM. Any other
argument is ignored.

ENVIRONMENT (all optional):
    SNITCHWATCH_GRPC_BIND   gRPC bind address (default: 127.0.0.1:0)
    SNITCHWATCH_WS_SOCKET   Unix domain socket path for the WS server
                            (default: $XDG_RUNTIME_DIR/snitchwatch/bridge.sock)

OUTPUT:
    On startup these lines are printed to stdout so wrapping processes can
    discover where the bridge is listening:

    GRPC_LISTEN_ADDR=<addr>
    WS_SOCKET_PATH=<path>
    WS_TOKEN_PATH=<path>

    The token file is written with mode 0600 next to the socket.
",
        version = version_line()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn long_help_flag_is_help() {
        assert_eq!(
            early_exit(argv(&["bridge", "--help"])),
            Some(EarlyExit::Help)
        );
    }

    #[test]
    fn short_help_flag_is_help() {
        assert_eq!(early_exit(argv(&["bridge", "-h"])), Some(EarlyExit::Help));
    }

    #[test]
    fn long_version_flag_is_version() {
        assert_eq!(
            early_exit(argv(&["bridge", "--version"])),
            Some(EarlyExit::Version)
        );
    }

    #[test]
    fn short_version_flag_is_version() {
        assert_eq!(
            early_exit(argv(&["bridge", "-V"])),
            Some(EarlyExit::Version)
        );
    }

    #[test]
    fn help_wins_over_version_in_either_order() {
        assert_eq!(
            early_exit(argv(&["bridge", "--version", "--help"])),
            Some(EarlyExit::Help)
        );
        assert_eq!(
            early_exit(argv(&["bridge", "-h", "-V"])),
            Some(EarlyExit::Help)
        );
    }

    #[test]
    fn flag_is_found_anywhere_after_argv0() {
        assert_eq!(
            early_exit(argv(&["bridge", "whatever", "-V", "more"])),
            Some(EarlyExit::Version)
        );
    }

    #[test]
    fn argv0_alone_is_none() {
        assert_eq!(early_exit(argv(&["bridge"])), None);
    }

    #[test]
    fn empty_argv_is_none() {
        assert_eq!(early_exit(Vec::<OsString>::new()), None);
    }

    #[test]
    fn argv0_is_never_treated_as_a_flag() {
        assert_eq!(early_exit(argv(&["--help"])), None);
        assert_eq!(early_exit(argv(&["-V"])), None);
    }

    #[test]
    fn unrelated_args_are_ignored() {
        assert_eq!(early_exit(argv(&["bridge", "--bogus", "-x", "foo"])), None);
    }

    #[test]
    fn near_miss_flags_are_not_matched() {
        assert_eq!(
            early_exit(argv(&["bridge", "--helpme", "--versions", "-v", "-H", "-"])),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_args_do_not_panic() {
        use std::os::unix::ffi::OsStringExt;

        let bad = OsString::from_vec(vec![0xff, 0xfe, 0x2d]);
        assert_eq!(
            early_exit(vec![OsString::from("bridge"), bad.clone()]),
            None
        );
        assert_eq!(
            early_exit(vec![
                OsString::from("bridge"),
                bad,
                OsString::from("--version")
            ]),
            Some(EarlyExit::Version)
        );
    }

    #[test]
    fn version_line_is_name_and_version() {
        assert_eq!(
            version_line(),
            format!("snitchwatch-bridge-cli {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn usage_documents_env_vars_defaults_and_discovery_lines() {
        let text = usage();
        for needle in [
            "SNITCHWATCH_GRPC_BIND",
            "127.0.0.1:0",
            "SNITCHWATCH_WS_SOCKET",
            "$XDG_RUNTIME_DIR/snitchwatch/bridge.sock",
            "GRPC_LISTEN_ADDR=",
            "WS_SOCKET_PATH=",
            "WS_TOKEN_PATH=",
        ] {
            assert!(text.contains(needle), "usage missing `{needle}`:\n{text}");
        }
        assert!(text.starts_with(&version_line()));
        assert!(text.ends_with('\n'));
    }
}

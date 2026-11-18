# Security policy

Snitchwatch is pre-alpha software. It is a frontend for `opensnitchd`, and its bridge
handles a root-owned daemon's control channel, so security reports are welcome.

## Reporting a vulnerability

Please do not open a public issue for a security bug.

Report it privately through GitHub: the **Security** tab of this repository, then
**Report a vulnerability**. Include what you found, how to reproduce it, and which
flavour (system, or a dev-only setup) and release it affects.

This is a one-person project. Expect an acknowledgement within a few days, not hours.

## Known and tracked

- The system bridge is the only shipped flavour; its gRPC socket is root-only. Loopback
  TCP gRPC survives only as a dev option behind `SNITCHWATCH_DEV_TCP=1` (also gating the
  Tauri shell and the spike) and is not deny-safe: any local process that binds the port
  first can talk to the daemon. Nothing packaged sets the flag. See issue #35 and
  `docs/packaging/system-bridge-integration.md`.

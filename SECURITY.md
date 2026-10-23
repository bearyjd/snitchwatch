# Security policy

Snitchwatch is pre-alpha software. It is a frontend for `opensnitchd`, and its bridge
handles a root-owned daemon's control channel, so security reports are welcome.

## Reporting a vulnerability

Please do not open a public issue for a security bug.

Report it privately through GitHub: the **Security** tab of this repository, then
**Report a vulnerability**. Include what you found, how to reproduce it, and which
flavour (system or user) and release it affects.

This is a one-person project. Expect an acknowledgement within a few days, not hours.

## Known and tracked

- The user flavour of the bridge listens on TCP `127.0.0.1:50051` and is not deny-safe
  (any local process that binds the port first can talk to the daemon). The system
  flavour uses a root-only Unix socket instead. See issue #35 and
  `docs/packaging/system-bridge-integration.md`.

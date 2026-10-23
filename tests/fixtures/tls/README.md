# TLS test fixtures

`localhost-test-only.cert.der` and `localhost-test-only.key.der` are a
self-signed key pair for `127.0.0.1`, `127.0.0.2` and `localhost`, generated
with the `openssl` CLI. They exist only so
`crates/snitchwatch-bridge/src/blocklists/fetcher_tls_tests.rs` can run a local
HTTPS server. The private key is public on purpose and secures nothing: no
production code trusts this certificate. Do not reuse it anywhere else.

Secret scanners that flag the `.key.der` file can be pointed at the allowlist
in `.gitleaks.toml`.

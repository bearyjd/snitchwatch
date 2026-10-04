//! Generated tonic stubs for the OpenSnitch UI gRPC service.
//!
//! The proto file lives in `vendor/opensnitch/proto/ui.proto` and is compiled
//! at build time by `build.rs`. The generated code is exposed under [`protocol`].

// Generated code we don't control: clippy 1.98's `result_large_err` fires on
// tonic's server stubs (`Result<_, tonic::Status>`, a >128-byte Err), and
// clippy 1.99's `double_must_use` on its `#[must_use]` client futures.
// Lint-only; scoped to this module so hand-written crates still get the lints.
#[allow(clippy::result_large_err, clippy::double_must_use)]
pub mod protocol {
    tonic::include_proto!("protocol");
}

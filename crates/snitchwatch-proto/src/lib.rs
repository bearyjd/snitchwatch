//! Generated tonic stubs for the OpenSnitch UI gRPC service.
//!
//! The proto file lives in `vendor/opensnitch/proto/ui.proto` and is compiled
//! at build time by `build.rs`. The generated code is exposed under [`protocol`].

// Generated code we don't control: clippy 1.98's `result_large_err` fires on
// tonic's server stubs (`Result<_, tonic::Status>`, a >128-byte Err). Lint-only;
// scoped to this module so hand-written crates still get the lint.
#[allow(clippy::result_large_err)]
pub mod protocol {
    tonic::include_proto!("protocol");
}

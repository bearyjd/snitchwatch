//! The standalone CLI's subscriber. Dependency frame payloads are never logged.
use tracing::Level;
use tracing_subscriber::{
    filter::{filter_fn, FilterExt},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter, Layer,
};

/// Install logging once, before starting the bridge. A separate hard cap cannot
/// be overridden by a more specific RUST_LOG directive.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let frame_cap = filter_fn(|metadata| {
        let frame_logger = ["tungstenite", "tokio_tungstenite"].iter().any(|target| {
            metadata
                .target()
                .strip_prefix(target)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
        });
        !frame_logger || *metadata.level() <= Level::INFO
    });
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter.and(frame_cap)))
        .init();
}

#[cfg(test)]
mod tests;

//! Prompt-slot plan Part C through the gRPC service: `subscribe` keeps the
//! daemon's own settings (item 10).

use super::*;

#[tokio::test]
async fn subscribe_keeps_the_daemons_settings_and_echoes_the_config() {
    let cache = Arc::new(Mutex::new(ConnectionCache::new(8)));
    let (tx, _rx) = broadcast::channel(8);
    let svc = UiService::new(
        cache,
        tx,
        Arc::new(TrayStatePublisher::new()),
        Arc::new(NoticeBus::new()),
        Arc::new(FilterPause::new()),
    );
    assert_eq!(svc.daemon_config_handle().get(), None);

    let raw = r#"{"DefaultAction": "deny", "Stats": {"MaxEvents": 50}}"#;
    let echoed = svc
        .subscribe(Request::new(ClientConfig {
            config: raw.into(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(echoed.config, raw, "the daemon gets its own config back");
    let view = svc.daemon_config_handle().get().unwrap();
    assert_eq!(view.default_action.as_deref(), Some("deny"));
    assert_eq!(view.max_events, Some(50));
    assert_eq!(view.checksums_enabled, None);

    // A later subscribe with garbage replaces the view: nothing is known.
    svc.subscribe(Request::new(ClientConfig {
        config: "garbage".into(),
        ..Default::default()
    }))
    .await
    .unwrap();
    assert_eq!(svc.daemon_config_handle().default_row_action(), None);
}

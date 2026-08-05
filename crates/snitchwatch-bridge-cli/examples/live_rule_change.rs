//! Live verification harness for the CHANGE_RULE path against a **real**
//! `opensnitchd`. Mirrors `tests/mock_opensnitchd/examples/fire_ask_rule.rs`'s
//! role: a manual, human-run probe, deliberately not part of `cargo test`.
//!
//! Everything in the automated suite proves the bridge sends a well-formed
//! notification to a *mock*. This proves a real daemon **accepts and applies**
//! it — the gap issue #14 lived in, where every mock-driven test passed while
//! the real daemon rejected 100% of rules and silently applied its default.
//!
//! Usage:
//!
//! ```bash
//! # opensnitchd must be running and configured to dial 127.0.0.1:50051
//! RUST_LOG=info cargo run -p snitchwatch-bridge-cli --example live_rule_change -- change <rule-name> <file>
//! ```
//!
//! It re-sends an existing rule **unchanged**, so a successful run mutates
//! nothing: the daemon deserializes, validates, and `Replace`s the rule with
//! identical content. Success looks like `[notification] change rule:` in the
//! daemon's log plus a `NotificationReply` with code 0 (OK) in this process's
//! log; a rejected rule shows up as code 1 (ERROR) or no reply at all.
//!
//! The rule JSON is read from argv[2] (a file) so the caller controls exactly
//! what goes on the wire — including `precedence`/`nolog`, which must survive
//! the round trip or the daemon's wholesale `Replace` silently clears them.

use snitchwatch_bridge::ws_messages::ClientMessage;
use snitchwatch_bridge_cli::{run, BridgeConfig};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let verb = args.next().unwrap_or_default();
    let rule_name = args.next().unwrap_or_default();
    let rule_path = args.next();

    if rule_name.is_empty() || !matches!(verb.as_str(), "change" | "delete") {
        anyhow::bail!(
            "usage:\n  \
             live_rule_change change <rule-name> <rule-json-file>\n  \
             live_rule_change delete <rule-name>\n\n\
             `delete` destroys the named rule on the daemon — point it at a \
             throwaway rule, never a real one."
        );
    }

    // For `change`: read the rule the daemon already has, so we send back
    // byte-equivalent content and "did it apply?" isn't confounded by a real
    // edit. Unused for `delete`, which needs only the name.
    let rule: Option<serde_json::Value> = match (verb.as_str(), rule_path) {
        ("change", Some(p)) => Some(serde_json::from_str(&std::fs::read_to_string(&p)?)?),
        ("change", None) => anyhow::bail!("`change` needs a rule-json-file argument"),
        _ => None,
    };

    let tmp = std::env::temp_dir().join("snitchwatch-live");
    std::fs::create_dir_all(&tmp)?;

    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:50051".parse()?,
        ws_socket_path: tmp.join("bridge.sock"),
        cache_capacity: 64,
    };

    tracing::info!("starting bridge on 127.0.0.1:50051; waiting for opensnitchd to dial in");
    let bridge = run(cfg).await?;

    // The daemon retries its dial-out on a timer; give it room to connect and
    // open the Notifications stream before we push a command into it.
    tokio::time::sleep(Duration::from_secs(10)).await;

    let message = match rule {
        Some(rule) => {
            tracing::info!(%rule_name, "sending CHANGE_RULE with unchanged content");
            ClientMessage::UpdateRule {
                rule_id: rule_name,
                rule,
            }
        }
        None => {
            tracing::warn!(%rule_name, "sending DELETE_RULE — this destroys the rule");
            ClientMessage::DeleteRule { rule_id: rule_name }
        }
    };
    bridge.inbound_tx.send(message).await?;

    // Long enough for the daemon to deserialize, Replace, and reply.
    tokio::time::sleep(Duration::from_secs(6)).await;

    tracing::info!("done; shutting down");
    bridge.shutdown();
    Ok(())
}

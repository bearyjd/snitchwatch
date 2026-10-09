//! A Rules-page delete the daemon refuses (tower r12). opensnitchd drops the
//! rule from memory before it removes the file, and only removing the file
//! can fail, so the rule already stopped applying: the row leaves the list,
//! and the result says its file may bring it back at the next restart.

use super::tests::{bound, commands, result};
use crate::test_daemon::*;
use snitchwatch_bridge::ws_messages::{ClientMessage, RuleCommandOutcome, ServerMessage};
use std::time::Duration;
use tokio::sync::broadcast;

fn delete(rule_id: &str, id: Option<&str>) -> ClientMessage {
    ClientMessage::DeleteRule {
        rule_id: rule_id.into(),
        request_id: id.map(str::to_string),
        reply: None,
    }
}

fn listed(daemon: &Daemon) -> Vec<String> {
    let cache = daemon.cache.lock().unwrap();
    cache.rules().unwrap().keys().cloned().collect()
}

/// The names of the last `SetRules` and the `leftOnDisk` of the
/// `RulesNotShown` after it, once one says `left_on_disk`.
async fn list_until_left_on_disk(
    rx: &mut broadcast::Receiver<ServerMessage>,
    left_on_disk: u32,
) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut names = None;
        loop {
            match rx.recv().await.unwrap() {
                ServerMessage::SetRules { rules } => {
                    names = Some(
                        rules
                            .iter()
                            .map(|r| r["name"].as_str().unwrap().to_string())
                            .collect(),
                    )
                }
                ServerMessage::RulesNotShown {
                    left_on_disk: left, ..
                } if left == left_on_disk => {
                    if let Some(names) = names.take() {
                        return names;
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .expect("no list with that count")
}

#[tokio::test]
async fn a_refused_delete_unlists_the_rule_and_says_its_file_may_bring_it_back() {
    let rules = [bound("100-a", "deny"), bound("100-b", "deny")];
    let mut daemon = daemon(rules.to_vec());
    let model = model(&rules);
    model.lock().unwrap().stuck_files.insert("100-a".into());
    run_model(&mut daemon, model.clone());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut lists = daemon.broadcast.subscribe();

    commands.try_route(delete("100-a", Some("d1")));
    match result(&mut rx).await {
        RuleCommandOutcome::OkWithNote { note } => {
            assert!(note.starts_with("Deleted."), "{note}");
            assert!(note.contains("may come back"), "{note}");
            assert!(note.contains("operation not permitted"), "{note}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(listed(&daemon), vec!["100-b"]);
    assert_eq!(list_until_left_on_disk(&mut lists, 1).await, vec!["100-b"]);
    assert_eq!(applying(&model), vec!["100-b"], "it stopped applying");
    assert_eq!(files(&model), vec!["100-a", "100-b"], "its file stayed");
}

/// Kirigami's Rules page sends deletes without a request id (#48): no
/// result, but the list it gets has no row for the rule, and says a deleted
/// rule may come back.
#[tokio::test]
async fn without_a_request_id_the_list_alone_tells_the_gui() {
    let rules = [bound("100-a", "deny")];
    let mut daemon = daemon(rules.to_vec());
    let model = model(&rules);
    model.lock().unwrap().stuck_files.insert("100-a".into());
    run_model(&mut daemon, model.clone());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();

    commands.try_route(delete("100-a", None));
    assert_eq!(
        list_until_left_on_disk(&mut rx, 1).await,
        Vec::<String>::new()
    );
    assert!(listed(&daemon).is_empty());
}

/// A later delete of the same name is the no-op `OK` of a name not in
/// memory: the file stays, and so does the note.
#[tokio::test]
async fn a_second_delete_of_a_name_not_in_memory_keeps_the_note() {
    let rules = [bound("100-a", "deny")];
    let mut daemon = daemon(rules.to_vec());
    let model = model(&rules);
    model.lock().unwrap().stuck_files.insert("100-a".into());
    run_model(&mut daemon, model.clone());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(delete("100-a", Some("d1")));
    result(&mut rx).await;
    model.lock().unwrap().stuck_files.clear();

    commands.try_route(delete("100-a", Some("d2")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    assert_eq!(files(&model), vec!["100-a"]);
    assert!(daemon.cache.lock().unwrap().files_left().contains("100-a"));
}

/// The cache's own publish (`RulesSync::apply_refused`) already carries
/// the row's removal: the GUI's optimistic removal stands, and the list
/// goes out once, not again for the result.
#[tokio::test]
async fn a_refused_delete_publishes_the_list_once() {
    for request_id in [Some("d1"), None] {
        let rules = [bound("100-a", "deny")];
        let mut daemon = daemon(rules.to_vec());
        let model = model(&rules);
        model.lock().unwrap().stuck_files.insert("100-a".into());
        run_model(&mut daemon, model.clone());
        let commands = commands(&daemon);
        let mut rx = daemon.broadcast.subscribe();
        let mut lists = daemon.broadcast.subscribe();

        commands.try_route(delete("100-a", request_id));
        list_until_left_on_disk(&mut rx, 1).await;
        if request_id.is_some() {
            // Sent after any list of its own.
            result(&mut rx).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut published = 0;
        while let Ok(message) = lists.try_recv() {
            if matches!(message, ServerMessage::SetRules { .. }) {
                published += 1;
            }
        }
        assert_eq!(published, 1, "request id {request_id:?}");
    }
}

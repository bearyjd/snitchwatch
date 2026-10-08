//! A desktop notification with actions, answered only by the notification
//! server (PR #100 security review, HIGH).
//!
//! notify-rust's `wait_for_action` takes an `ActionInvoked` signal from
//! anyone on the session bus that names the right notification id, and ids
//! count up. So a sandboxed app could broadcast `ActionInvoked(N,
//! "allow-once")` and let its own connections through. Here instead:
//! - the notification is sent with `Notify` on the connection that also
//!   listens, so a server that targets its signals at the sender still
//!   reaches us;
//! - the listener's match rule names the server's unique name (looked up
//!   with `GetNameOwner`), path and interface, and [`classify`] checks the
//!   sender of every message again;
//! - only the caller's own action keys count. An unknown key, or a signal
//!   for another notification, is ignored and the wait goes on;
//! - if `org.freedesktop.Notifications` changes owner (the server
//!   restarted), the wait ends with no action.

use std::collections::HashMap;
use std::future::Future;

use futures_util::StreamExt;
use zbus::message::Type;
use zbus::names::{BusName, OwnedUniqueName, UniqueName};
use zbus::{Connection, MatchRule, Message, MessageStream};

pub(crate) const SERVER: &str = "org.freedesktop.Notifications";
pub(crate) const PATH: &str = "/org/freedesktop/Notifications";
pub(crate) const INTERFACE: &str = "org.freedesktop.Notifications";

/// What one message means for notification `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// The server says the user chose this one of our action keys.
    Action(&'static str),
    /// The server closed the notification.
    Closed,
    /// Anything else, including any message not from the server.
    Ignore,
}

/// Classify `msg` for notification `id`, trusting only `owner` and only
/// action keys in `keys`.
pub(crate) fn classify(
    msg: &Message,
    owner: &UniqueName<'_>,
    id: u32,
    keys: &[&'static str],
) -> Signal {
    let header = msg.header();
    let from_server = header.message_type() == Type::Signal
        && header.sender() == Some(owner)
        && header.path().is_some_and(|path| path.as_str() == PATH)
        && header
            .interface()
            .is_some_and(|interface| interface.as_str() == INTERFACE);
    if !from_server {
        return Signal::Ignore;
    }
    match header.member().map(|member| member.as_str()) {
        Some("ActionInvoked") => match msg.body().deserialize::<(u32, String)>() {
            Ok((notification, key)) if notification == id => keys
                .iter()
                .find(|ours| **ours == key)
                .map_or(Signal::Ignore, |ours| Signal::Action(ours)),
            _ => Signal::Ignore,
        },
        Some("NotificationClosed") => match msg.body().deserialize::<(u32, u32)>() {
            Ok((notification, _reason)) if notification == id => Signal::Closed,
            _ => Signal::Ignore,
        },
        _ => Signal::Ignore,
    }
}

/// How waiting on a notice ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitEnd {
    Action(&'static str),
    Closed,
    /// The server changed owner or went away: the notice's actions are void.
    ServerChanged,
    /// The caller's `stop` future finished first.
    Stopped,
}

/// A shown notification and the subscription that hears its answer.
pub(crate) struct Notice {
    conn: Connection,
    owner: OwnedUniqueName,
    id: u32,
    signals: MessageStream,
    owner_changes: zbus::fdo::NameOwnerChangedStream,
}

impl Notice {
    /// Show a notification with `actions` (key, label). Listens first, in
    /// this order: owner changes, the owner, the server's signals, then
    /// `Notify`. If the reply comes from a different owner, the server
    /// changed in between: the notification is closed and nothing is
    /// returned.
    pub(crate) async fn show(
        conn: &Connection,
        summary: &str,
        body: &str,
        actions: &[(&str, &str)],
    ) -> zbus::Result<Self> {
        let dbus = zbus::fdo::DBusProxy::new(conn).await?;
        let owner_changes = dbus
            .receive_name_owner_changed_with_args(&[(0, SERVER)])
            .await?;
        let owner = dbus.get_name_owner(BusName::try_from(SERVER)?).await?;
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .sender(owner.as_str())?
            .path(PATH)?
            .interface(INTERFACE)?
            .build();
        let signals = MessageStream::for_match_rule(rule, conn, Some(16)).await?;
        let flat: Vec<&str> = actions
            .iter()
            .flat_map(|(key, label)| [*key, *label])
            .collect();
        let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
        let reply = conn
            .call_method(
                Some(SERVER),
                PATH,
                Some(INTERFACE),
                "Notify",
                &(
                    "Snitchwatch",
                    0u32,
                    "security-high",
                    summary,
                    body,
                    flat,
                    hints,
                    -1i32,
                ),
            )
            .await?;
        let id: u32 = reply.body().deserialize()?;
        let notice = Self {
            conn: conn.clone(),
            owner,
            id,
            signals,
            owner_changes,
        };
        if reply.header().sender() != Some(&*notice.owner) {
            notice.close().await;
            return Err(zbus::Error::Failure(
                "the notification server changed while showing".into(),
            ));
        }
        Ok(notice)
    }

    /// Wait for one of `keys`, the notice closing, the server changing, or
    /// `stop`.
    pub(crate) async fn wait(
        &mut self,
        keys: &[&'static str],
        stop: impl Future<Output = ()>,
    ) -> WaitEnd {
        tokio::pin!(stop);
        loop {
            tokio::select! {
                message = self.signals.next() => match message {
                    Some(Ok(message)) => match classify(&message, &self.owner, self.id, keys) {
                        Signal::Action(key) => return WaitEnd::Action(key),
                        Signal::Closed => return WaitEnd::Closed,
                        Signal::Ignore => {}
                    },
                    Some(Err(error)) => tracing::debug!(%error, "unreadable notification signal"),
                    None => return WaitEnd::ServerChanged,
                },
                _ = self.owner_changes.next() => return WaitEnd::ServerChanged,
                _ = &mut stop => return WaitEnd::Stopped,
            }
        }
    }

    /// Ask the server to close the notification. Best effort.
    pub(crate) async fn close(&self) {
        if let Err(error) = self
            .conn
            .call_method(
                Some(SERVER),
                PATH,
                Some(INTERFACE),
                "CloseNotification",
                &(self.id,),
            )
            .await
        {
            tracing::debug!(%error, "closing the notification failed");
        }
    }
}

#[cfg(test)]
#[path = "notification_signals/bus_tests.rs"]
mod bus_tests;

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: [&str; 3] = ["allow-once", "deny", "review"];
    const SERVER_NAME: &str = ":1.5";

    fn signal(
        sender: &str,
        path: &str,
        interface: &str,
        member: &str,
        body: &(u32, &str),
    ) -> Message {
        Message::signal(path, interface, member)
            .unwrap()
            .sender(sender)
            .unwrap()
            .build(body)
            .unwrap()
    }

    fn classified(msg: &Message) -> Signal {
        classify(msg, &UniqueName::try_from(SERVER_NAME).unwrap(), 7, &KEYS)
    }

    #[test]
    fn only_the_servers_signal_for_our_notice_and_key_counts() {
        let action = |sender, id, key| signal(sender, PATH, INTERFACE, "ActionInvoked", &(id, key));
        assert_eq!(
            classified(&action(SERVER_NAME, 7, "allow-once")),
            Signal::Action("allow-once")
        );
        assert_eq!(
            classified(&action(SERVER_NAME, 7, "deny")),
            Signal::Action("deny")
        );
        // Another sender on the bus, with the right id and key.
        assert_eq!(
            classified(&action(":1.99", 7, "allow-once")),
            Signal::Ignore
        );
        // Another notification, or a key that isn't ours.
        assert_eq!(
            classified(&action(SERVER_NAME, 8, "allow-once")),
            Signal::Ignore
        );
        for key in ["default", "allow", "allow-forever", "", "Allow-once"] {
            assert_eq!(
                classified(&action(SERVER_NAME, 7, key)),
                Signal::Ignore,
                "{key:?}"
            );
        }
        // Right sender, wrong path or interface.
        let elsewhere = signal(
            SERVER_NAME,
            "/org/example",
            INTERFACE,
            "ActionInvoked",
            &(7, "deny"),
        );
        assert_eq!(classified(&elsewhere), Signal::Ignore);
        let other = signal(
            SERVER_NAME,
            PATH,
            "org.example.Notifications",
            "ActionInvoked",
            &(7, "deny"),
        );
        assert_eq!(classified(&other), Signal::Ignore);
    }

    #[test]
    fn the_server_closing_our_notice_ends_the_wait() {
        let closed = |sender, id: u32| {
            Message::signal(PATH, INTERFACE, "NotificationClosed")
                .unwrap()
                .sender(sender)
                .unwrap()
                .build(&(id, 2u32))
                .unwrap()
        };
        assert_eq!(classified(&closed(SERVER_NAME, 7)), Signal::Closed);
        assert_eq!(classified(&closed(SERVER_NAME, 8)), Signal::Ignore);
        assert_eq!(classified(&closed(":1.99", 7)), Signal::Ignore);
    }
}

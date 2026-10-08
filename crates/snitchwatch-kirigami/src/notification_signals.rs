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
//! - if the server that showed the notice loses `org.freedesktop.Notifications`
//!   (it restarted, or another server took over), the wait ends with no
//!   action, and [`Notice::close`] goes to that server, not the new one. A
//!   change that isn't that server losing the name, such as a server started
//!   on demand taking it while the notice was being shown, is ignored (PR
//!   #100 re-review).
//!
//! A notice stays answerable until its caller stops waiting (r11, Plasma
//! 6.7.4 in a Flatpak):
//! - it is sent `resident`, so a server keeps it, and its buttons, after the
//!   popup times out or a button is clicked; [`Notice::close`] removes it.
//!   (Plasma 6.3 and older otherwise close an expired popup and strip its
//!   actions; 6.4 and later keep them for a notice with actions.)
//! - the server saying it *expired* (`NotificationClosed` reason 1) doesn't
//!   end the wait: a server can still show it, in a history, and send its
//!   click later. Any other close (dismissed, closed by a call, undefined)
//!   ends it.
//! - it names its desktop entry, the Flatpak app id, so the server can tie
//!   it to the app rather than guess from the sender's process.

use std::collections::HashMap;
use std::future::Future;

use futures_util::StreamExt;
use zbus::fdo::{DBusProxy, NameOwnerChanged, NameOwnerChangedStream};
use zbus::message::Type;
use zbus::names::{BusName, OwnedUniqueName, UniqueName};
use zbus::{Connection, MatchRule, Message, MessageStream};

pub(crate) const SERVER: &str = "org.freedesktop.Notifications";
pub(crate) const PATH: &str = "/org/freedesktop/Notifications";
pub(crate) const INTERFACE: &str = "org.freedesktop.Notifications";
/// The app's desktop entry (its Flatpak app id), sent as the
/// `desktop-entry` hint.
pub(crate) const DESKTOP_ENTRY: &str = "org.snitchwatch.Snitchwatch";
/// `NotificationClosed` reasons, from the Desktop Notifications spec.
pub(crate) const EXPIRED: u32 = 1;

/// What one message means for notification `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// The server says the user chose this one of our action keys.
    Action(&'static str),
    /// The server closed the notification, for this reason.
    Closed(u32),
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
            Ok((notification, key)) if notification == id => {
                let ours = keys.iter().find(|ours| **ours == key);
                if ours.is_none() {
                    tracing::info!(
                        id,
                        ?key,
                        "notification server sent an action that isn't ours; ignored"
                    );
                }
                ours.map_or(Signal::Ignore, |ours| Signal::Action(ours))
            }
            _ => Signal::Ignore,
        },
        Some("NotificationClosed") => match msg.body().deserialize::<(u32, u32)>() {
            Ok((notification, reason)) if notification == id => Signal::Closed(reason),
            _ => Signal::Ignore,
        },
        _ => Signal::Ignore,
    }
}

/// How waiting on a notice ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitEnd {
    Action(&'static str),
    /// The server closed it for this reason, other than expiry.
    Closed(u32),
    /// The server that showed the notice lost the name (it went away or was
    /// replaced): the notice's actions are void.
    ServerChanged,
    /// The caller's `stop` future finished first.
    Stopped,
}

/// A shown notification and the subscription that hears its answer.
pub(crate) struct Notice {
    conn: Connection,
    /// The server that showed it: the only sender trusted.
    owner: OwnedUniqueName,
    id: u32,
    signals: MessageStream,
    owner_changes: NameOwnerChangedStream,
}

impl Notice {
    /// Show a notification with `actions` (key, label). Listens first, in
    /// this order: owner changes, the owner, the server's signals, then
    /// `Notify`. If the reply comes from a different owner, the server
    /// changed in between: the notification is closed on the server that
    /// answered, and nothing is returned.
    pub(crate) async fn show(
        conn: &Connection,
        summary: &str,
        body: &str,
        actions: &[(&str, &str)],
    ) -> zbus::Result<Self> {
        let dbus = DBusProxy::new(conn).await?;
        let owner_changes = hear_owner_changes(&dbus).await?;
        Self::show_heard(conn, &dbus, owner_changes, summary, body, actions).await
    }

    /// The rest of [`Notice::show`], once `owner_changes` is heard. Apart so
    /// a test can take the server's name in between, as a server started on
    /// demand does.
    async fn show_heard(
        conn: &Connection,
        dbus: &DBusProxy<'_>,
        owner_changes: NameOwnerChangedStream,
        summary: &str,
        body: &str,
        actions: &[(&str, &str)],
    ) -> zbus::Result<Self> {
        // A server that starts on demand (dunst, say) has no owner until
        // something calls it; notify-rust's `Notify` used to start it.
        if let Err(error) = dbus
            .start_service_by_name(zbus::names::WellKnownName::try_from(SERVER)?, 0)
            .await
        {
            tracing::debug!(%error, "the notification server didn't start on demand");
        }
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
                    hints(),
                    -1i32,
                ),
            )
            .await?;
        let id: u32 = reply.body().deserialize()?;
        let shown_by = reply.header().sender().cloned();
        if shown_by.as_ref() != Some(&*owner) {
            tracing::info!(id, server = %owner, ?shown_by, "notification server changed while showing a notice");
            if let Some(shown_by) = shown_by {
                close_on(conn, &shown_by, id).await;
            }
            return Err(zbus::Error::Failure(
                "the notification server changed while showing".into(),
            ));
        }
        tracing::info!(
            id,
            server = %owner,
            us = ?conn.unique_name().map(|name| name.as_str()),
            "notice shown"
        );
        Ok(Self {
            conn: conn.clone(),
            owner,
            id,
            signals,
            owner_changes,
        })
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
            // Owner changes first, so a click queued behind one loses.
            let end = tokio::select! {
                biased;
                change = self.owner_changes.next() => {
                    if !voids(change.as_ref(), &self.owner) {
                        continue;
                    }
                    WaitEnd::ServerChanged
                }
                message = self.signals.next() => match message {
                    Some(Ok(message)) => match classify(&message, &self.owner, self.id, keys) {
                        Signal::Action(key) => WaitEnd::Action(key),
                        Signal::Closed(EXPIRED) => {
                            tracing::info!(id = self.id, "notice expired; its buttons still count");
                            continue;
                        }
                        Signal::Closed(reason) => WaitEnd::Closed(reason),
                        Signal::Ignore => continue,
                    },
                    Some(Err(error)) => {
                        tracing::debug!(%error, "unreadable notification signal");
                        continue;
                    }
                    None => WaitEnd::ServerChanged,
                },
                _ = &mut stop => WaitEnd::Stopped,
            };
            tracing::info!(id = self.id, ?end, "notice wait ended");
            return end;
        }
    }

    /// Ask the server that showed the notification to close it. Best
    /// effort. It goes to that server's unique name, not to whoever owns
    /// `org.freedesktop.Notifications` now: after a takeover, the new
    /// server's notification with this id is somebody else's.
    pub(crate) async fn close(&self) {
        tracing::info!(id = self.id, server = %self.owner, "closing notice");
        close_on(&self.conn, &self.owner, self.id).await;
    }
}

/// `Notify`'s hints: see the module doc.
fn hints() -> HashMap<&'static str, zbus::zvariant::Value<'static>> {
    HashMap::from([
        ("resident", zbus::zvariant::Value::from(true)),
        ("desktop-entry", zbus::zvariant::Value::from(DESKTOP_ENTRY)),
    ])
}

/// Whether a change of the server's owner voids a notice shown by `owner`:
/// only when `owner` lost the name. Any other change is ignored, above all
/// a server started on demand taking the name (`""` to it) while `show`
/// was looking its owner up. An unreadable change, or the end of the
/// changes, voids it.
fn voids(change: Option<&NameOwnerChanged>, owner: &UniqueName<'_>) -> bool {
    let Some(change) = change else {
        return true;
    };
    match change.args() {
        Ok(args) => {
            let lost = args.old_owner().as_ref() == Some(owner);
            tracing::info!(
                old = ?args.old_owner().as_ref().map(|name| name.as_str()),
                new = ?args.new_owner().as_ref().map(|name| name.as_str()),
                voids = lost,
                "notification server owner changed"
            );
            lost
        }
        Err(error) => {
            tracing::info!(%error, "unreadable owner change of the notification server");
            true
        }
    }
}

/// How long a `CloseNotification` may take before it is given up on.
pub(crate) const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Ask notification server `server` (a unique name) to close notification
/// `id`. Best effort, and given up after [`CLOSE_WAIT`]: a server that
/// doesn't answer mustn't hold up whoever waits on this.
async fn close_on(conn: &Connection, server: &UniqueName<'_>, id: u32) {
    let body = (id,);
    let call = conn.call_method(
        Some(server.as_str()),
        PATH,
        Some(INTERFACE),
        "CloseNotification",
        &body,
    );
    match tokio::time::timeout(CLOSE_WAIT, call).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::info!(%error, id, %server, "closing the notification failed")
        }
        Err(_) => tracing::info!(
            id,
            %server,
            wait = ?CLOSE_WAIT,
            "the notification server didn't answer CloseNotification; given up"
        ),
    }
}

/// `org.freedesktop.Notifications`' owner changes, heard before its owner
/// is looked up.
async fn hear_owner_changes(dbus: &DBusProxy<'_>) -> zbus::Result<NameOwnerChangedStream> {
    dbus.receive_name_owner_changed_with_args(&[(0, SERVER)])
        .await
}

#[cfg(test)]
#[path = "notification_signals/bus_tests.rs"]
pub(crate) mod bus_tests;

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
    fn the_server_closing_our_notice_says_why() {
        let closed = |sender, id: u32, reason: u32| {
            Message::signal(PATH, INTERFACE, "NotificationClosed")
                .unwrap()
                .sender(sender)
                .unwrap()
                .build(&(id, reason))
                .unwrap()
        };
        for reason in [EXPIRED, 2, 3, 4] {
            assert_eq!(
                classified(&closed(SERVER_NAME, 7, reason)),
                Signal::Closed(reason)
            );
        }
        assert_eq!(classified(&closed(SERVER_NAME, 8, 2)), Signal::Ignore);
        assert_eq!(classified(&closed(":1.99", 7, 2)), Signal::Ignore);
    }

    /// Plasma 6.3 and older strip an expired notice's buttons unless it is
    /// resident, and clicking a non-resident one closes it before we act.
    #[test]
    fn a_notice_is_resident_and_names_its_desktop_entry() {
        let hints = hints();
        assert_eq!(hints.len(), 2, "{hints:?}");
        assert_eq!(hints["resident"], zbus::zvariant::Value::Bool(true));
        assert_eq!(
            hints["desktop-entry"],
            zbus::zvariant::Value::from("org.snitchwatch.Snitchwatch")
        );
        // The desktop entry is the Flatpak's app id, in both manifests.
        for manifest in [
            include_str!("../../../packaging/flatpak/org.snitchwatch.Snitchwatch.yml"),
            include_str!("../../../packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml"),
        ] {
            assert!(
                manifest
                    .lines()
                    .any(|line| line == format!("app-id: {DESKTOP_ENTRY}")),
                "a Flatpak manifest's app-id drifted from DESKTOP_ENTRY"
            );
        }
    }
}

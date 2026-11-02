//! A change counter for the link, so the GUI waits for link changes instead
//! of polling for them while idle (perf/gui-idle-wakeups). Bumped wherever
//! the link status or the session (connected, connection id) is written.
//! Receivers re-read the real state; the count itself carries nothing.

use std::sync::LazyLock;

use tokio::sync::watch;

static LINK_WAKE: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::channel(0).0);

/// Wake every receiver. Works with no receiver yet (`send_modify` always
/// stores the new count).
pub(crate) fn bump() {
    LINK_WAKE.send_modify(|count| *count = count.wrapping_add(1));
}

/// A receiver that sees every later [`bump`] (`changed()` resolves at once
/// if one happened since the last `borrow_and_update`).
pub fn subscribe() -> watch::Receiver<u64> {
    LINK_WAKE.subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_bump_before_waiting_is_not_lost() {
        let mut rx = subscribe();
        rx.borrow_and_update();
        bump();
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.changed())
            .await
            .expect("a bump made before the wait wakes it")
            .expect("the sender lives forever");
    }
}

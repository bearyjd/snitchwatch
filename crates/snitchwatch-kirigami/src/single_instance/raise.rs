//! Where a raise request goes: to the window once it exists, or held as one
//! pending raise until it does. Qt-free; `NotificationController` registers
//! the callback that queues `raiseRequested` on the Qt thread.

use std::sync::{Arc, Mutex, OnceLock};

type RaiseCallback = Box<dyn Fn() + Send>;

#[derive(Default)]
struct State {
    pending: bool,
    callback: Option<RaiseCallback>,
}

/// One raise target. A raise that arrives before [`RaiseSink::register`] is
/// remembered (several early ones fold into one) and delivered by it.
#[derive(Default)]
pub struct RaiseSink {
    state: Mutex<State>,
}

impl RaiseSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// A raise request, from the D-Bus object (a zbus worker thread).
    pub fn raise(&self) {
        let mut state = self.lock();
        match state.callback.as_ref() {
            Some(callback) => callback(),
            None => state.pending = true,
        }
    }

    /// Set where raises go from now on, and deliver the one that came early,
    /// if any. The pending flag is taken under the same lock the callback is
    /// installed under, so an early raise is delivered exactly once.
    pub fn register(&self, callback: impl Fn() + Send + 'static) {
        let mut state = self.lock();
        let pending = std::mem::take(&mut state.pending);
        state.callback = Some(Box::new(callback));
        if pending {
            if let Some(callback) = state.callback.as_ref() {
                callback();
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The process's raise target, shared by [`super::acquire`] and the
/// `NotificationController` that registers the window's callback.
pub fn raise_sink() -> Arc<RaiseSink> {
    static SINK: OnceLock<Arc<RaiseSink>> = OnceLock::new();
    SINK.get_or_init(|| Arc::new(RaiseSink::new())).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn counting(sink: &RaiseSink) -> Arc<AtomicU32> {
        let count = Arc::new(AtomicU32::new(0));
        let seen = count.clone();
        sink.register(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        count
    }

    #[test]
    fn a_raise_before_register_is_delivered_once_on_register() {
        let sink = RaiseSink::new();
        sink.raise();
        sink.raise();
        let count = counting(&sink);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn without_an_early_raise_register_delivers_nothing() {
        let sink = RaiseSink::new();
        let count = counting(&sink);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        sink.raise();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}

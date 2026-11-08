//! One GUI per session (per app id; both Flatpak flavours share it).
//!
//! The first instance owns the well-known D-Bus name
//! `org.snitchwatch.Snitchwatch` and serves `org.freedesktop.Application` at
//! `/org/snitchwatch/Snitchwatch`. A second launch finds the name taken,
//! calls `Activate` on the owner (which raises its window) and exits 0.
//!
//! **Fail open.** The process exits only on a *positive* result (the owner
//! answered `Activate`). No session bus, a bus error, or an owner that does
//! not answer in time (wedged, or shutting down) all mean "run as an
//! ordinary instance", logged, so a hung first instance never makes the app
//! impossible to start. Demo mode and `SNITCHWATCH_ALLOW_MULTIPLE=1` skip
//! the guard entirely.
//!
//! **What the bus can do to us.** Any client of the same session bus may
//! call the three methods; each one only raises the window. Their arguments
//! (URIs, action names, platform data) are never read.
//!
//! Qt-free: the raise goes through [`RaiseSink`], whose callback
//! `NotificationController` registers once the window exists. The blocking
//! zbus API runs on zbus' own runtime, so nothing here may be called from
//! inside a tokio runtime (`main.rs` calls it on the main thread).

mod raise;

pub use raise::{raise_sink, RaiseSink};

use std::collections::HashMap;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use zbus::blocking::{connection, Connection};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::zvariant::{OwnedValue, Value};

/// The well-known name: the app id.
pub const BUS_NAME: &str = "org.snitchwatch.Snitchwatch";
/// Where the `org.freedesktop.Application` object is served.
pub const OBJECT_PATH: &str = "/org/snitchwatch/Snitchwatch";
const APPLICATION_IFACE: &str = "org.freedesktop.Application";
/// Env var that skips the guard (exactly `1`), for running two builds.
pub const ENV_ALLOW_MULTIPLE: &str = "SNITCHWATCH_ALLOW_MULTIPLE";

/// How long a second instance waits for the owner's answer, and how long it
/// pauses before its one retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwardPolicy {
    pub timeout: Duration,
    pub retry_delay: Duration,
}

impl ForwardPolicy {
    pub const DEFAULT: Self = Self {
        timeout: Duration::from_secs(2),
        retry_delay: Duration::from_millis(500),
    };
}

/// Why the guard did not decide anything: the process runs normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Demo,
    AllowMultiple,
    NoSessionBus,
    /// Connecting to the bus or asking for the name failed.
    BusError,
    /// The name is owned but `Activate` failed or timed out, twice.
    OwnerUnresponsive,
}

/// What [`acquire`] decided.
#[derive(Debug)]
pub enum Outcome {
    /// This process owns the name; keep the guard until shutdown starts.
    Primary(Guard),
    /// The running instance was asked to raise its window: exit 0.
    Forwarded,
    /// Run as an ordinary instance.
    Skipped(SkipReason),
}

/// Owns the name while alive; releases it when dropped.
pub struct Guard {
    conn: Connection,
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("single_instance::Guard")
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Bounded by the connection's method timeout. Closing the connection
        // would release it too; this makes it happen now, not when the last
        // zbus task lets go of the connection.
        if let Err(error) = self.conn.release_name(BUS_NAME) {
            tracing::warn!(%error, "single instance: could not release the bus name");
        }
    }
}

/// What the guard reads from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvSnapshot {
    pub demo: bool,
    pub allow_multiple: bool,
    /// The session bus address, if one is reachable at all.
    pub bus_address: Option<String>,
}

impl EnvSnapshot {
    pub fn from_process() -> Self {
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR");
        Self {
            demo: crate::demo::enabled(),
            allow_multiple: allow_multiple_flag(std::env::var(ENV_ALLOW_MULTIPLE).ok().as_deref()),
            bus_address: session_bus_address(
                std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().as_deref(),
                runtime_dir.as_deref().map(Path::new),
            ),
        }
    }
}

/// `SNITCHWATCH_ALLOW_MULTIPLE` counts only as exactly `1`, like `SNITCHWATCH_DEMO`.
pub fn allow_multiple_flag(value: Option<&str>) -> bool {
    value == Some("1")
}

/// `DBUS_SESSION_BUS_ADDRESS` if set, else `$XDG_RUNTIME_DIR/bus` if that is
/// a socket (what libdbus and zbus fall back to), else none.
pub fn session_bus_address(explicit: Option<&str>, runtime_dir: Option<&Path>) -> Option<String> {
    if let Some(address) = explicit.filter(|address| !address.is_empty()) {
        return Some(address.to_string());
    }
    let socket = runtime_dir?.join("bus");
    let is_socket = std::fs::metadata(&socket)
        .map(|meta| meta.file_type().is_socket())
        .unwrap_or(false);
    is_socket.then(|| format!("unix:path={}", socket.display()))
}

/// Whether to skip the guard, before touching any bus.
pub fn skip_reason(env: &EnvSnapshot) -> Option<SkipReason> {
    if env.demo {
        Some(SkipReason::Demo)
    } else if env.allow_multiple {
        Some(SkipReason::AllowMultiple)
    } else if env.bus_address.is_none() {
        Some(SkipReason::NoSessionBus)
    } else {
        None
    }
}

/// The process's guard: reads the environment, raises into [`raise_sink`].
pub fn acquire() -> Outcome {
    let env = EnvSnapshot::from_process();
    let outcome = acquire_with(&env, ForwardPolicy::DEFAULT, raise_sink());
    log_outcome(&outcome);
    outcome
}

/// [`acquire`] with everything it reads passed in (the tests' entry).
pub fn acquire_with(env: &EnvSnapshot, policy: ForwardPolicy, sink: Arc<RaiseSink>) -> Outcome {
    if let Some(reason) = skip_reason(env) {
        return Outcome::Skipped(reason);
    }
    match env.bus_address.as_deref() {
        Some(address) => acquire_at(address, policy, sink),
        None => Outcome::Skipped(SkipReason::NoSessionBus),
    }
}

/// Serve the object, then ask for the name (never replacing, never queued).
pub fn acquire_at(address: &str, policy: ForwardPolicy, sink: Arc<RaiseSink>) -> Outcome {
    let conn = match connect_serving(address, policy, sink) {
        Ok(conn) => conn,
        Err(error) => {
            tracing::error!(%error, "single instance: no usable session bus; running anyway");
            return Outcome::Skipped(SkipReason::BusError);
        }
    };
    match conn.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            Outcome::Primary(Guard { conn })
        }
        // zbus 5 reports a taken name as `Error::NameTaken`
        // (`connection/mod.rs`, `request_name_with_flags`); the raw reply is
        // matched too, so either form forwards.
        Err(zbus::Error::NameTaken) | Ok(RequestNameReply::Exists) => forward(&conn, policy),
        Ok(RequestNameReply::InQueue) => {
            // Impossible with DoNotQueue; leave the queue and run anyway.
            let _ = conn.release_name(BUS_NAME);
            tracing::error!("single instance: queued for the bus name; running anyway");
            Outcome::Skipped(SkipReason::BusError)
        }
        Err(error) => {
            tracing::error!(
                %error,
                "single instance: could not ask for the bus name; running anyway"
            );
            Outcome::Skipped(SkipReason::BusError)
        }
    }
}

fn connect_serving(
    address: &str,
    policy: ForwardPolicy,
    sink: Arc<RaiseSink>,
) -> zbus::Result<Connection> {
    connection::Builder::address(address)?
        .method_timeout(policy.timeout)
        .serve_at(OBJECT_PATH, Application { sink })?
        .build()
}

/// Ask the owner to raise its window: one try, one retry.
fn forward(conn: &Connection, policy: ForwardPolicy) -> Outcome {
    let result = match activate_owner(conn) {
        Ok(()) => Ok(()),
        Err(error) => {
            tracing::warn!(
                %error,
                "single instance: the running instance did not answer; retrying"
            );
            std::thread::sleep(policy.retry_delay);
            activate_owner(conn)
        }
    };
    match result {
        Ok(()) => Outcome::Forwarded,
        Err(error) => {
            tracing::error!(
                %error,
                "single instance: Snitchwatch is already running but did not answer; \
                 starting a second instance"
            );
            Outcome::Skipped(SkipReason::OwnerUnresponsive)
        }
    }
}

fn activate_owner(conn: &Connection) -> zbus::Result<()> {
    let platform_data: HashMap<&str, Value<'_>> = HashMap::new();
    conn.call_method(
        Some(BUS_NAME),
        OBJECT_PATH,
        Some(APPLICATION_IFACE),
        "Activate",
        &(platform_data,),
    )
    .map(|_| ())
}

fn log_outcome(outcome: &Outcome) {
    match outcome {
        Outcome::Primary(_) => tracing::info!("single instance: this is the running instance"),
        Outcome::Forwarded => {
            tracing::info!("single instance: raised the running instance; exiting")
        }
        Outcome::Skipped(reason) => tracing::info!(?reason, "single instance: guard skipped"),
    }
}

/// The served object. Every method only raises the window.
struct Application {
    sink: Arc<RaiseSink>,
}

#[zbus::interface(name = "org.freedesktop.Application")]
impl Application {
    fn activate(&self, _platform_data: HashMap<String, OwnedValue>) {
        self.sink.raise();
    }

    fn open(&self, _uris: Vec<String>, _platform_data: HashMap<String, OwnedValue>) {
        self.sink.raise();
    }

    fn activate_action(
        &self,
        _action_name: String,
        _parameter: Vec<OwnedValue>,
        _platform_data: HashMap<String, OwnedValue>,
    ) {
        self.sink.raise();
    }
}

#[cfg(test)]
#[path = "single_instance/tests.rs"]
mod tests;

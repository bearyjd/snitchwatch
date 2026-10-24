//! Session-activated unprivileged broker; the privileged scanner remains one-shot.
use scanner_broker::{Broker, BUS_NAME, OBJECT_PATH};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Never run the adapter with root's session or turn it into a root service.
    if unsafe { libc::geteuid() } == 0 {
        return Err("the scanner session broker must run unprivileged".into());
    }
    let broker = Broker::default();
    let _connection = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, broker.clone())?
        .build()
        .await?;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if broker.claim_idle_shutdown(Duration::from_secs(30)) {
            return Ok(());
        }
    }
}

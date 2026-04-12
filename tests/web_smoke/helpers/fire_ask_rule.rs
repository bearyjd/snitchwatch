//! Test helper: dial the bridge's gRPC server, fire one AskRule, print the
//! returned Rule action and exit. Used by the Playwright web smoke tests.
//!
//! Usage:
//!   cargo run --quiet --manifest-path tests/web_smoke/helpers/Cargo.toml \
//!     -- --grpc 127.0.0.1:50321 --process /usr/bin/curl --host example.com --port 443

use clap::Parser;
use snitchwatch_proto::protocol::{ui_client::UiClient, Connection};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    grpc: String,
    #[arg(long)]
    process: String,
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 443)]
    port: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{}", args.grpc))?;
    let channel = endpoint.connect().await?;
    let mut client = UiClient::new(channel);

    let conn = Connection {
        protocol: "tcp".into(),
        dst_host: args.host.clone(),
        dst_ip: "0.0.0.0".into(),
        dst_port: args.port,
        process_path: args.process.clone(),
        ..Default::default()
    };
    let rule = client.ask_rule(conn).await?.into_inner();
    println!("rule.action={}", rule.action);
    Ok(())
}

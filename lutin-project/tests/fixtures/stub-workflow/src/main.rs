//! Minimal workflow binary used by `lutin-project` integration tests.
//!
//! Reads the spawn handoff env vars, binds a TCP listener, writes the
//! bound addr to the handoff path, then sleeps until the supervisor
//! kills the child via `kill_on_drop`. Real workflows write the
//! handoff atomically via `lutin_keypair::write_atomic`; this fixture
//! uses plain `tokio::fs::write` because atomicity isn't needed here.

use std::net::SocketAddr;

use anyhow::Context;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr: SocketAddr = std::env::var("LUTIN_WORKFLOW_ADDR")
        .context("LUTIN_WORKFLOW_ADDR not set")?
        .parse()
        .context("LUTIN_WORKFLOW_ADDR parse")?;
    let handoff_path =
        std::env::var("LUTIN_WORKFLOW_HANDOFF_PATH").context("LUTIN_WORKFLOW_HANDOFF_PATH not set")?;

    let listener = TcpListener::bind(addr).await.context("bind")?;
    let bound = listener.local_addr().context("local_addr")?;
    tokio::fs::write(&handoff_path, format!("{bound}\n"))
        .await
        .context("write handoff")?;

    std::future::pending::<()>().await;
    Ok(())
}

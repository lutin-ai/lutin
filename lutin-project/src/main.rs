use std::path::PathBuf;

use lutin_auth::{Slug, pubkey_from_str, pubkey_to_string};
use lutin_keypair::{load_or_create_keypair, write_atomic};
use lutin_project::workflows::load_workflows;
use lutin_project::{SpawnConfig, Supervisor, run};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let slug = Slug::parse(env_required("LUTIN_PROJECT_SLUG")?)?;
    let issuer = pubkey_from_str(&env_required("LUTIN_PROJECT_ISSUER_PUBKEY")?)?;

    let keypair_path: PathBuf = std::env::var("LUTIN_PROJECT_KEYPAIR_PATH")
        .unwrap_or_else(|_| "/data/keypair".into())
        .into();
    let signing = load_or_create_keypair(&keypair_path)?;
    let pubkey = pubkey_to_string(&signing.verifying_key());

    // Config dirs are forwarded to spawned workflow binaries; the
    // project supervisor itself never reads settings or personas.
    let global_config_dir: PathBuf = std::env::var("LUTIN_GLOBAL_CONFIG_DIR")
        .unwrap_or_else(|_| "/etc/lutin/.lutin".into())
        .into();
    let project_config_dir: PathBuf = std::env::var("LUTIN_PROJECT_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| global_config_dir.clone());

    // Workflows live under the project config dir by convention; the
    // explicit env override is kept for tests + ad-hoc deployments.
    let workflows_dir: PathBuf = std::env::var("LUTIN_PROJECT_WORKFLOWS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| project_config_dir.join("workflows"));
    let workflows = load_workflows(&workflows_dir)?;
    info!(count = workflows.len(), dir = %workflows_dir.display(), "workflows loaded");

    let sup = Supervisor::spawn(
        slug,
        issuer,
        signing,
        SpawnConfig {
            workflows,
            global_config_dir,
            project_config_dir,
        },
    );
    let state = sup.state.clone();
    let addr = std::env::var("LUTIN_PROJECT_ADDR").unwrap_or_else(|_| "0.0.0.0:7879".into());
    let listener = TcpListener::bind(&addr).await?;
    let bound = listener.local_addr()?;

    // Atomic single-file handoff: pubkey + bound addr in ONE file the
    // parent (control-panel) reads as a unit. Two-line format: pubkey
    // on line 1, addr on line 2. write_atomic uses rename, so readers
    // see all-or-nothing; no torn reads. Optional — bare `cargo run`
    // with a fixed addr skips it.
    if let Ok(handoff_path) = std::env::var("LUTIN_PROJECT_HANDOFF_PATH") {
        let body = format!("{pubkey}\n{bound}\n");
        write_atomic(
            std::path::Path::new(&handoff_path),
            body.as_bytes(),
            0o644,
        )?;
    }

    info!(%bound, slug = %state.slug, "project listening");
    run(listener, state).await
}

fn env_required(key: &str) -> anyhow::Result<String> {
    std::env::var(key).map_err(|_| anyhow::anyhow!("missing required env var {key}"))
}

use std::path::PathBuf;

use lutin_control_panel::{SpawnBackend, SpawnConfig, Supervisor, defaults, run, workflow_images};
use lutin_keypair::load_or_create_keypair;
use tokio::net::TcpListener;
use tokio::signal;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load `.env` from CWD if present (developer convenience). Existing
    // process env wins — operators can override any value at the shell
    // without editing the file. Missing file is not an error: production
    // deployments inject env via the orchestrator, not a dotfile.
    match dotenvy::dotenv() {
        Ok(path) => eprintln!("loaded env from {}", path.display()),
        Err(e) if e.not_found() => {}
        Err(e) => eprintln!("warning: failed to load .env: {e}"),
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let data_dir: PathBuf = std::env::var("LUTIN_CP_DATA_DIR")
        .unwrap_or_else(|_| "/var/lib/lutin/control-panel".into())
        .into();
    let keypair_path = data_dir.join("keypair");
    let signing = load_or_create_keypair(&keypair_path)?;

    // `LUTIN_CONFIG_ROOT` parents the global `<root>/lutin/.lutin/`
    // tree (matches `lutin_storage::layout`). `LUTIN_GLOBAL_CONFIG_DIR`
    // skips the layout helper for ad-hoc deployments.
    let config_root: PathBuf = std::env::var("LUTIN_CONFIG_ROOT")
        .unwrap_or_else(|_| "/etc/lutin".into())
        .into();
    let global_config_dir: PathBuf = std::env::var("LUTIN_GLOBAL_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| lutin_storage::layout::global_config(&config_root));

    // Seed the in-tree default chat workflow + assistant persona into
    // the global config tier on every launch (idempotent — markers
    // protect existing user files). Workflow engines never write to
    // global, so this path is the sole writer.
    defaults::seed(&global_config_dir)?;

    // Materialize workflow cdylibs from installed Docker images (new
    // post-refactor source of truth). Logs and continues on failure —
    // the legacy seed path above still produces source the project
    // tier can build, so docker-less subprocess deployments still work.
    let installed = workflow_images::install_all(&global_config_dir);
    info!(count = installed.len(), "workflow cdylibs materialized from images");

    let backend = backend_from_env()?;
    let config = SpawnConfig {
        backend,
        // Single root parents every per-project tree. Each project
        // owns `<projects_root>/<slug>/` (user files + `.lutin/`), and
        // the registry sits at `<projects_root>/projects.toml`. The
        // default is CWD-relative so a developer running CP from the
        // workspace root drops state into `./projects/` next to their
        // checkout, with no global filesystem layout to set up first.
        projects_root: std::env::var("LUTIN_CP_PROJECTS_ROOT")
            .unwrap_or_else(|_| "./projects".into())
            .into(),
        global_config_dir,
    };

    let sup = Supervisor::spawn(signing, config);
    let state = sup.state.clone();
    let addr = std::env::var("LUTIN_CP_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
    let listener = TcpListener::bind(&addr).await?;
    let bound = listener.local_addr()?;
    info!(%bound, "control-panel listening");

    // Drive Supervisor::shutdown on SIGINT/SIGTERM so project
    // containers (which the Docker daemon owns, not us) get stopped
    // cleanly. The `run` loop itself never returns voluntarily, so we
    // race it against the signal future.
    tokio::select! {
        res = run(listener, state) => {
            sup.shutdown().await;
            res
        }
        _ = shutdown_signal() => {
            info!("shutdown signal received, stopping projects");
            sup.shutdown().await;
            Ok(())
        }
    }
}

fn backend_from_env() -> anyhow::Result<SpawnBackend> {
    let kind = std::env::var("LUTIN_CP_SPAWN_BACKEND").unwrap_or_else(|_| "docker".into());
    match kind.as_str() {
        "docker" => {
            let image = std::env::var("LUTIN_CP_PROJECT_IMAGE")
                .unwrap_or_else(|_| format!("lutin-project:{}", env!("CARGO_PKG_VERSION")));
            if image.is_empty() {
                anyhow::bail!("LUTIN_CP_PROJECT_IMAGE must not be empty");
            }
            let container_prefix = std::env::var("LUTIN_CP_CONTAINER_PREFIX")
                .unwrap_or_else(|_| "lutin-project".into());
            if container_prefix.is_empty() {
                anyhow::bail!("LUTIN_CP_CONTAINER_PREFIX must not be empty");
            }
            Ok(SpawnBackend::Docker {
                image,
                container_prefix,
            })
        }
        "subprocess" => {
            let binary = std::env::var("LUTIN_PROJECT_BINARY")
                .unwrap_or_else(|_| "/usr/local/bin/lutin-project".into())
                .into();
            Ok(SpawnBackend::Subprocess { binary })
        }
        other => anyhow::bail!(
            "LUTIN_CP_SPAWN_BACKEND={other:?}, expected \"docker\" or \"subprocess\""
        ),
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            warn!(error = %e, "failed to install SIGINT handler");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let term = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                warn!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}

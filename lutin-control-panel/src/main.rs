use std::path::PathBuf;

use lutin_control_panel::{SpawnConfig, Supervisor, defaults, run, workflow_images};
use lutin_keypair::load_or_create_keypair;
use tokio::net::TcpListener;
use tokio::signal;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    let config_root: PathBuf = std::env::var("LUTIN_CONFIG_ROOT")
        .unwrap_or_else(|_| "/etc/lutin".into())
        .into();
    let global_config_dir: PathBuf = std::env::var("LUTIN_GLOBAL_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| lutin_storage::layout::global_config(&config_root));

    defaults::seed(&global_config_dir)?;

    let installed = workflow_images::install_all(&global_config_dir);
    info!(count = installed.len(), "workflow cdylibs materialized from images");

    let config = SpawnConfig {
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

    tokio::select! {
        res = run(listener, state) => {
            sup.shutdown().await;
            res
        }
        _ = shutdown_signal() => {
            info!("shutdown signal received, stopping sessions");
            sup.shutdown().await;
            Ok(())
        }
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

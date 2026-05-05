use std::path::PathBuf;

use lutin_desktop::{App, DesktopSettings, WorkflowCache};
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    // Connection config comes from desktop-local settings only
    // (`~/.config/lutin/desktop.json`). When the active connection is
    // missing, malformed, or has an empty token we still launch — the
    // chrome opens in the Settings view so the user can configure one
    // and apply without restarting.
    let settings = DesktopSettings::load();

    // Workflow cdylib lookup mirrors the control-panel's env vars so
    // both agree on where the materialized workflow tree lives. The CP
    // extracts each workflow image's cdylib to
    // `<global>/workflows/<id>/lib<id>.so` (see workflow_images.rs);
    // the desktop dlopens from the same location.
    let config_root: PathBuf = std::env::var("LUTIN_CONFIG_ROOT")
        .unwrap_or_else(|_| "/etc/lutin".into())
        .into();
    let global_config_dir: PathBuf = std::env::var("LUTIN_GLOBAL_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| lutin_storage::layout::global_config(&config_root));
    let workflow_cache = WorkflowCache::new(global_config_dir.join("workflows"));

    // Multi-thread runtime: chrome runs egui on the main thread, the
    // tokio runtime drives the WS pump on its own threads. We hand a
    // `Handle` to the App, which spawns (and re-spawns on settings
    // save) the cp worker itself.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let handle = rt.handle().clone();

    // Keep the runtime alive for the duration of run_native; dropped on return.
    let _runtime = rt;

    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("lutin"),
        ..Default::default()
    };

    eframe::run_native(
        "lutin",
        opts,
        Box::new(move |cc| Ok(Box::new(App::new(cc, handle, workflow_cache, settings)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}

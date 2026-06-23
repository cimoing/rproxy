use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use rproxy_core::AppService;
use slint::{ComponentHandle, SharedString};
use tokio::runtime::Runtime;
use tracing_subscriber::EnvFilter;

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let runtime = Arc::new(Runtime::new().context("failed to create tokio runtime")?);
    let service = AppService::new();
    let ui = MainWindow::new().context("failed to create main window")?;

    ui.set_config_path(default_config_path().to_string_lossy().to_string().into());
    ui.set_status("Ready".into());

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_load_config(move |path| {
            let service = service.clone();
            let ui_weak = ui_weak.clone();
            runtime.spawn(async move {
                let status = match service.load_config(path.as_str()).await {
                    Ok(status) => status.message,
                    Err(error) => format!("Config error: {error}"),
                };
                update_status(ui_weak, status);
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_start_proxy(move || {
            let service = service.clone();
            let ui_weak = ui_weak.clone();
            runtime.spawn(async move {
                let status = match service.start().await {
                    Ok(status) => status.message,
                    Err(error) => format!("Start error: {error}"),
                };
                update_status(ui_weak, status);
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_stop_proxy(move || {
            let service = service.clone();
            let ui_weak = ui_weak.clone();
            runtime.spawn(async move {
                let status = service.stop().await;
                update_status(ui_weak, status.message);
            });
        });
    }

    ui.on_open_pac(move |url| {
        let _ = webbrowser::open(url.as_str());
    });

    ui.run().context("failed to run UI")
}

fn default_config_path() -> PathBuf {
    PathBuf::from("examples/default.yaml")
}

fn update_status(ui_weak: slint::Weak<MainWindow>, status: String) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_status(SharedString::from(status));
        }
    });
}

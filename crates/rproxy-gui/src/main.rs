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
    ui.set_running(false);

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_load_config(move |path| {
            let service = service.clone();
            let ui_weak = ui_weak.clone();
            runtime.spawn(async move {
                let status = match service.load_config(path.as_str()).await {
                    Ok(status) => UiStatus::from(status),
                    Err(error) => UiStatus::message(format!("Config error: {error}")),
                };
                update_status(ui_weak, status);
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_toggle_proxy(move || {
            let service = service.clone();
            let ui_weak = ui_weak.clone();
            runtime.spawn(async move {
                let is_running = service.status().await.running;
                let status = if is_running {
                    UiStatus::from(service.stop().await)
                } else {
                    match service.start().await {
                        Ok(status) => UiStatus::from(status),
                        Err(error) => UiStatus::message(format!("Start error: {error}")),
                    }
                };
                update_status(ui_weak, status);
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

struct UiStatus {
    message: String,
    running: Option<bool>,
}

impl UiStatus {
    fn message(message: String) -> Self {
        Self {
            message,
            running: None,
        }
    }
}

impl From<rproxy_core::AppStatus> for UiStatus {
    fn from(status: rproxy_core::AppStatus) -> Self {
        Self {
            message: status.message,
            running: Some(status.running),
        }
    }
}

fn update_status(ui_weak: slint::Weak<MainWindow>, status: UiStatus) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_status(SharedString::from(status.message));
            if let Some(running) = status.running {
                ui.set_running(running);
            }
        }
    });
}

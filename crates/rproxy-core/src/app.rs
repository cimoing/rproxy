use std::{path::Path, sync::Arc};

use tokio::sync::Mutex;

use crate::{
    config::{Config, ConfigError},
    platform::{Autostart, SystemProxy},
    proxy::{ProxyRuntime, RuntimeError, RuntimeStatus},
};

#[derive(Debug, Clone)]
pub struct AppStatus {
    pub running: bool,
    pub message: String,
    pub runtime: Option<RuntimeStatus>,
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("no config loaded")]
    NoConfig,
}

#[derive(Clone)]
pub struct AppService {
    inner: Arc<Mutex<AppState>>,
}

struct AppState {
    config: Option<Config>,
    runtime: Option<ProxyRuntime>,
    status: AppStatus,
}

impl AppService {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AppState {
                config: None,
                runtime: None,
                status: AppStatus {
                    running: false,
                    message: "Ready".into(),
                    runtime: None,
                },
            })),
        }
    }

    pub async fn load_config(&self, path: impl AsRef<Path>) -> Result<AppStatus, AppError> {
        let config = Config::load(path)?;
        config.validate()?;
        let mut state = self.inner.lock().await;
        state.status.message = format!("Loaded profile {}", config.profile.name);
        state.config = Some(config);
        Ok(state.status.clone())
    }

    pub async fn start(&self) -> Result<AppStatus, AppError> {
        let mut state = self.inner.lock().await;
        let config = state.config.clone().ok_or(AppError::NoConfig)?;
        let mut runtime = ProxyRuntime::new(config.clone());
        runtime.start().await?;

        if config.pac.enabled {
            let pac_url = format!("http://{}/proxy.pac", config.pac.listen);
            let _ = SystemProxy::enable_pac(&pac_url);
        } else {
            let _ = SystemProxy::enable_http(config.proxy.http_listen);
        }

        let _ = Autostart::set_enabled(config.system.auto_start);

        let runtime_status = runtime.status();
        state.status = AppStatus {
            running: true,
            message: "Proxy started".into(),
            runtime: Some(runtime_status),
        };
        state.runtime = Some(runtime);
        Ok(state.status.clone())
    }

    pub async fn stop(&self) -> AppStatus {
        let mut state = self.inner.lock().await;
        if let Some(mut runtime) = state.runtime.take() {
            runtime.stop().await;
        }
        let _ = SystemProxy::disable();
        state.status = AppStatus {
            running: false,
            message: "Proxy stopped".into(),
            runtime: None,
        };
        state.status.clone()
    }

    pub async fn status(&self) -> AppStatus {
        self.inner.lock().await.status.clone()
    }
}

impl Default for AppService {
    fn default() -> Self {
        Self::new()
    }
}

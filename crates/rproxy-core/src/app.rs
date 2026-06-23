use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::Mutex;

use crate::{
    config::{Config, ConfigError, NodeConfig},
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
    config_path: Option<PathBuf>,
    runtime: Option<ProxyRuntime>,
    status: AppStatus,
}

impl AppService {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AppState {
                config: None,
                config_path: None,
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
        let path = path.as_ref().to_path_buf();
        let config = Config::load(&path)?;
        self.set_config(config, Some(path)).await
    }

    pub async fn load_or_create_config(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<AppStatus, AppError> {
        let config = Config::load_or_create(path.as_ref())?;
        self.set_config(config, Some(path.as_ref().to_path_buf()))
            .await
    }

    async fn set_config(
        &self,
        config: Config,
        config_path: Option<PathBuf>,
    ) -> Result<AppStatus, AppError> {
        config.validate()?;
        let mut state = self.inner.lock().await;
        state.status.message = format!("Loaded profile {}", config.profile.name);
        state.config_path = config_path;
        state.config = Some(config);
        Ok(state.status.clone())
    }

    pub async fn nodes(&self) -> Vec<NodeConfig> {
        self.inner
            .lock()
            .await
            .config
            .as_ref()
            .map(|config| config.nodes.clone())
            .unwrap_or_default()
    }

    pub async fn save_node(
        &self,
        index: Option<usize>,
        node: NodeConfig,
    ) -> Result<AppStatus, AppError> {
        let mut state = self.inner.lock().await;
        let mut config = state.config.clone().ok_or(AppError::NoConfig)?;
        match index {
            Some(index) if index < config.nodes.len() => {
                let mut node = node;
                node.id = config.nodes[index].id.clone();
                config.nodes[index] = node;
            }
            Some(_) | None => config.nodes.push(node),
        }
        config.validate()?;

        if let Some(path) = &state.config_path {
            config.save(path)?;
        }

        state.status.message = "Node saved".into();
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

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::Mutex;

use crate::{
    config::{Config, ConfigError, NodeConfig},
    platform::{Autostart, SystemProxy, SystemProxySnapshot},
    proxy::{ProxyRuntime, RuntimeError, RuntimeStatus},
};

#[derive(Debug, Clone)]
pub struct AppStatus {
    pub running: bool,
    pub message: String,
    pub runtime: Option<RuntimeStatus>,
}

#[derive(Debug, Clone)]
pub struct AppSettings {
    pub pac_enabled: bool,
    pub auto_start: bool,
    pub http_listen: SocketAddr,
    pub socks_listen: SocketAddr,
    pub pac_listen: SocketAddr,
}

#[derive(Debug, Clone)]
pub struct NodeEntry {
    pub node: NodeConfig,
    pub active: bool,
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
    system_proxy_snapshot: Option<SystemProxySnapshot>,
    status: AppStatus,
}

impl AppService {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AppState {
                config: None,
                config_path: None,
                runtime: None,
                system_proxy_snapshot: None,
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

    pub async fn nodes(&self) -> Vec<NodeEntry> {
        self.inner
            .lock()
            .await
            .config
            .as_ref()
            .map(|config| {
                let active_id = config.active_node().map(|node| node.id.as_str());
                config
                    .nodes
                    .iter()
                    .cloned()
                    .map(|node| {
                        let active = Some(node.id.as_str()) == active_id;
                        NodeEntry { node, active }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn settings(&self) -> Option<AppSettings> {
        self.inner
            .lock()
            .await
            .config
            .as_ref()
            .map(|config| AppSettings {
                pac_enabled: config.pac.enabled,
                auto_start: config.system.auto_start,
                http_listen: config.proxy.http_listen,
                socks_listen: config.proxy.socks_listen,
                pac_listen: config.pac.listen,
            })
    }

    pub async fn save_settings(
        &self,
        pac_enabled: bool,
        auto_start: bool,
        http_listen: SocketAddr,
        socks_listen: SocketAddr,
        pac_listen: SocketAddr,
    ) -> Result<AppStatus, AppError> {
        let mut state = self.inner.lock().await;
        let mut config = state.config.clone().ok_or(AppError::NoConfig)?;
        config.pac.enabled = pac_enabled;
        config.system.auto_start = auto_start;
        config.proxy.http_listen = http_listen;
        config.proxy.socks_listen = socks_listen;
        config.pac.listen = pac_listen;
        config.validate()?;

        if let Some(path) = &state.config_path {
            config.save(path)?;
        }

        let _ = Autostart::set_enabled(config.system.auto_start);
        state.config = Some(config);
        state.status.message = if state.status.running {
            "Settings saved; restart proxy to apply PAC changes".into()
        } else {
            "Settings saved".into()
        };
        Ok(state.status.clone())
    }

    pub async fn set_active_node(&self, index: usize) -> Result<AppStatus, AppError> {
        let mut state = self.inner.lock().await;
        let mut config = state.config.clone().ok_or(AppError::NoConfig)?;
        let node =
            config.nodes.get(index).cloned().ok_or_else(|| {
                ConfigError::Validation(format!("node index {index} does not exist"))
            })?;
        config.profile.active_node = Some(node.id.clone());

        if let Some(path) = &state.config_path {
            config.save(path)?;
        }

        state.config = Some(config);
        state.status.message = if state.status.running {
            format!("Active node set to {}; restart proxy to apply", node.name)
        } else {
            format!("Active node set to {}", node.name)
        };
        Ok(state.status.clone())
    }

    pub async fn delete_node(&self, index: usize) -> Result<AppStatus, AppError> {
        let mut state = self.inner.lock().await;
        let mut config = state.config.clone().ok_or(AppError::NoConfig)?;
        if index >= config.nodes.len() {
            return Err(
                ConfigError::Validation(format!("node index {index} does not exist")).into(),
            );
        }

        let removed = config.nodes.remove(index);
        if config.profile.active_node.as_deref() == Some(removed.id.as_str()) {
            config.profile.active_node = config.nodes.first().map(|node| node.id.clone());
        }
        config.validate()?;

        if let Some(path) = &state.config_path {
            config.save(path)?;
        }

        state.config = Some(config);
        state.status.message = if state.status.running {
            format!("Node {} deleted; restart proxy to apply", removed.name)
        } else {
            format!("Node {} deleted", removed.name)
        };
        Ok(state.status.clone())
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

        state.system_proxy_snapshot = SystemProxy::snapshot().ok();
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
        if let Some(snapshot) = state.system_proxy_snapshot.take() {
            let _ = SystemProxy::restore(&snapshot);
        } else {
            let _ = SystemProxy::disable();
        }
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

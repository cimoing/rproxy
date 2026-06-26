use std::{collections::HashSet, fs, net::SocketAddr, path::Path};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    Read(#[from] std::io::Error),
    #[error("failed to process yaml config: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("config validation failed: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub profile: ProfileConfig,
    #[serde(default)]
    pub nodes: Vec<NodeConfig>,
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub system: SystemConfig,
    #[serde(default)]
    pub tun: TunConfig,
    #[serde(default)]
    pub pac: PacConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub routing_profiles: Vec<RouteProfileConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub active_node: Option<String>,
    pub active_route: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub server: String,
    pub port: u16,
    #[serde(default)]
    pub options: NodeOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http,
    Socks,
    Vmess,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeOptions {
    pub username: Option<String>,
    pub password: Option<String>,
    pub uuid: Option<String>,
    pub alter_id: Option<u16>,
    pub security: Option<String>,
    pub request_host: Option<String>,
    #[serde(default)]
    pub tls: bool,
    pub transport: Option<Transport>,
    pub websocket: Option<WebSocketOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Tcp,
    WebSocket,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketOptions {
    #[serde(default = "default_ws_path")]
    pub path: String,
    pub host: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub http_listen: SocketAddr,
    pub socks_listen: SocketAddr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemConfig {
    #[serde(default = "default_true")]
    pub tray: bool,
    #[serde(default)]
    pub auto_start: bool,
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            tray: true,
            auto_start: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_tun_name")]
    pub interface_name: String,
    #[serde(default = "default_true")]
    pub auto_route: bool,
}

impl Default for TunConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interface_name: default_tun_name(),
            auto_route: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_pac_listen")]
    pub listen: SocketAddr,
}

impl Default for PacConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: default_pac_listen(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub mode: RoutingMode,
    #[serde(default = "default_proxy_action")]
    pub default_action: RouteAction,
    #[serde(default)]
    pub geosite: GeositeConfig,
    #[serde(default)]
    pub rules: Vec<RouteRule>,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            mode: RoutingMode::Auto,
            default_action: RouteAction::Proxy,
            geosite: GeositeConfig::default(),
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteProfileConfig {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub routing: RoutingConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    GlobalProxy,
    GlobalDirect,
    #[default]
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeositeConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub auto_update: bool,
    pub path: Option<String>,
}

impl Default for GeositeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_update: false,
            path: Some("data/dlc.dat".into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRule {
    #[serde(rename = "type")]
    pub kind: RouteRuleType,
    pub value: String,
    pub action: RouteAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteRuleType {
    Domain,
    DomainSuffix,
    IpCidr,
    Port,
    Geosite,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RouteAction {
    Proxy,
    Direct,
    Block,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path)?;
        let mut config: Self = serde_yaml::from_str(&text)?;
        config.normalize_routes();
        config.validate()?;
        Ok(config)
    }

    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let config = Self::default_user_config();
            config.save(path)?;
            return Ok(config);
        }

        Self::load(path)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_yaml::to_string(self)?;
        fs::write(path, text)?;
        Ok(())
    }

    pub fn default_user_config() -> Self {
        Self {
            profile: ProfileConfig {
                id: "default".into(),
                name: "Default".into(),
                enabled: true,
                active_node: None,
                active_route: Some("bypass-lan".into()),
            },
            nodes: Vec::new(),
            proxy: ProxyConfig {
                http_listen: "127.0.0.1:7890".parse().expect("valid default HTTP listen"),
                socks_listen: "127.0.0.1:7891"
                    .parse()
                    .expect("valid default SOCKS listen"),
            },
            system: SystemConfig::default(),
            tun: TunConfig::default(),
            pac: PacConfig::default(),
            routing: bypass_lan_routing(),
            routing_profiles: default_route_profiles(),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.profile.id.trim().is_empty() {
            return Err(ConfigError::Validation("profile.id is required".into()));
        }

        let mut node_ids = HashSet::new();
        for node in &self.nodes {
            if node.id.trim().is_empty() {
                return Err(ConfigError::Validation("node id is required".into()));
            }
            if !node_ids.insert(node.id.as_str()) {
                return Err(ConfigError::Validation(format!(
                    "node {} id is duplicated",
                    node.id
                )));
            }
            if node.server.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "node {} server is required",
                    node.id
                )));
            }

            if matches!(node.protocol, Protocol::Vmess) {
                if node
                    .options
                    .uuid
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    return Err(ConfigError::Validation(format!(
                        "{:?} node {} requires uuid",
                        node.protocol, node.id
                    )));
                }
            }

            if matches!(node.protocol, Protocol::Vmess) {
                if node.options.transport == Some(Transport::WebSocket)
                    && node.options.websocket.is_none()
                {
                    return Err(ConfigError::Validation(format!(
                        "{:?} websocket node {} requires websocket options",
                        node.protocol, node.id
                    )));
                }
            }
        }

        if let Some(active_node) = self.profile.active_node.as_deref() {
            if !self.nodes.iter().any(|node| node.id == active_node) {
                return Err(ConfigError::Validation(format!(
                    "profile.active_node {active_node} does not exist"
                )));
            }
        }

        let mut route_ids = HashSet::new();
        for route in &self.routing_profiles {
            if route.id.trim().is_empty() {
                return Err(ConfigError::Validation("route id is required".into()));
            }
            if !route_ids.insert(route.id.as_str()) {
                return Err(ConfigError::Validation(format!(
                    "route {} id is duplicated",
                    route.id
                )));
            }
            if route.name.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "route {} name is required",
                    route.id
                )));
            }
        }

        if let Some(active_route) = self.profile.active_route.as_deref() {
            if !self
                .routing_profiles
                .iter()
                .any(|route| route.id == active_route)
            {
                return Err(ConfigError::Validation(format!(
                    "profile.active_route {active_route} does not exist"
                )));
            }
        }

        if self.tun.enabled && self.tun.interface_name.trim().is_empty() {
            return Err(ConfigError::Validation(
                "tun.interface_name is required when Tun is enabled".into(),
            ));
        }

        Ok(())
    }

    pub fn active_node(&self) -> Option<&NodeConfig> {
        self.profile
            .active_node
            .as_deref()
            .and_then(|id| self.nodes.iter().find(|node| node.id == id))
            .or_else(|| self.nodes.first())
    }

    pub fn active_route(&self) -> Option<&RouteProfileConfig> {
        self.profile
            .active_route
            .as_deref()
            .and_then(|id| self.routing_profiles.iter().find(|route| route.id == id))
            .or_else(|| self.routing_profiles.first())
    }

    pub fn active_routing(&self) -> &RoutingConfig {
        self.active_route()
            .map(|route| &route.routing)
            .unwrap_or(&self.routing)
    }

    pub fn normalize_routes(&mut self) {
        if self.routing_profiles.is_empty() {
            let legacy_route = RouteProfileConfig {
                id: "current-route".into(),
                name: "当前路由".into(),
                routing: self.routing.clone(),
            };
            self.routing_profiles = std::iter::once(legacy_route)
                .chain(default_route_profiles())
                .collect();
            self.profile.active_route = Some("current-route".into());
        }

        if self.profile.active_route.is_none() {
            self.profile.active_route = self.routing_profiles.first().map(|route| route.id.clone());
        }

        if let Some(route) = self.active_route() {
            self.routing = route.routing.clone();
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_proxy_action() -> RouteAction {
    RouteAction::Proxy
}

fn default_tun_name() -> String {
    "rproxy-tun".into()
}

fn default_pac_listen() -> SocketAddr {
    "127.0.0.1:7892".parse().expect("valid default PAC address")
}

fn default_ws_path() -> String {
    "/".into()
}

pub fn default_route_profiles() -> Vec<RouteProfileConfig> {
    vec![
        RouteProfileConfig {
            id: "bypass-lan".into(),
            name: "绕过局域网".into(),
            routing: bypass_lan_routing(),
        },
        RouteProfileConfig {
            id: "global-proxy".into(),
            name: "全局".into(),
            routing: RoutingConfig {
                mode: RoutingMode::GlobalProxy,
                default_action: RouteAction::Proxy,
                geosite: GeositeConfig::default(),
                rules: Vec::new(),
            },
        },
        RouteProfileConfig {
            id: "bypass-cn-lan".into(),
            name: "绕过大陆及局域网".into(),
            routing: RoutingConfig {
                mode: RoutingMode::Auto,
                default_action: RouteAction::Proxy,
                geosite: GeositeConfig::default(),
                rules: vec![RouteRule {
                    kind: RouteRuleType::Geosite,
                    value: "cn".into(),
                    action: RouteAction::Direct,
                }],
            },
        },
    ]
}

fn bypass_lan_routing() -> RoutingConfig {
    RoutingConfig {
        mode: RoutingMode::Auto,
        default_action: RouteAction::Proxy,
        geosite: GeositeConfig::default(),
        rules: private_network_rules(),
    }
}

fn private_network_rules() -> Vec<RouteRule> {
    [
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "127.0.0.0/8",
        "169.254.0.0/16",
    ]
    .into_iter()
    .map(|value| RouteRule {
        kind: RouteRuleType::IpCidr,
        value: value.into(),
        action: RouteAction::Direct,
    })
    .collect()
}

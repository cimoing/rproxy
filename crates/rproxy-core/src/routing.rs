use std::net::IpAddr;

use crate::config::{Config, RouteAction, RouteRule, RouteRuleType, RoutingMode};

#[derive(Debug, Clone)]
pub struct Router {
    mode: RoutingMode,
    default_action: RouteAction,
    rules: Vec<RouteRule>,
    geosite_cn: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RouteDecision {
    pub action: RouteAction,
    pub reason: String,
}

impl Router {
    pub fn from_config(config: &Config) -> Self {
        Self {
            mode: config.routing.mode.clone(),
            default_action: config.routing.default_action,
            rules: config.routing.rules.clone(),
            geosite_cn: load_builtin_geosite_cn(),
        }
    }

    pub fn decide_host(&self, host: &str) -> RouteDecision {
        match self.mode {
            RoutingMode::GlobalProxy => {
                return RouteDecision::new(RouteAction::Proxy, "global_proxy");
            }
            RoutingMode::GlobalDirect => {
                return RouteDecision::new(RouteAction::Direct, "global_direct");
            }
            RoutingMode::Auto => {}
        }

        let normalized = host.trim().trim_end_matches('.').to_ascii_lowercase();

        for rule in &self.rules {
            if self.matches_rule(&normalized, rule) {
                return RouteDecision::new(
                    rule.action,
                    format!("rule:{:?}:{}", rule.kind, rule.value),
                );
            }
        }

        if self
            .geosite_cn
            .iter()
            .any(|suffix| domain_suffix_matches(&normalized, suffix))
        {
            return RouteDecision::new(RouteAction::Direct, "geosite:cn");
        }

        RouteDecision::new(self.default_action, "default")
    }

    pub fn decide_ip(&self, ip: IpAddr) -> RouteDecision {
        if is_private_or_loopback(ip) {
            return RouteDecision::new(RouteAction::Direct, "private_ip");
        }

        RouteDecision::new(self.default_action, "default")
    }

    fn matches_rule(&self, host: &str, rule: &RouteRule) -> bool {
        match rule.kind {
            RouteRuleType::Domain => host == rule.value.to_ascii_lowercase(),
            RouteRuleType::DomainSuffix => {
                domain_suffix_matches(host, &rule.value.to_ascii_lowercase())
            }
            RouteRuleType::Geosite => {
                rule.value.eq_ignore_ascii_case("cn")
                    && self
                        .geosite_cn
                        .iter()
                        .any(|suffix| domain_suffix_matches(host, suffix))
            }
            RouteRuleType::IpCidr | RouteRuleType::Port => false,
        }
    }
}

impl RouteDecision {
    fn new(action: RouteAction, reason: impl Into<String>) -> Self {
        Self {
            action,
            reason: reason.into(),
        }
    }
}

fn domain_suffix_matches(host: &str, suffix: &str) -> bool {
    host == suffix || host.ends_with(&format!(".{suffix}"))
}

fn is_private_or_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
    }
}

fn load_builtin_geosite_cn() -> Vec<String> {
    include_str!("../data/geosite-cn.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_ascii_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        GeositeConfig, PacConfig, ProfileConfig, ProxyConfig, RoutingConfig, SystemConfig,
        TunConfig,
    };

    #[test]
    fn matches_builtin_cn_domains() {
        let config = Config {
            profile: ProfileConfig {
                id: "default".into(),
                name: "Default".into(),
                enabled: true,
            },
            nodes: vec![],
            proxy: ProxyConfig {
                http_listen: "127.0.0.1:7890".parse().unwrap(),
                socks_listen: "127.0.0.1:7891".parse().unwrap(),
            },
            system: SystemConfig::default(),
            tun: TunConfig::default(),
            pac: PacConfig::default(),
            routing: RoutingConfig {
                mode: RoutingMode::Auto,
                default_action: RouteAction::Proxy,
                geosite: GeositeConfig::default(),
                rules: vec![],
            },
        };

        let router = Router::from_config(&config);
        assert_eq!(
            router.decide_host("www.baidu.com").action,
            RouteAction::Direct
        );
        assert_eq!(router.decide_host("example.org").action, RouteAction::Proxy);
    }
}

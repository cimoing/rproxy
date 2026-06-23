mod geosite;

use std::collections::HashMap;
use std::net::IpAddr;

use crate::config::{Config, RouteAction, RouteRule, RouteRuleType, RoutingMode};

const MAX_PAC_GEOSITE_EXPANSION: usize = 2048;

#[derive(Debug, Clone)]
pub struct Router {
    mode: RoutingMode,
    default_action: RouteAction,
    rules: Vec<RouteRule>,
    geosite_cn: Vec<geosite::GeositeMatcher>,
    geosite_rules: HashMap<String, Vec<geosite::GeositeMatcher>>,
}

#[derive(Debug, Clone)]
pub struct RouteDecision {
    pub action: RouteAction,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct PacRule {
    pub action: RouteAction,
    pub condition: String,
}

impl Router {
    pub fn from_config(config: &Config) -> Self {
        Self {
            mode: config.routing.mode.clone(),
            default_action: config.routing.default_action,
            rules: config.routing.rules.clone(),
            geosite_cn: if config.routing.geosite.enabled {
                geosite::load_cn_with_fallback(config.routing.geosite.path.as_deref())
            } else {
                Vec::new()
            },
            geosite_rules: if config.routing.geosite.enabled {
                geosite::load_categories(
                    config.routing.geosite.path.as_deref(),
                    config
                        .routing
                        .rules
                        .iter()
                        .filter(|rule| rule.kind == RouteRuleType::Geosite)
                        .map(|rule| rule.value.clone()),
                )
            } else {
                HashMap::new()
            },
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
            .any(|matcher| geosite::matches(&normalized, matcher))
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
                let category = geosite::normalize_category(&rule.value);
                let matchers = if category == "CN" {
                    Some(&self.geosite_cn)
                } else {
                    self.geosite_rules.get(&category)
                };
                matchers
                    .into_iter()
                    .flatten()
                    .any(|matcher| geosite::matches(host, matcher))
            }
            RouteRuleType::IpCidr | RouteRuleType::Port => false,
        }
    }

    pub fn pac_rules(&self) -> Vec<PacRule> {
        if self.mode != RoutingMode::Auto {
            return Vec::new();
        }

        let mut rules = Vec::new();

        for rule in &self.rules {
            rules.extend(self.route_rule_to_pac(rule));
        }

        rules
    }

    pub fn default_action(&self) -> RouteAction {
        match self.mode {
            RoutingMode::GlobalProxy => RouteAction::Proxy,
            RoutingMode::GlobalDirect => RouteAction::Direct,
            RoutingMode::Auto => self.default_action,
        }
    }

    fn route_rule_to_pac(&self, rule: &RouteRule) -> Vec<PacRule> {
        let conditions = match rule.kind {
            RouteRuleType::Domain => vec![format!(
                r#"host == "{}""#,
                escape_js(&rule.value.to_ascii_lowercase())
            )],
            RouteRuleType::DomainSuffix => {
                let suffix = rule
                    .value
                    .trim()
                    .trim_start_matches('.')
                    .to_ascii_lowercase();
                vec![format!(
                    r#"(host == "{suffix}" || dnsDomainIs(host, ".{suffix}"))"#
                )]
            }
            RouteRuleType::IpCidr => cidr_to_pac_condition(&rule.value).into_iter().collect(),
            RouteRuleType::Port => port_to_pac_condition(&rule.value).into_iter().collect(),
            RouteRuleType::Geosite => {
                let category = geosite::normalize_category(&rule.value);
                let matchers = if category == "CN" {
                    Some(&self.geosite_cn)
                } else {
                    self.geosite_rules.get(&category)
                };
                matchers.map_or_else(Vec::new, |matchers| geosite_conditions(matchers))
            }
        };

        conditions
            .into_iter()
            .map(|condition| PacRule {
                action: rule.action,
                condition,
            })
            .collect()
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

fn geosite_conditions(matchers: &[geosite::GeositeMatcher]) -> Vec<String> {
    matchers
        .iter()
        .take(MAX_PAC_GEOSITE_EXPANSION)
        .filter_map(geosite::to_pac_expr)
        .collect()
}

fn cidr_to_pac_condition(value: &str) -> Option<String> {
    let (ip, prefix) = value.split_once('/')?;
    let prefix = prefix.parse::<u8>().ok()?;
    if prefix > 32 {
        return None;
    }
    let ip = ip.parse::<std::net::Ipv4Addr>().ok()?;
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    let network = u32::from(ip) & mask;
    Some(format!(
        r#"isInNet(dnsResolve(host), "{}", "{}")"#,
        std::net::Ipv4Addr::from(network),
        std::net::Ipv4Addr::from(mask)
    ))
}

fn port_to_pac_condition(value: &str) -> Option<String> {
    let port = value.parse::<u16>().ok()?;
    Some(format!(
        r#"/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\/[^\/:]+:{port}(\/|$)/.test(url)"#
    ))
}

fn escape_js(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
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
                active_node: None,
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

    #[test]
    fn geosite_google_rule_uses_loaded_category() {
        let router = Router {
            mode: RoutingMode::Auto,
            default_action: RouteAction::Direct,
            rules: vec![RouteRule {
                kind: RouteRuleType::Geosite,
                value: "google".into(),
                action: RouteAction::Proxy,
            }],
            geosite_cn: vec![],
            geosite_rules: HashMap::from([(
                "GOOGLE".into(),
                vec![geosite::GeositeMatcher::Domain {
                    value: "google.com".into(),
                    attrs: Vec::new(),
                }],
            )]),
        };

        assert_eq!(
            router.decide_host("www.google.com").action,
            RouteAction::Proxy
        );
        assert_eq!(router.decide_host("example.cn").action, RouteAction::Direct);
    }
}

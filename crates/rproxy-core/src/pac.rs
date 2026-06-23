use std::{net::SocketAddr, sync::Arc};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
};

use crate::{config::RouteAction, routing::Router};

#[derive(Debug)]
pub struct PacServer {
    listen: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl PacServer {
    pub async fn start(listen: SocketAddr, script: String) -> std::io::Result<Self> {
        let listener = TcpListener::bind(listen).await?;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let script = Arc::new(script);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let Ok((mut stream, _)) = accept else {
                            continue;
                        };
                        let script = Arc::clone(&script);
                        tokio::spawn(async move {
                            let mut buffer = [0_u8; 1024];
                            let _ = stream.read(&mut buffer).await;
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                script.len(),
                                script
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                            let _ = stream.shutdown().await;
                        });
                    }
                }
            }
        });

        Ok(Self {
            listen,
            shutdown: Some(shutdown_tx),
            task,
        })
    }

    pub fn listen(&self) -> SocketAddr {
        self.listen
    }

    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.task.await;
    }
}

pub fn generate_pac(router: &Router, proxy_addr: SocketAddr) -> String {
    let rule_lines = router
        .pac_rules()
        .into_iter()
        .map(|rule| {
            format!(
                "    if ({}) return \"{}\";",
                rule.condition,
                pac_action(rule.action, proxy_addr)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let default_action = pac_action(router.default_action(), proxy_addr);

    format!(
        r#"function FindProxyForURL(url, host) {{
    host = host.toLowerCase();
    if (isPlainHostName(host)) return "DIRECT";
    if (shExpMatch(host, "localhost")) return "DIRECT";
    if (isInNet(dnsResolve(host), "10.0.0.0", "255.0.0.0")) return "DIRECT";
    if (isInNet(dnsResolve(host), "172.16.0.0", "255.240.0.0")) return "DIRECT";
    if (isInNet(dnsResolve(host), "192.168.0.0", "255.255.0.0")) return "DIRECT";
{rule_lines}
    return "{default_action}";
}}
"#
    )
}

fn pac_action(action: RouteAction, proxy_addr: SocketAddr) -> String {
    match action {
        RouteAction::Proxy => format!("PROXY {proxy_addr}"),
        RouteAction::Direct => "DIRECT".into(),
        RouteAction::Block => "PROXY 127.0.0.1:9".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{
            Config, GeositeConfig, PacConfig, ProfileConfig, ProxyConfig, RouteRule, RouteRuleType,
            RoutingConfig, RoutingMode, SystemConfig, TunConfig,
        },
        routing::Router,
    };

    #[test]
    fn pac_contains_route_rules() {
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
                geosite: GeositeConfig {
                    enabled: false,
                    auto_update: false,
                    path: None,
                },
                rules: vec![RouteRule {
                    kind: RouteRuleType::DomainSuffix,
                    value: "example.cn".into(),
                    action: RouteAction::Direct,
                }],
            },
        };

        let pac = generate_pac(&Router::from_config(&config), config.proxy.http_listen);
        assert!(pac.contains(r#"dnsDomainIs(host, ".example.cn")"#));
        assert!(pac.contains(r#"return "DIRECT";"#));
        assert!(pac.contains(r#"return "PROXY 127.0.0.1:7890";"#));
    }

    #[test]
    fn pac_does_not_expand_implicit_cn_fallback() {
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
                geosite: GeositeConfig {
                    enabled: true,
                    auto_update: false,
                    path: None,
                },
                rules: vec![],
            },
        };

        let pac = generate_pac(&Router::from_config(&config), config.proxy.http_listen);
        assert!(!pac.contains("36kr.com"));
        assert!(!pac.contains("baidu.com"));
        assert!(pac.contains(r#"return "PROXY 127.0.0.1:7890";"#));
    }
}

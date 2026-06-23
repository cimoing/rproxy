mod outbound;

use std::{net::SocketAddr, sync::Arc};

use tokio::{
    io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{oneshot, Mutex},
    task::JoinHandle,
};
use tracing::{info, warn};

use outbound::{
    connect_direct, connect_via_node, encode_socks_addr, read_socks_target, TargetAddr,
};

use crate::{
    config::{Config, NodeConfig, RouteAction},
    pac,
    routing::Router,
};

#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    pub running: bool,
    pub http_listen: SocketAddr,
    pub socks_listen: SocketAddr,
    pub pac_listen: Option<SocketAddr>,
    pub active_node: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct ProxyRuntime {
    config: Config,
    router: Router,
    http: Option<ListenerHandle>,
    socks: Option<ListenerHandle>,
    pac: Option<pac::PacServer>,
}

struct ListenerHandle {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ProxyRuntime {
    pub fn new(config: Config) -> Self {
        let router = Router::from_config(&config);
        Self {
            config,
            router,
            http: None,
            socks: None,
            pac: None,
        }
    }

    pub async fn start(&mut self) -> Result<(), RuntimeError> {
        if self.http.is_some() {
            return Ok(());
        }

        self.http = Some(
            start_http_listener(
                self.config.proxy.http_listen,
                self.router.clone(),
                self.config.active_node().cloned(),
            )
            .await?,
        );
        self.socks = Some(
            start_socks_listener(
                self.config.proxy.socks_listen,
                self.router.clone(),
                self.config.active_node().cloned(),
            )
            .await?,
        );

        if self.config.pac.enabled {
            let script = pac::generate_pac(&self.router, self.config.proxy.http_listen);
            self.pac = Some(pac::PacServer::start(self.config.pac.listen, script).await?);
        }

        if let Some(node) = self.config.active_node() {
            info!(
                protocol = ?node.protocol,
                server = %node.server,
                port = node.port,
                "active outbound node configured"
            );
        }

        Ok(())
    }

    pub async fn stop(&mut self) {
        if let Some(handle) = self.http.take() {
            let _ = handle.shutdown.send(());
            let _ = handle.task.await;
        }

        if let Some(handle) = self.socks.take() {
            let _ = handle.shutdown.send(());
            let _ = handle.task.await;
        }

        if let Some(pac) = self.pac.take() {
            pac.stop().await;
        }
    }

    pub fn status(&self) -> RuntimeStatus {
        RuntimeStatus {
            running: self.http.is_some(),
            http_listen: self.config.proxy.http_listen,
            socks_listen: self.config.proxy.socks_listen,
            pac_listen: self.pac.as_ref().map(pac::PacServer::listen),
            active_node: self.config.active_node().map(|node| node.name.clone()),
        }
    }
}

impl Drop for ProxyRuntime {
    fn drop(&mut self) {
        if self.http.is_some() || self.socks.is_some() || self.pac.is_some() {
            warn!("ProxyRuntime dropped while still running");
        }
    }
}

async fn start_http_listener(
    listen: SocketAddr,
    router: Router,
    active_node: Option<NodeConfig>,
) -> Result<ListenerHandle, std::io::Error> {
    let listener = TcpListener::bind(listen).await?;
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    let router = Arc::new(router);
    let active_node = Arc::new(active_node);
    let task = tokio::spawn(async move {
        info!(%listen, "HTTP proxy listener started");
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let Ok((mut stream, peer)) = accepted else {
                        continue;
                    };
                    let router = Arc::clone(&router);
                    let active_node = Arc::clone(&active_node);
                    tokio::spawn(async move {
                        let mut buffer = [0_u8; 2048];
                        let Ok(read) = stream.read(&mut buffer).await else {
                            return;
                        };
                        let request = String::from_utf8_lossy(&buffer[..read]);
                        let Some(target) = parse_http_connect_target(&request) else {
                            let _ = write_http_error(&mut stream, 501, "Only CONNECT is supported").await;
                            return;
                        };
                        let decision = router.decide_host(&target.host_for_routing());
                        info!(%peer, target = %target, action = ?decision.action, reason = %decision.reason, "HTTP CONNECT routed");

                        if decision.action == RouteAction::Block {
                            let _ = write_http_error(&mut stream, 403, "Blocked by routing rule").await;
                            return;
                        }

                        let outbound = match connect_for_decision(&decision.action, &active_node, &target).await {
                            Ok(outbound) => outbound,
                            Err(error) => {
                                warn!(%peer, target = %target, %error, "HTTP CONNECT outbound failed");
                                let _ = write_http_error(&mut stream, 502, "Outbound connection failed").await;
                                return;
                            }
                        };

                        let mut outbound = outbound;
                        if stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.is_err() {
                            return;
                        }
                        let _ = copy_bidirectional(&mut stream, &mut outbound).await;
                    });
                }
            }
        }
        info!(%listen, "HTTP proxy listener stopped");
    });

    Ok(ListenerHandle { shutdown, task })
}

async fn start_socks_listener(
    listen: SocketAddr,
    router: Router,
    active_node: Option<NodeConfig>,
) -> Result<ListenerHandle, std::io::Error> {
    let listener = TcpListener::bind(listen).await?;
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    let router = Arc::new(Mutex::new(router));
    let active_node = Arc::new(active_node);
    let task = tokio::spawn(async move {
        info!(%listen, "SOCKS listener started");
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let Ok((mut stream, peer)) = accepted else {
                        continue;
                    };
                    let router = Arc::clone(&router);
                    let active_node = Arc::clone(&active_node);
                    tokio::spawn(async move {
                        let mut greeting = [0_u8; 2];
                        if stream.read_exact(&mut greeting).await.is_err() {
                            return;
                        }
                        let methods_len = greeting[1] as usize;
                        let mut methods = vec![0_u8; methods_len];
                        if stream.read_exact(&mut methods).await.is_err() {
                            return;
                        }
                        if stream.write_all(&[0x05, 0x00]).await.is_err() {
                            return;
                        }
                        let mut header = [0_u8; 4];
                        if stream.read_exact(&mut header).await.is_err() {
                            return;
                        }
                        if header[0] != 0x05 || header[1] != 0x01 {
                            let _ = write_socks_reply(&mut stream, 0x07, None).await;
                            return;
                        }

                        let target = match read_socks_target(&mut stream, header[3]).await {
                            Ok(target) => target,
                            Err(error) => {
                                warn!(%peer, %error, "SOCKS target parse failed");
                                let _ = write_socks_reply(&mut stream, 0x08, None).await;
                                return;
                            }
                        };

                        let decision = router.lock().await.decide_host(&target.host_for_routing());
                        info!(%peer, target = %target, action = ?decision.action, reason = %decision.reason, "SOCKS request routed");

                        if decision.action == RouteAction::Block {
                            let _ = write_socks_reply(&mut stream, 0x02, None).await;
                            return;
                        }

                        let outbound = match connect_for_decision(&decision.action, &active_node, &target).await {
                            Ok(outbound) => outbound,
                            Err(error) => {
                                warn!(%peer, target = %target, %error, "SOCKS outbound failed");
                                let _ = write_socks_reply(&mut stream, 0x05, None).await;
                                return;
                            }
                        };

                        let bind = TargetAddr::parse_host_port("0.0.0.0:0").ok();
                        if write_socks_reply(&mut stream, 0x00, bind.as_ref()).await.is_err() {
                            return;
                        }

                        let mut outbound = outbound;
                        let _ = copy_bidirectional(&mut stream, &mut outbound).await;
                    });
                }
            }
        }
        info!(%listen, "SOCKS listener stopped");
    });

    Ok(ListenerHandle { shutdown, task })
}

async fn connect_for_decision(
    action: &RouteAction,
    active_node: &Option<NodeConfig>,
    target: &TargetAddr,
) -> Result<tokio::net::TcpStream, outbound::OutboundError> {
    match action {
        RouteAction::Direct => connect_direct(target).await,
        RouteAction::Proxy => {
            let Some(node) = active_node else {
                return connect_direct(target).await;
            };
            connect_via_node(node, target).await
        }
        RouteAction::Block => unreachable!("block is handled before connect_for_decision"),
    }
}

fn parse_http_connect_target(request: &str) -> Option<TargetAddr> {
    let first_line = request.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if !method.eq_ignore_ascii_case("CONNECT") {
        return None;
    }
    TargetAddr::parse_host_port(target).ok()
}

async fn write_http_error(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    message: &str,
) -> std::io::Result<()> {
    let status_text = match status {
        403 => "Forbidden",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        _ => "Proxy Error",
    };
    let body = format!("{message}\n");
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

async fn write_socks_reply(
    stream: &mut tokio::net::TcpStream,
    code: u8,
    bind: Option<&TargetAddr>,
) -> std::io::Result<()> {
    let mut response = vec![0x05, code, 0x00];
    if let Some(bind) = bind {
        encode_socks_addr(bind, &mut response).map_err(std::io::Error::other)?;
        response.extend_from_slice(&bind.port().to_be_bytes());
    } else {
        response.extend_from_slice(&[0x01, 0, 0, 0, 0, 0, 0]);
    }
    stream.write_all(&response).await
}

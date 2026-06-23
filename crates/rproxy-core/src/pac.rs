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
    let direct_cn = router.decide_host("baidu.com").action == RouteAction::Direct;
    let cn_hint = if direct_cn {
        "    if (dnsDomainIs(host, \".cn\")) return \"DIRECT\";\n"
    } else {
        ""
    };

    format!(
        r#"function FindProxyForURL(url, host) {{
    if (isPlainHostName(host)) return "DIRECT";
    if (shExpMatch(host, "localhost")) return "DIRECT";
    if (isInNet(dnsResolve(host), "10.0.0.0", "255.0.0.0")) return "DIRECT";
    if (isInNet(dnsResolve(host), "172.16.0.0", "255.240.0.0")) return "DIRECT";
    if (isInNet(dnsResolve(host), "192.168.0.0", "255.255.0.0")) return "DIRECT";
{cn_hint}    return "PROXY {proxy_addr}";
}}
"#
    )
}

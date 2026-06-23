use std::{env, error::Error, sync::Arc};

use rproxy_core::{config::Config, proxy::ProxyRuntime, routing::Router};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{rustls, TlsConnector};
use webpki_roots::TLS_SERVER_ROOTS;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config_path = env::args()
        .nth(1)
        .unwrap_or_else(|| "config.yaml".to_string());
    let target = env::args()
        .nth(2)
        .unwrap_or_else(|| "google.com:443".to_string());
    let host = target
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(target.as_str())
        .to_string();
    let port = target
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .unwrap_or(443);

    let config = Config::load(&config_path)?;
    let router = Router::from_config(&config);
    let decision = router.decide_host(&host);
    let http_listen = config.proxy.http_listen;
    let active_protocol = config
        .active_node()
        .map(|node| format!("{:?}", node.protocol))
        .unwrap_or_else(|| "none".to_string());

    let mut runtime = ProxyRuntime::new(config);
    runtime.start().await?;

    println!("active_protocol={active_protocol}");
    println!("route_action={:?}", decision.action);
    println!("route_reason={}", decision.reason);
    println!("target={target}");

    let result = test_connect(http_listen, &target, &host, port).await;
    runtime.stop().await;

    let status_line = result?;
    println!("status={status_line}");
    Ok(())
}

async fn test_connect(
    proxy_addr: std::net::SocketAddr,
    target: &str,
    host: &str,
    port: u16,
) -> Result<String, Box<dyn Error>> {
    let mut stream = TcpStream::connect(proxy_addr).await?;
    let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;

    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await? == 0 {
            return Err("proxy closed before CONNECT response".into());
        }
        response.push(byte[0]);
        if response.len() > 8192 {
            return Err("CONNECT response too large".into());
        }
    }

    let connect_response = String::from_utf8_lossy(&response);
    let connect_status = connect_response.lines().next().unwrap_or_default();
    if !connect_status.contains(" 200 ") {
        return Err(format!("CONNECT failed: {connect_status}").into());
    }

    let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let response = if port == 443 {
        let connector = TlsConnector::from(Arc::new(tls_config()));
        let server_name = rustls_pki_types::ServerName::try_from(host.to_string())?;
        let mut tls_stream = connector.connect(server_name, stream).await?;
        tls_stream.write_all(request.as_bytes()).await?;

        let body = read_response_lossy(&mut tls_stream).await?;
        String::from_utf8_lossy(&body).into_owned()
    } else {
        stream.write_all(request.as_bytes()).await?;
        let body = read_response_lossy(&mut stream).await?;
        String::from_utf8_lossy(&body).into_owned()
    };
    Ok(response.lines().next().unwrap_or_default().to_string())
}

async fn read_response_lossy<S>(stream: &mut S) -> Result<Vec<u8>, Box<dyn Error>>
where
    S: AsyncRead + Unpin,
{
    let mut body = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => body.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) if !body.is_empty() => {
                let message = error.to_string();
                if message.contains("close_notify") {
                    break;
                }
                return Err(error.into());
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(body)
}

fn tls_config() -> rustls::ClientConfig {
    let roots = rustls::RootCertStore {
        roots: TLS_SERVER_ROOTS.to_vec(),
    };
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

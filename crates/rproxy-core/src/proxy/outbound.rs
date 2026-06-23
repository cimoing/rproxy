use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use crate::config::{NodeConfig, Protocol};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetAddr {
    Domain { host: String, port: u16 },
    Ip { ip: IpAddr, port: u16 },
}

#[derive(Debug, thiserror::Error)]
pub enum OutboundError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid target address: {0}")]
    InvalidTarget(String),
    #[error("outbound protocol {0:?} is not implemented")]
    UnsupportedProtocol(Protocol),
    #[error("http proxy connect failed: {0}")]
    HttpConnect(String),
    #[error("socks5 proxy connect failed with reply code {0:#04x}")]
    SocksReply(u8),
    #[error("socks5 authentication failed")]
    SocksAuth,
}

impl TargetAddr {
    pub fn parse_host_port(value: &str) -> Result<Self, OutboundError> {
        if let Ok(addr) = value.parse::<SocketAddr>() {
            return Ok(Self::Ip {
                ip: addr.ip(),
                port: addr.port(),
            });
        }

        let (host, port) = value
            .rsplit_once(':')
            .ok_or_else(|| OutboundError::InvalidTarget(value.into()))?;
        let port = port
            .parse::<u16>()
            .map_err(|_| OutboundError::InvalidTarget(value.into()))?;
        if host.trim().is_empty() {
            return Err(OutboundError::InvalidTarget(value.into()));
        }

        Ok(Self::Domain {
            host: host.trim_matches(['[', ']']).to_string(),
            port,
        })
    }

    pub fn host_for_routing(&self) -> String {
        match self {
            Self::Domain { host, .. } => host.clone(),
            Self::Ip { ip, .. } => ip.to_string(),
        }
    }

    pub fn port(&self) -> u16 {
        match self {
            Self::Domain { port, .. } | Self::Ip { port, .. } => *port,
        }
    }
}

impl fmt::Display for TargetAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain { host, port } => write!(f, "{host}:{port}"),
            Self::Ip {
                ip: IpAddr::V4(ip),
                port,
            } => write!(f, "{ip}:{port}"),
            Self::Ip {
                ip: IpAddr::V6(ip),
                port,
            } => write!(f, "[{ip}]:{port}"),
        }
    }
}

pub async fn connect_direct(target: &TargetAddr) -> Result<TcpStream, OutboundError> {
    Ok(TcpStream::connect(target.to_string()).await?)
}

pub async fn connect_via_node(
    node: &NodeConfig,
    target: &TargetAddr,
) -> Result<TcpStream, OutboundError> {
    match node.protocol {
        Protocol::Http => connect_via_http(node, target).await,
        Protocol::Socks => connect_via_socks5(node, target).await,
        Protocol::Vless => Err(OutboundError::UnsupportedProtocol(node.protocol.clone())),
    }
}

async fn connect_via_http(
    node: &NodeConfig,
    target: &TargetAddr,
) -> Result<TcpStream, OutboundError> {
    let mut stream = TcpStream::connect(format!("{}:{}", node.server, node.port)).await?;
    let mut request =
        format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nProxy-Connection: keep-alive\r\n");

    if let (Some(username), Some(password)) = (&node.options.username, &node.options.password) {
        let token = STANDARD.encode(format!("{username}:{password}"));
        request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }

    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;

    let mut response = Vec::with_capacity(1024);
    let mut buf = [0_u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        if stream.read(&mut buf).await? == 0 {
            return Err(OutboundError::HttpConnect(
                "proxy closed connection before response".into(),
            ));
        }
        response.push(buf[0]);
        if response.len() > 8192 {
            return Err(OutboundError::HttpConnect(
                "proxy response header is too large".into(),
            ));
        }
    }

    let status_line = String::from_utf8_lossy(&response)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    if status_line.contains(" 200 ") {
        Ok(stream)
    } else {
        Err(OutboundError::HttpConnect(status_line))
    }
}

async fn connect_via_socks5(
    node: &NodeConfig,
    target: &TargetAddr,
) -> Result<TcpStream, OutboundError> {
    let mut stream = TcpStream::connect(format!("{}:{}", node.server, node.port)).await?;

    let use_auth = node.options.username.is_some() && node.options.password.is_some();
    if use_auth {
        stream.write_all(&[0x05, 0x02, 0x00, 0x02]).await?;
    } else {
        stream.write_all(&[0x05, 0x01, 0x00]).await?;
    }

    let mut method = [0_u8; 2];
    stream.read_exact(&mut method).await?;
    match method {
        [0x05, 0x00] => {}
        [0x05, 0x02] if use_auth => {
            let username = node
                .options
                .username
                .as_deref()
                .unwrap_or_default()
                .as_bytes();
            let password = node
                .options
                .password
                .as_deref()
                .unwrap_or_default()
                .as_bytes();
            if username.len() > u8::MAX as usize || password.len() > u8::MAX as usize {
                return Err(OutboundError::SocksAuth);
            }
            let mut auth = Vec::with_capacity(3 + username.len() + password.len());
            auth.push(0x01);
            auth.push(username.len() as u8);
            auth.extend_from_slice(username);
            auth.push(password.len() as u8);
            auth.extend_from_slice(password);
            stream.write_all(&auth).await?;

            let mut auth_response = [0_u8; 2];
            stream.read_exact(&mut auth_response).await?;
            if auth_response != [0x01, 0x00] {
                return Err(OutboundError::SocksAuth);
            }
        }
        [0x05, 0xff] => return Err(OutboundError::SocksAuth),
        _ => return Err(OutboundError::SocksAuth),
    }

    let mut request = Vec::new();
    request.extend_from_slice(&[0x05, 0x01, 0x00]);
    encode_socks_addr(target, &mut request)?;
    request.extend_from_slice(&target.port().to_be_bytes());
    stream.write_all(&request).await?;

    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).await?;
    if header[1] != 0x00 {
        return Err(OutboundError::SocksReply(header[1]));
    }

    read_socks_bound_addr(&mut stream, header[3]).await?;
    Ok(stream)
}

pub fn encode_socks_addr(target: &TargetAddr, out: &mut Vec<u8>) -> Result<(), OutboundError> {
    match target {
        TargetAddr::Domain { host, .. } => {
            if host.len() > u8::MAX as usize {
                return Err(OutboundError::InvalidTarget(host.clone()));
            }
            out.push(0x03);
            out.push(host.len() as u8);
            out.extend_from_slice(host.as_bytes());
        }
        TargetAddr::Ip {
            ip: IpAddr::V4(ip), ..
        } => {
            out.push(0x01);
            out.extend_from_slice(&ip.octets());
        }
        TargetAddr::Ip {
            ip: IpAddr::V6(ip), ..
        } => {
            out.push(0x04);
            out.extend_from_slice(&ip.octets());
        }
    }
    Ok(())
}

pub async fn read_socks_target(
    stream: &mut TcpStream,
    atyp: u8,
) -> Result<TargetAddr, OutboundError> {
    let target = match atyp {
        0x01 => {
            let mut octets = [0_u8; 4];
            stream.read_exact(&mut octets).await?;
            TargetAddr::Ip {
                ip: IpAddr::V4(Ipv4Addr::from(octets)),
                port: read_port(stream).await?,
            }
        }
        0x03 => {
            let mut len = [0_u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0_u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            TargetAddr::Domain {
                host: String::from_utf8_lossy(&domain).into_owned(),
                port: read_port(stream).await?,
            }
        }
        0x04 => {
            let mut octets = [0_u8; 16];
            stream.read_exact(&mut octets).await?;
            TargetAddr::Ip {
                ip: IpAddr::V6(Ipv6Addr::from(octets)),
                port: read_port(stream).await?,
            }
        }
        _ => return Err(OutboundError::InvalidTarget(format!("atyp {atyp:#04x}"))),
    };

    Ok(target)
}

async fn read_socks_bound_addr(stream: &mut TcpStream, atyp: u8) -> Result<(), OutboundError> {
    match atyp {
        0x01 => {
            let mut skip = [0_u8; 4 + 2];
            stream.read_exact(&mut skip).await?;
        }
        0x03 => {
            let mut len = [0_u8; 1];
            stream.read_exact(&mut len).await?;
            let mut skip = vec![0_u8; len[0] as usize + 2];
            stream.read_exact(&mut skip).await?;
        }
        0x04 => {
            let mut skip = [0_u8; 16 + 2];
            stream.read_exact(&mut skip).await?;
        }
        _ => return Err(OutboundError::InvalidTarget(format!("atyp {atyp:#04x}"))),
    }
    Ok(())
}

async fn read_port(stream: &mut TcpStream) -> Result<u16, OutboundError> {
    let mut port = [0_u8; 2];
    stream.read_exact(&mut port).await?;
    Ok(u16::from_be_bytes(port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_domain_target() {
        let target = TargetAddr::parse_host_port("example.com:443").unwrap();
        assert_eq!(
            target,
            TargetAddr::Domain {
                host: "example.com".into(),
                port: 443
            }
        );
    }

    #[test]
    fn parses_ipv4_target() {
        let target = TargetAddr::parse_host_port("127.0.0.1:80").unwrap();
        assert_eq!(
            target,
            TargetAddr::Ip {
                ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: 80
            }
        );
    }
}

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use aes::cipher::{BlockEncrypt, KeyInit as AesKeyInit};
use aes::Aes128;
use aes_gcm::{
    aead::{Aead, Payload},
    Aes128Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use cfb_mode::cipher::{AsyncStreamCipher, KeyIvInit};
use crc32fast::hash as crc32;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use rand::RngCore;
use sha2::Sha256;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf},
    net::TcpStream,
};
use tokio_rustls::{rustls, TlsConnector};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
};
use uuid::Uuid;
use webpki_roots::TLS_SERVER_ROOTS;

use crate::config::{NodeConfig, Protocol, Transport};

type Aes128CfbEnc = cfb_mode::Encryptor<Aes128>;
type Aes128CfbDec = cfb_mode::BufDecryptor<Aes128>;
type HmacMd5 = Hmac<Md5>;
type HmacSha256 = Hmac<Sha256>;

pub trait AsyncProxyStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> AsyncProxyStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

pub type BoxedProxyStream = Box<dyn AsyncProxyStream>;

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
    #[error("http proxy connect failed: {0}")]
    HttpConnect(String),
    #[error("socks5 proxy connect failed with reply code {0:#04x}")]
    SocksReply(u8),
    #[error("socks5 authentication failed")]
    SocksAuth,
    #[error("vmess config error: {0}")]
    VmessConfig(String),
    #[error("websocket error: {0}")]
    WebSocket(String),
    #[error("tls error: {0}")]
    Tls(String),
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

pub async fn connect_direct(target: &TargetAddr) -> Result<BoxedProxyStream, OutboundError> {
    Ok(Box::new(TcpStream::connect(target.to_string()).await?))
}

pub async fn connect_via_node(
    node: &NodeConfig,
    target: &TargetAddr,
) -> Result<BoxedProxyStream, OutboundError> {
    match node.protocol {
        Protocol::Http => connect_via_http(node, target).await,
        Protocol::Socks => connect_via_socks5(node, target).await,
        Protocol::Vmess => connect_via_vmess(node, target).await,
    }
}

async fn connect_via_http(
    node: &NodeConfig,
    target: &TargetAddr,
) -> Result<BoxedProxyStream, OutboundError> {
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
        Ok(Box::new(stream))
    } else {
        Err(OutboundError::HttpConnect(status_line))
    }
}

async fn connect_via_socks5(
    node: &NodeConfig,
    target: &TargetAddr,
) -> Result<BoxedProxyStream, OutboundError> {
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
    Ok(Box::new(stream))
}

async fn connect_via_vmess(
    node: &NodeConfig,
    target: &TargetAddr,
) -> Result<BoxedProxyStream, OutboundError> {
    if node.options.alter_id.unwrap_or_default() != 0 {
        return Err(OutboundError::VmessConfig(
            "alter_id other than 0 is not supported yet".into(),
        ));
    }
    let session = VmessSession::new(true);
    match node.options.transport {
        Some(Transport::WebSocket) => connect_via_vmess_websocket(node, target, session).await,
        Some(Transport::Tcp) | None => connect_via_vmess_stream(node, target, session).await,
    }
}

async fn connect_via_vmess_stream(
    node: &NodeConfig,
    target: &TargetAddr,
    session: VmessSession,
) -> Result<BoxedProxyStream, OutboundError> {
    let stream = TcpStream::connect(format!("{}:{}", node.server, node.port)).await?;
    let header = encode_vmess_legacy_request(node, target, &session)?;

    if node.options.tls {
        let server_name = rustls_pki_types::ServerName::try_from(node.server.clone())
            .map_err(|error| OutboundError::Tls(error.to_string()))?;
        let connector = TlsConnector::from(Arc::new(tls_config()));
        let mut stream = connector.connect(server_name, stream).await?;
        stream.write_all(&header).await?;
        Ok(Box::new(VmessResponseStream::new(stream, session)))
    } else {
        let mut stream = stream;
        stream.write_all(&header).await?;
        Ok(Box::new(VmessResponseStream::new(stream, session)))
    }
}

async fn connect_via_vmess_websocket(
    node: &NodeConfig,
    target: &TargetAddr,
    session: VmessSession,
) -> Result<BoxedProxyStream, OutboundError> {
    let ws = node
        .options
        .websocket
        .as_ref()
        .ok_or_else(|| OutboundError::VmessConfig("websocket options are required".into()))?;
    let scheme = if node.options.tls { "wss" } else { "ws" };
    let path = if ws.path.starts_with('/') {
        ws.path.clone()
    } else {
        format!("/{}", ws.path)
    };
    let url = format!("{scheme}://{}:{}{path}", node.server, node.port);
    let mut request = url
        .into_client_request()
        .map_err(|error| OutboundError::WebSocket(error.to_string()))?;
    if let Some(host) = &ws.host {
        request.headers_mut().insert(
            "Host",
            host.parse().map_err(|error| {
                OutboundError::WebSocket(format!("invalid websocket host header: {error}"))
            })?,
        );
    }

    let (mut websocket, _) = connect_async(request)
        .await
        .map_err(|error| OutboundError::WebSocket(error.to_string()))?;
    websocket
        .send(Message::Binary(
            encode_vmess_legacy_request(node, target, &session)?.into(),
        ))
        .await
        .map_err(|error| OutboundError::WebSocket(error.to_string()))?;

    let (client, mut app) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let mut response_decoder = VmessResponseDecoder::new(session);
        loop {
            tokio::select! {
                read = read_duplex_chunk(&mut app) => {
                    let Ok(Some(chunk)) = read else {
                        let _ = websocket.close(None).await;
                        break;
                    };
                    if websocket.send(Message::Binary(chunk.into())).await.is_err() {
                        break;
                    }
                }
                message = websocket.next() => {
                    let Some(Ok(message)) = message else {
                        let _ = app.shutdown().await;
                        break;
                    };
                    let data = match message {
                        Message::Binary(data) => data.to_vec(),
                        Message::Close(_) => {
                            let _ = app.shutdown().await;
                            break;
                        }
                        _ => continue,
                    };
                    match response_decoder.decode_chunk(data) {
                        Ok(payload) if !payload.is_empty() => {
                            if app.write_all(&payload).await.is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => {
                            let _ = app.shutdown().await;
                            break;
                        }
                    }
                }
            }
        }
    });

    Ok(Box::new(client))
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

#[derive(Debug, Clone)]
struct VmessSession {
    aead_header: bool,
    request_body_key: [u8; 16],
    request_body_iv: [u8; 16],
    response_body_key: [u8; 16],
    response_body_iv: [u8; 16],
    response_header: u8,
}

impl VmessSession {
    fn new(aead_header: bool) -> Self {
        let mut request_body_key = [0_u8; 16];
        let mut request_body_iv = [0_u8; 16];
        let mut response_header = [0_u8; 1];
        let mut rng = rand::rng();
        rng.fill_bytes(&mut request_body_key);
        rng.fill_bytes(&mut request_body_iv);
        rng.fill_bytes(&mut response_header);

        let response_body_key = if aead_header {
            sha256_16(&request_body_key)
        } else {
            md5_16(&request_body_key)
        };
        let response_body_iv = if aead_header {
            sha256_16(&request_body_iv)
        } else {
            md5_16(&request_body_iv)
        };

        Self {
            aead_header,
            response_body_key,
            response_body_iv,
            request_body_key,
            request_body_iv,
            response_header: response_header[0],
        }
    }
}

fn encode_vmess_legacy_request(
    node: &NodeConfig,
    target: &TargetAddr,
    session: &VmessSession,
) -> Result<Vec<u8>, OutboundError> {
    ensure_vmess_none_security(node)?;
    let uuid = node
        .options
        .uuid
        .as_deref()
        .ok_or_else(|| OutboundError::VmessConfig("uuid is required".into()))?;
    let uuid = Uuid::parse_str(uuid)
        .map_err(|error| OutboundError::VmessConfig(format!("invalid uuid: {error}")))?;

    let timestamp = current_unix_timestamp();
    let command_key = vmess_command_key(uuid.as_bytes());
    let header_iv = vmess_header_iv(timestamp);

    let mut header = Vec::with_capacity(96);
    header.push(0x01);
    header.extend_from_slice(&session.request_body_iv);
    header.extend_from_slice(&session.request_body_key);
    header.push(session.response_header);
    header.push(0x00);
    header.push(0x05);
    header.push(0x00);
    header.push(0x01);
    header.extend_from_slice(&target.port().to_be_bytes());
    encode_vmess_addr(target, &mut header)?;
    let checksum = fnv1a(&header);
    header.extend_from_slice(&checksum.to_be_bytes());

    if session.aead_header {
        return seal_vmess_aead_header(&command_key, &header);
    }

    let mut encrypted_header = header;
    Aes128CfbEnc::new(&command_key.into(), &header_iv.into()).encrypt(&mut encrypted_header);

    let auth = vmess_auth(uuid.as_bytes(), timestamp)?;
    let mut out = Vec::with_capacity(auth.len() + encrypted_header.len());
    out.extend_from_slice(&auth);
    out.extend_from_slice(&encrypted_header);
    Ok(out)
}

fn ensure_vmess_none_security(node: &NodeConfig) -> Result<(), OutboundError> {
    let security = node.options.security.as_deref().unwrap_or("none");
    if !security.eq_ignore_ascii_case("none") {
        return Err(OutboundError::VmessConfig(format!(
            "security {security} is not supported yet; use none"
        )));
    }
    Ok(())
}

fn encode_vmess_addr(target: &TargetAddr, out: &mut Vec<u8>) -> Result<(), OutboundError> {
    match target {
        TargetAddr::Ip {
            ip: IpAddr::V4(ip), ..
        } => {
            out.push(0x01);
            out.extend_from_slice(&ip.octets());
        }
        TargetAddr::Domain { host, .. } => {
            if host.len() > u8::MAX as usize {
                return Err(OutboundError::InvalidTarget(host.clone()));
            }
            out.push(0x02);
            out.push(host.len() as u8);
            out.extend_from_slice(host.as_bytes());
        }
        TargetAddr::Ip {
            ip: IpAddr::V6(ip), ..
        } => {
            out.push(0x03);
            out.extend_from_slice(&ip.octets());
        }
    }
    Ok(())
}

fn current_unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn vmess_command_key(uuid: &[u8; 16]) -> [u8; 16] {
    let mut hasher = Md5::new();
    hasher.update(uuid);
    hasher.update(b"c48619fe-8f02-49e0-b9e9-edf763e17e21");
    hasher.finalize().into()
}

fn vmess_auth(uuid: &[u8; 16], timestamp: i64) -> Result<[u8; 16], OutboundError> {
    let mut hmac = <HmacMd5 as Mac>::new_from_slice(uuid)
        .map_err(|error| OutboundError::VmessConfig(error.to_string()))?;
    hmac.update(&timestamp.to_be_bytes());
    Ok(hmac.finalize().into_bytes().into())
}

fn vmess_header_iv(timestamp: i64) -> [u8; 16] {
    let mut hasher = Md5::new();
    for _ in 0..4 {
        hasher.update(timestamp.to_be_bytes());
    }
    hasher.finalize().into()
}

fn seal_vmess_aead_header(key: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, OutboundError> {
    let auth_id = create_vmess_auth_id(key)?;
    let mut nonce = [0_u8; 8];
    rand::rng().fill_bytes(&mut nonce);

    let length = (data.len() as u16).to_be_bytes();
    let length_key = vmess_kdf16(
        key,
        &[
            b"VMess Header AEAD Key_Length".to_vec(),
            auth_id.to_vec(),
            nonce.to_vec(),
        ],
    )?;
    let length_nonce = vmess_kdf(
        key,
        &[
            b"VMess Header AEAD Nonce_Length".to_vec(),
            auth_id.to_vec(),
            nonce.to_vec(),
        ],
    )?;
    let encrypted_length = aes_gcm_encrypt(
        &length_key,
        &length_nonce[..12],
        &length,
        &auth_id,
        "vmess aead length",
    )?;

    let payload_key = vmess_kdf16(
        key,
        &[
            b"VMess Header AEAD Key".to_vec(),
            auth_id.to_vec(),
            nonce.to_vec(),
        ],
    )?;
    let payload_nonce = vmess_kdf(
        key,
        &[
            b"VMess Header AEAD Nonce".to_vec(),
            auth_id.to_vec(),
            nonce.to_vec(),
        ],
    )?;
    let encrypted_payload = aes_gcm_encrypt(
        &payload_key,
        &payload_nonce[..12],
        data,
        &auth_id,
        "vmess aead payload",
    )?;

    let mut out = Vec::with_capacity(16 + encrypted_length.len() + 8 + encrypted_payload.len());
    out.extend_from_slice(&auth_id);
    out.extend_from_slice(&encrypted_length);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&encrypted_payload);
    Ok(out)
}

fn create_vmess_auth_id(key: &[u8; 16]) -> Result<[u8; 16], OutboundError> {
    let mut block = [0_u8; 16];
    block[..8].copy_from_slice(&current_unix_timestamp().to_be_bytes());
    rand::rng().fill_bytes(&mut block[8..12]);
    let checksum = crc32(&block[..12]).to_be_bytes();
    block[12..].copy_from_slice(&checksum);

    let encryption_key = vmess_kdf16(key, &[b"AES Auth ID Encryption".to_vec()])?;
    let cipher = Aes128::new_from_slice(&encryption_key)
        .map_err(|error| OutboundError::VmessConfig(error.to_string()))?;
    cipher.encrypt_block((&mut block).into());
    Ok(block)
}

fn aes_gcm_encrypt(
    key: &[u8],
    nonce: &[u8],
    data: &[u8],
    aad: &[u8],
    label: &str,
) -> Result<Vec<u8>, OutboundError> {
    let cipher = Aes128Gcm::new_from_slice(key)
        .map_err(|error| OutboundError::VmessConfig(error.to_string()))?;
    cipher
        .encrypt(Nonce::from_slice(nonce), Payload { msg: data, aad })
        .map_err(|_| OutboundError::VmessConfig(format!("{label} encrypt failed")))
}

fn aes_gcm_decrypt(
    key: &[u8],
    nonce: &[u8],
    data: &[u8],
    aad: &[u8],
    label: &str,
) -> Result<Vec<u8>, OutboundError> {
    let cipher = Aes128Gcm::new_from_slice(key)
        .map_err(|error| OutboundError::VmessConfig(error.to_string()))?;
    cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: data, aad })
        .map_err(|_| OutboundError::VmessConfig(format!("{label} decrypt failed")))
}

fn vmess_kdf16(key: &[u8], path: &[Vec<u8>]) -> Result<[u8; 16], OutboundError> {
    let digest = vmess_kdf(key, path)?;
    let mut out = [0_u8; 16];
    out.copy_from_slice(&digest[..16]);
    Ok(out)
}

fn vmess_kdf(key: &[u8], path: &[Vec<u8>]) -> Result<[u8; 32], OutboundError> {
    let mut salts = Vec::with_capacity(path.len() + 1);
    salts.push(b"VMess AEAD KDF".to_vec());
    salts.extend_from_slice(path);
    nested_hmac_sha256(&salts, key)
}

fn nested_hmac_sha256(salts: &[Vec<u8>], data: &[u8]) -> Result<[u8; 32], OutboundError> {
    if salts.len() == 1 {
        let mut hmac = <HmacSha256 as Mac>::new_from_slice(&salts[0])
            .map_err(|error| OutboundError::VmessConfig(error.to_string()))?;
        hmac.update(data);
        return Ok(hmac.finalize().into_bytes().into());
    }

    let digest = |input: &[u8]| nested_hmac_sha256(&salts[..salts.len() - 1], input);
    custom_hmac_digest(&salts[salts.len() - 1], data, digest)
}

fn custom_hmac_digest(
    key: &[u8],
    data: &[u8],
    digest: impl Fn(&[u8]) -> Result<[u8; 32], OutboundError>,
) -> Result<[u8; 32], OutboundError> {
    const BLOCK_SIZE: usize = 64;
    let mut normalized_key = [0_u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        normalized_key[..32].copy_from_slice(&digest(key)?);
    } else {
        normalized_key[..key.len()].copy_from_slice(key);
    }

    let mut inner = [0x36_u8; BLOCK_SIZE];
    let mut outer = [0x5c_u8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner[index] ^= normalized_key[index];
        outer[index] ^= normalized_key[index];
    }

    let mut inner_data = Vec::with_capacity(BLOCK_SIZE + data.len());
    inner_data.extend_from_slice(&inner);
    inner_data.extend_from_slice(data);
    let inner_digest = digest(&inner_data)?;

    let mut outer_data = Vec::with_capacity(BLOCK_SIZE + inner_digest.len());
    outer_data.extend_from_slice(&outer);
    outer_data.extend_from_slice(&inner_digest);
    digest(&outer_data)
}

fn md5_16(input: &[u8; 16]) -> [u8; 16] {
    Md5::digest(input).into()
}

fn sha256_16(input: &[u8; 16]) -> [u8; 16] {
    let digest = Sha256::digest(input);
    let mut out = [0_u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c9dc5_u32;
    for byte in bytes {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

fn tls_config() -> rustls::ClientConfig {
    let roots = rustls::RootCertStore {
        roots: TLS_SERVER_ROOTS.to_vec(),
    };
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

async fn read_duplex_chunk(stream: &mut DuplexStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut buffer = vec![0_u8; 16 * 1024];
    let read = stream.read(&mut buffer).await?;
    if read == 0 {
        return Ok(None);
    }
    buffer.truncate(read);
    Ok(Some(buffer))
}

struct VmessResponseStream<S> {
    inner: S,
    decoder: VmessResponseDecoder,
    pending_plaintext: Vec<u8>,
}

impl<S> VmessResponseStream<S> {
    fn new(inner: S, session: VmessSession) -> Self {
        Self {
            inner,
            decoder: VmessResponseDecoder::new(session),
            pending_plaintext: Vec::new(),
        }
    }
}

impl<S> AsyncRead for VmessResponseStream<S>
where
    S: AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.pending_plaintext.is_empty() {
            let len = self.pending_plaintext.len().min(buf.remaining());
            let drained = self.pending_plaintext.drain(..len).collect::<Vec<_>>();
            buf.put_slice(&drained);
            return Poll::Ready(Ok(()));
        }

        loop {
            let mut encrypted = vec![0_u8; 16 * 1024];
            let mut read_buf = ReadBuf::new(&mut encrypted);
            match Pin::new(&mut self.inner).poll_read(cx, &mut read_buf) {
                Poll::Ready(Ok(())) if read_buf.filled().is_empty() => return Poll::Ready(Ok(())),
                Poll::Ready(Ok(())) => {
                    let plaintext = match self.decoder.decode_chunk(read_buf.filled().to_vec()) {
                        Ok(plaintext) => plaintext,
                        Err(error) => {
                            return Poll::Ready(Err(std::io::Error::other(error)));
                        }
                    };
                    if plaintext.is_empty() {
                        continue;
                    }
                    let len = plaintext.len().min(buf.remaining());
                    buf.put_slice(&plaintext[..len]);
                    if len < plaintext.len() {
                        self.pending_plaintext.extend_from_slice(&plaintext[len..]);
                    }
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<S> AsyncWrite for VmessResponseStream<S>
where
    S: AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

struct VmessResponseDecoder {
    decryptor: Aes128CfbDec,
    aead_header: bool,
    response_body_key: [u8; 16],
    response_body_iv: [u8; 16],
    aead_buffer: Vec<u8>,
    response_prefix: Vec<u8>,
    response_header: u8,
    consumed_header: bool,
}

impl VmessResponseDecoder {
    fn new(session: VmessSession) -> Self {
        Self {
            decryptor: Aes128CfbDec::new(
                &session.response_body_key.into(),
                &session.response_body_iv.into(),
            ),
            aead_header: session.aead_header,
            response_body_key: session.response_body_key,
            response_body_iv: session.response_body_iv,
            aead_buffer: Vec::with_capacity(128),
            response_prefix: Vec::with_capacity(4),
            response_header: session.response_header,
            consumed_header: false,
        }
    }

    fn decode_chunk(&mut self, data: Vec<u8>) -> Result<Vec<u8>, OutboundError> {
        if self.consumed_header {
            return Ok(data);
        }

        if self.aead_header {
            return self.decode_aead_header(data);
        }

        let mut offset = 0;
        while offset < data.len() && !self.consumed_header {
            if self.response_prefix.len() < 4 {
                let needed = 4 - self.response_prefix.len();
                let take = needed.min(data.len() - offset);
                let mut chunk = data[offset..offset + take].to_vec();
                self.decryptor.decrypt(&mut chunk);
                self.response_prefix.extend_from_slice(&chunk);
                offset += take;

                if self.response_prefix.len() < 4 {
                    continue;
                }

                if self.response_prefix[0] != self.response_header {
                    self.consumed_header = true;
                    return Err(OutboundError::VmessConfig(
                        "unexpected vmess response header".into(),
                    ));
                }
            }

            let command_len = self.response_prefix[3] as usize;
            let expected_prefix_len = 4 + command_len;
            if self.response_prefix.len() < expected_prefix_len {
                let needed = expected_prefix_len - self.response_prefix.len();
                let take = needed.min(data.len() - offset);
                let mut chunk = data[offset..offset + take].to_vec();
                self.decryptor.decrypt(&mut chunk);
                self.response_prefix.extend_from_slice(&chunk);
                offset += take;
            }

            if self.response_prefix.len() >= expected_prefix_len {
                self.consumed_header = true;
            }
        }

        if self.consumed_header && offset < data.len() {
            Ok(data[offset..].to_vec())
        } else {
            Ok(Vec::new())
        }
    }

    fn decode_aead_header(&mut self, data: Vec<u8>) -> Result<Vec<u8>, OutboundError> {
        self.aead_buffer.extend_from_slice(&data);
        if self.aead_buffer.len() < 18 {
            return Ok(Vec::new());
        }

        let length_key = vmess_kdf16(
            &self.response_body_key,
            &[b"AEAD Resp Header Len Key".to_vec()],
        )?;
        let length_nonce = vmess_kdf(
            &self.response_body_iv,
            &[b"AEAD Resp Header Len IV".to_vec()],
        )?;
        let length = aes_gcm_decrypt(
            &length_key,
            &length_nonce[..12],
            &self.aead_buffer[..18],
            &[],
            "vmess aead response length",
        )?;
        if length.len() != 2 {
            return Ok(Vec::new());
        }
        let payload_len = u16::from_be_bytes([length[0], length[1]]) as usize;
        let encrypted_payload_len = payload_len + 16;
        let header_len = 18 + encrypted_payload_len;
        if self.aead_buffer.len() < header_len {
            return Ok(Vec::new());
        }

        let payload_key =
            vmess_kdf16(&self.response_body_key, &[b"AEAD Resp Header Key".to_vec()])?;
        let payload_nonce = vmess_kdf(&self.response_body_iv, &[b"AEAD Resp Header IV".to_vec()])?;
        let response_prefix = aes_gcm_decrypt(
            &payload_key,
            &payload_nonce[..12],
            &self.aead_buffer[18..header_len],
            &[],
            "vmess aead response payload",
        )?;
        if response_prefix.len() < 4 || response_prefix[0] != self.response_header {
            self.consumed_header = true;
            return Err(OutboundError::VmessConfig(
                "unexpected vmess aead response header".into(),
            ));
        }

        self.consumed_header = true;
        Ok(self.aead_buffer[header_len..].to_vec())
    }
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

    #[test]
    fn decodes_vmess_response_header_and_keeps_plain_payload() {
        let session = VmessSession::new(false);
        let mut encrypted_prefix = vec![session.response_header, 0x00, 0x00, 0x00];
        cfb_mode::BufEncryptor::<Aes128>::new(
            &session.response_body_key.into(),
            &session.response_body_iv.into(),
        )
        .encrypt(&mut encrypted_prefix);
        encrypted_prefix.extend_from_slice(b"hello");

        let mut decoder = VmessResponseDecoder::new(session);
        assert_eq!(decoder.decode_chunk(encrypted_prefix).unwrap(), b"hello");
        assert_eq!(decoder.decode_chunk(b" world".to_vec()).unwrap(), b" world");
    }

    #[test]
    fn vmess_kdf_matches_v2fly_vector() {
        let digest = vmess_kdf(
            b"Demo Key for KDF Value Test",
            &[
                b"Demo Path for KDF Value Test".to_vec(),
                b"Demo Path for KDF Value Test2".to_vec(),
                b"Demo Path for KDF Value Test3".to_vec(),
            ],
        )
        .unwrap();
        assert_eq!(
            hex_lower(&digest),
            "53e9d7e1bd7bd25022b71ead07d8a596efc8a845c7888652fd684b4903dc8892"
        );
    }

    fn hex_lower(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

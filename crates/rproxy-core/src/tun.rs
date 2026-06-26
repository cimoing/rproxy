use std::{
    env, io,
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use tracing::{info, warn};

use crate::{
    config::{Config, NodeConfig, RouteAction},
    routing::Router,
};

#[cfg(windows)]
const TUN_ADDR: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
#[cfg(target_os = "linux")]
const TUN_PREFIX: &str = "198.18.0.1/15";
const TUN_DNS: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
#[cfg(windows)]
const TUN_NETMASK: &str = "255.254.0.0";
const DIRECT_DNS_UPSTREAM: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(223, 5, 5, 5)), 53);
const PROXY_DNS_UPSTREAM: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 53);
const DIRECT_DNS_ROUTE_IPS: &[Ipv4Addr] = &[Ipv4Addr::new(223, 5, 5, 5)];
const PROXY_DNS_ROUTE_IPS: &[Ipv4Addr] = &[
    Ipv4Addr::new(8, 8, 8, 8),
    Ipv4Addr::new(8, 8, 4, 4),
    Ipv4Addr::new(1, 1, 1, 1),
    Ipv4Addr::new(1, 0, 0, 1),
    Ipv4Addr::new(9, 9, 9, 9),
    Ipv4Addr::new(149, 112, 112, 112),
    Ipv4Addr::new(208, 67, 222, 222),
    Ipv4Addr::new(208, 67, 220, 220),
];

#[derive(Debug, Clone)]
pub struct TunStatus {
    pub enabled: bool,
    pub interface_name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TunError {
    #[error("tun2socks executable was not found; set RPROXY_TUN2SOCKS or add tun2socks to PATH")]
    Tun2SocksNotFound,
    #[error("Tun mode requires an active outbound node")]
    MissingActiveNode,
    #[error("failed to resolve active node {0}")]
    ResolveNode(String),
    #[error("failed to detect default route")]
    DefaultRouteNotFound,
    #[error("command failed: {0}")]
    Command(String),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

pub struct TunRuntime {
    interface_name: String,
    route_guard: Option<RouteGuard>,
    dns: Option<DnsRuntime>,
    child: Option<Child>,
}

struct RouteGuard {
    interface_name: String,
    node_ip: Option<IpAddr>,
    dns_configured: bool,
}

struct DnsRuntime {
    shutdown: mpsc::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TunRuntime {
    pub fn start(config: &Config) -> Result<Option<Self>, TunError> {
        if !config.tun.enabled {
            return Ok(None);
        }

        let active_node = config.active_node().ok_or(TunError::MissingActiveNode)?;
        let node_ip = resolve_node_ip(active_node)?;
        let interface_name = config.tun.interface_name.clone();
        let tun2socks = find_tun2socks().ok_or(TunError::Tun2SocksNotFound)?;
        let proxy_uri = socks_proxy_uri(config.proxy.socks_listen);

        let mut child = Command::new(&tun2socks)
            .args([
                "-device",
                &format!("tun://{interface_name}"),
                "-proxy",
                &proxy_uri,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                TunError::Command(format!("failed to start {}: {error}", tun2socks.display()))
            })?;

        thread::sleep(Duration::from_millis(500));
        if let Some(status) = child.try_wait()? {
            return Err(TunError::Command(format!(
                "tun2socks exited during startup with status {status}"
            )));
        }

        let mut route_guard = if config.tun.auto_route {
            match configure_routes(&interface_name, node_ip) {
                Ok(guard) => Some(guard),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        } else {
            None
        };

        let dns = if config.tun.auto_route {
            match DnsRuntime::start(config.proxy.socks_listen, Router::from_config(config)) {
                Ok(dns) => {
                    let dns_configured = configure_dns(&interface_name);
                    if !dns_configured {
                        warn!(
                            interface = %interface_name,
                            dns = %TUN_DNS,
                            "failed to configure system DNS for Tun; continuing without DNS hijack"
                        );
                    } else if let Some(guard) = &mut route_guard {
                        guard.dns_configured = true;
                    }
                    Some(dns)
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(guard) = route_guard.take() {
                        guard.restore();
                    }
                    return Err(error);
                }
            }
        } else {
            None
        };

        info!(
            interface = %interface_name,
            proxy = %proxy_uri,
            node = %active_node.server,
            "Tun mode started"
        );

        Ok(Some(Self {
            interface_name,
            route_guard,
            dns,
            child: Some(child),
        }))
    }

    pub fn status(&self) -> TunStatus {
        TunStatus {
            enabled: true,
            interface_name: self.interface_name.clone(),
        }
    }

    pub fn stop(&mut self) {
        if self.route_guard.is_none() && self.dns.is_none() && self.child.is_none() {
            return;
        }
        if let Some(mut dns) = self.dns.take() {
            dns.stop();
        }
        if let Some(guard) = self.route_guard.take() {
            guard.restore();
        }
        if let Some(mut child) = self.child.take() {
            if let Err(error) = child.kill() {
                warn!(%error, "failed to stop tun2socks process");
            }
            let _ = child.wait();
        }
        info!(interface = %self.interface_name, "Tun mode stopped");
    }
}

impl Drop for TunRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

impl RouteGuard {
    fn restore(self) {
        cleanup_routes(&self.interface_name, self.node_ip, self.dns_configured);
    }
}

impl DnsRuntime {
    fn start(socks_listen: SocketAddr, router: Router) -> Result<Self, TunError> {
        let listen = SocketAddr::new(IpAddr::V4(TUN_DNS), 53);
        let socket = std::net::UdpSocket::bind(listen)?;
        socket.set_read_timeout(Some(Duration::from_millis(300)))?;
        let (shutdown, shutdown_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut buffer = [0_u8; 1500];
            loop {
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }
                let Ok((len, peer)) = socket.recv_from(&mut buffer) else {
                    continue;
                };
                let query = buffer[..len].to_vec();
                let response =
                    resolve_dns_query(&query, socks_listen, &router).unwrap_or_else(|error| {
                        warn!(%error, "Tun DNS query failed");
                        dns_servfail_response(&query)
                    });
                let _ = socket.send_to(&response, peer);
            }
        });

        info!(%listen, "Tun DNS proxy started");
        Ok(Self {
            shutdown,
            thread: Some(thread),
        })
    }

    fn stop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        info!("Tun DNS proxy stopped");
    }
}

fn socks_proxy_uri(listen: SocketAddr) -> String {
    format!("socks5://{listen}")
}

fn resolve_dns_query(
    query: &[u8],
    socks_listen: SocketAddr,
    router: &Router,
) -> Result<Vec<u8>, TunError> {
    if dns_query_has_type(query, 28) {
        return Ok(dns_empty_response(query));
    }

    let action = dns_query_name(query)
        .map(|host| router.decide_host(&host).action)
        .unwrap_or_else(|| router.default_action());
    match action {
        RouteAction::Direct => resolve_dns_query_direct(query, DIRECT_DNS_UPSTREAM),
        RouteAction::Proxy => resolve_dns_query_via_socks(query, socks_listen, PROXY_DNS_UPSTREAM),
        RouteAction::Block => Ok(dns_servfail_response(query)),
    }
}

fn resolve_dns_query_direct(query: &[u8], upstream: SocketAddr) -> Result<Vec<u8>, TunError> {
    let socket = std::net::UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))?;
    socket.send_to(query, upstream)?;

    let mut response = vec![0_u8; 4096];
    let (len, _) = socket.recv_from(&mut response)?;
    response.truncate(len);
    Ok(response)
}

fn resolve_dns_query_via_socks(
    query: &[u8],
    socks_listen: SocketAddr,
    upstream: SocketAddr,
) -> Result<Vec<u8>, TunError> {
    let mut stream = std::net::TcpStream::connect(socks_listen)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    socks5_connect(&mut stream, upstream)?;

    let len = u16::try_from(query.len())
        .map_err(|_| TunError::Command("DNS query is too large".into()))?
        .to_be_bytes();
    io::Write::write_all(&mut stream, &len)?;
    io::Write::write_all(&mut stream, query)?;

    let mut len = [0_u8; 2];
    io::Read::read_exact(&mut stream, &mut len)?;
    let response_len = u16::from_be_bytes(len) as usize;
    if response_len > 4096 {
        return Err(TunError::Command("DNS response is too large".into()));
    }
    let mut response = vec![0_u8; response_len];
    io::Read::read_exact(&mut stream, &mut response)?;
    Ok(response)
}

fn socks5_connect(stream: &mut std::net::TcpStream, target: SocketAddr) -> Result<(), TunError> {
    io::Write::write_all(stream, &[0x05, 0x01, 0x00])?;
    let mut greeting = [0_u8; 2];
    io::Read::read_exact(stream, &mut greeting)?;
    if greeting != [0x05, 0x00] {
        return Err(TunError::Command(format!(
            "local SOCKS rejected DNS proxy auth: {greeting:02x?}"
        )));
    }

    let mut request = vec![0x05, 0x01, 0x00];
    match target.ip() {
        IpAddr::V4(ip) => {
            request.push(0x01);
            request.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            request.push(0x04);
            request.extend_from_slice(&ip.octets());
        }
    }
    request.extend_from_slice(&target.port().to_be_bytes());
    io::Write::write_all(stream, &request)?;

    let mut header = [0_u8; 4];
    io::Read::read_exact(stream, &mut header)?;
    if header[0] != 0x05 || header[1] != 0x00 {
        return Err(TunError::Command(format!(
            "local SOCKS failed DNS CONNECT with code {:#04x}",
            header[1]
        )));
    }

    let addr_len = match header[3] {
        0x01 => 4,
        0x03 => {
            let mut len = [0_u8; 1];
            io::Read::read_exact(stream, &mut len)?;
            len[0] as usize
        }
        0x04 => 16,
        atyp => {
            return Err(TunError::Command(format!(
                "local SOCKS returned invalid address type {atyp:#04x}"
            )));
        }
    };
    let mut skip = vec![0_u8; addr_len + 2];
    io::Read::read_exact(stream, &mut skip)?;
    Ok(())
}

fn dns_query_has_type(query: &[u8], qtype: u16) -> bool {
    dns_question_end(query)
        .and_then(|question_end| query.get(question_end..question_end + 4))
        .map(|tail| u16::from_be_bytes([tail[0], tail[1]]) == qtype)
        .unwrap_or(false)
}

fn dns_query_name(query: &[u8]) -> Option<String> {
    if query.len() < 12 {
        return None;
    }
    let question_count = u16::from_be_bytes([query[4], query[5]]);
    if question_count == 0 {
        return None;
    }

    let mut labels = Vec::new();
    let mut cursor = 12;
    while cursor < query.len() {
        let len = *query.get(cursor)?;
        cursor += 1;
        if len == 0 {
            break;
        }
        if len & 0xc0 != 0 {
            return None;
        }
        let len = len as usize;
        let end = cursor.checked_add(len)?;
        let label = std::str::from_utf8(query.get(cursor..end)?).ok()?;
        labels.push(label.to_ascii_lowercase());
        cursor = end;
    }

    (!labels.is_empty()).then(|| labels.join("."))
}

fn dns_question_end(query: &[u8]) -> Option<usize> {
    if query.len() < 12 {
        return None;
    }
    let question_count = u16::from_be_bytes([query[4], query[5]]);
    if question_count == 0 {
        return None;
    }
    let mut cursor = 12;
    while cursor < query.len() {
        let len = *query.get(cursor)? as usize;
        cursor += 1;
        if len == 0 {
            return Some(cursor);
        }
        cursor = cursor.checked_add(len)?;
    }
    None
}

fn dns_empty_response(query: &[u8]) -> Vec<u8> {
    dns_response_with_code(query, 0)
}

fn dns_servfail_response(query: &[u8]) -> Vec<u8> {
    dns_response_with_code(query, 2)
}

fn dns_response_with_code(query: &[u8], rcode: u8) -> Vec<u8> {
    let Some(question_end) = dns_question_end(query) else {
        return Vec::new();
    };
    let question_tail_end = question_end.saturating_add(4).min(query.len());
    let mut response = Vec::with_capacity(question_tail_end);
    response.extend_from_slice(&query[..question_tail_end]);
    response[2] = 0x81;
    response[3] = 0x80 | (rcode & 0x0f);
    response[6] = 0;
    response[7] = 0;
    response[8] = 0;
    response[9] = 0;
    response[10] = 0;
    response[11] = 0;
    response
}

fn resolve_node_ip(node: &NodeConfig) -> Result<IpAddr, TunError> {
    if let Ok(ip) = node.server.parse::<IpAddr>() {
        return Ok(ip);
    }

    (node.server.as_str(), node.port)
        .to_socket_addrs()?
        .next()
        .map(|addr| addr.ip())
        .ok_or_else(|| TunError::ResolveNode(node.server.clone()))
}

fn find_tun2socks() -> Option<PathBuf> {
    env::var_os("RPROXY_TUN2SOCKS")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| find_in_path(executable_name("tun2socks")))
}

fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    }
}

fn find_in_path(name: String) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|path| path.join(&name))
            .find(|path| path.is_file())
    })
}

#[cfg(target_os = "linux")]
fn find_command<'a>(names: &'a [&'a str]) -> Option<&'a str> {
    names
        .iter()
        .copied()
        .find(|name| Command::new(name).arg("--version").output().is_ok())
}

fn run_command(program: &str, args: &[&str]) -> Result<String, TunError> {
    let output = Command::new(program).args(args).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(TunError::Command(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[cfg(target_os = "linux")]
fn configure_routes(interface_name: &str, node_ip: IpAddr) -> Result<RouteGuard, TunError> {
    let default_route = linux_default_route()?;
    run_command("ip", &["addr", "add", TUN_PREFIX, "dev", interface_name])?;
    run_command("ip", &["link", "set", "dev", interface_name, "up"])?;

    if let IpAddr::V4(ip) = node_ip {
        run_command(
            "ip",
            &[
                "route",
                "add",
                &format!("{ip}/32"),
                "via",
                &default_route.gateway.to_string(),
                "dev",
                &default_route.interface,
            ],
        )?;
    }

    configure_linux_dns_routes(interface_name, &default_route);

    run_command("ip", &["route", "add", "0.0.0.0/1", "dev", interface_name])?;
    run_command(
        "ip",
        &["route", "add", "128.0.0.0/1", "dev", interface_name],
    )?;

    Ok(RouteGuard {
        interface_name: interface_name.into(),
        node_ip: Some(node_ip),
        dns_configured: false,
    })
}

#[cfg(target_os = "linux")]
fn cleanup_routes(interface_name: &str, node_ip: Option<IpAddr>, dns_configured: bool) {
    if dns_configured {
        cleanup_dns(interface_name);
    }
    let _ = run_command("ip", &["route", "del", "0.0.0.0/1", "dev", interface_name]);
    let _ = run_command(
        "ip",
        &["route", "del", "128.0.0.0/1", "dev", interface_name],
    );
    if let Some(IpAddr::V4(ip)) = node_ip {
        let _ = run_command("ip", &["route", "del", &format!("{ip}/32")]);
    }
    cleanup_linux_dns_routes();
    let _ = run_command("ip", &["link", "del", "dev", interface_name]);
}

#[cfg(target_os = "linux")]
fn configure_linux_dns_routes(interface_name: &str, default_route: &LinuxDefaultRoute) {
    for ip in DIRECT_DNS_ROUTE_IPS {
        if let Err(error) = run_command(
            "ip",
            &[
                "route",
                "add",
                &format!("{ip}/32"),
                "via",
                &default_route.gateway.to_string(),
                "dev",
                &default_route.interface,
            ],
        ) {
            warn!(%error, dns = %ip, "failed to add direct DNS bypass route");
        }
    }

    for ip in PROXY_DNS_ROUTE_IPS {
        if let Err(error) = run_command(
            "ip",
            &["route", "add", &format!("{ip}/32"), "dev", interface_name],
        ) {
            warn!(%error, dns = %ip, "failed to add proxy DNS Tun route");
        }
    }
}

#[cfg(target_os = "linux")]
fn cleanup_linux_dns_routes() {
    for ip in DIRECT_DNS_ROUTE_IPS
        .iter()
        .chain(PROXY_DNS_ROUTE_IPS.iter())
    {
        let _ = run_command("ip", &["route", "del", &format!("{ip}/32")]);
    }
}

#[cfg(target_os = "linux")]
fn configure_dns(interface_name: &str) -> bool {
    configure_resolved_dns(interface_name) || configure_network_manager_dns(interface_name)
}

#[cfg(target_os = "linux")]
fn cleanup_dns(interface_name: &str) {
    if let Some(resolver) = find_command(&["resolvectl", "systemd-resolve"]) {
        let _ = run_command(resolver, &["revert", interface_name]);
    }
    if let Some(nmcli) = find_command(&["nmcli"]) {
        let _ = run_command(nmcli, &["device", "reapply", interface_name]);
    }
}

#[cfg(target_os = "linux")]
fn configure_resolved_dns(interface_name: &str) -> bool {
    let Some(resolver) = find_command(&["resolvectl", "systemd-resolve"]) else {
        return false;
    };
    if let Err(error) = run_command(resolver, &["dns", interface_name, &TUN_DNS.to_string()]) {
        warn!(%error, "failed to configure Tun DNS through systemd-resolved");
        return false;
    }
    if let Err(error) = run_command(resolver, &["domain", interface_name, "~."]) {
        warn!(%error, "failed to configure Tun DNS domain through systemd-resolved");
        return false;
    }
    let _ = run_command(resolver, &["default-route", interface_name, "yes"]);
    let _ = run_command(resolver, &["flush-caches"]);
    true
}

#[cfg(target_os = "linux")]
fn configure_network_manager_dns(interface_name: &str) -> bool {
    let Some(nmcli) = find_command(&["nmcli"]) else {
        return false;
    };
    if let Err(error) = run_command(
        nmcli,
        &[
            "device",
            "modify",
            interface_name,
            "ipv4.dns",
            &TUN_DNS.to_string(),
            "ipv4.ignore-auto-dns",
            "yes",
            "ipv6.ignore-auto-dns",
            "yes",
        ],
    ) {
        warn!(%error, "failed to configure Tun DNS through NetworkManager");
        return false;
    }
    let _ = run_command(nmcli, &["device", "reapply", interface_name]);
    true
}

#[cfg(target_os = "linux")]
struct LinuxDefaultRoute {
    gateway: Ipv4Addr,
    interface: String,
}

#[cfg(target_os = "linux")]
fn linux_default_route() -> Result<LinuxDefaultRoute, TunError> {
    let output = run_command("ip", &["route", "show", "default"])?;
    parse_linux_default_route(&output).ok_or(TunError::DefaultRouteNotFound)
}

#[cfg(target_os = "linux")]
fn parse_linux_default_route(output: &str) -> Option<LinuxDefaultRoute> {
    for line in output.lines() {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.first().copied() != Some("default") {
            continue;
        }
        let gateway = parts
            .windows(2)
            .find_map(|pair| (pair[0] == "via").then(|| pair[1].parse::<Ipv4Addr>().ok()))
            .flatten()?;
        let interface = parts
            .windows(2)
            .find_map(|pair| (pair[0] == "dev").then(|| pair[1].to_string()))?;
        return Some(LinuxDefaultRoute { gateway, interface });
    }
    None
}

#[cfg(windows)]
fn configure_routes(interface_name: &str, node_ip: IpAddr) -> Result<RouteGuard, TunError> {
    let default_route = windows_default_route()?;
    run_command(
        "netsh",
        &[
            "interface",
            "ipv4",
            "set",
            "address",
            &format!("name={interface_name}"),
            "static",
            &TUN_ADDR.to_string(),
            TUN_NETMASK,
        ],
    )?;

    if let IpAddr::V4(ip) = node_ip {
        run_command(
            "route",
            &[
                "add",
                &ip.to_string(),
                "mask",
                "255.255.255.255",
                &default_route.gateway.to_string(),
            ],
        )?;
    }

    configure_windows_dns_routes(&default_route);

    run_command(
        "route",
        &[
            "add",
            "0.0.0.0",
            "mask",
            "128.0.0.0",
            &TUN_ADDR.to_string(),
            "metric",
            "1",
        ],
    )?;
    run_command(
        "route",
        &[
            "add",
            "128.0.0.0",
            "mask",
            "128.0.0.0",
            &TUN_ADDR.to_string(),
            "metric",
            "1",
        ],
    )?;

    Ok(RouteGuard {
        interface_name: interface_name.into(),
        node_ip: Some(node_ip),
        dns_configured: false,
    })
}

#[cfg(windows)]
fn cleanup_routes(interface_name: &str, node_ip: Option<IpAddr>, dns_configured: bool) {
    let _ = dns_configured;
    let _ = run_command("route", &["delete", "0.0.0.0", "mask", "128.0.0.0"]);
    let _ = run_command("route", &["delete", "128.0.0.0", "mask", "128.0.0.0"]);
    if let Some(IpAddr::V4(ip)) = node_ip {
        let _ = run_command("route", &["delete", &ip.to_string()]);
    }
    cleanup_windows_dns_routes();
    let _ = run_command(
        "netsh",
        &[
            "interface",
            "ipv4",
            "delete",
            "address",
            &format!("name={interface_name}"),
            &TUN_ADDR.to_string(),
        ],
    );
}

#[cfg(windows)]
fn configure_windows_dns_routes(default_route: &WindowsDefaultRoute) {
    for ip in DIRECT_DNS_ROUTE_IPS {
        if let Err(error) = run_command(
            "route",
            &[
                "add",
                &ip.to_string(),
                "mask",
                "255.255.255.255",
                &default_route.gateway.to_string(),
            ],
        ) {
            warn!(%error, dns = %ip, "failed to add direct DNS bypass route");
        }
    }

    for ip in PROXY_DNS_ROUTE_IPS {
        if let Err(error) = run_command(
            "route",
            &[
                "add",
                &ip.to_string(),
                "mask",
                "255.255.255.255",
                &TUN_ADDR.to_string(),
                "metric",
                "1",
            ],
        ) {
            warn!(%error, dns = %ip, "failed to add proxy DNS Tun route");
        }
    }
}

#[cfg(windows)]
fn cleanup_windows_dns_routes() {
    for ip in DIRECT_DNS_ROUTE_IPS
        .iter()
        .chain(PROXY_DNS_ROUTE_IPS.iter())
    {
        let _ = run_command("route", &["delete", &ip.to_string()]);
    }
}

#[cfg(windows)]
fn configure_dns(_interface_name: &str) -> bool {
    false
}

#[cfg(windows)]
struct WindowsDefaultRoute {
    gateway: Ipv4Addr,
}

#[cfg(windows)]
fn windows_default_route() -> Result<WindowsDefaultRoute, TunError> {
    let output = run_command("route", &["print", "-4", "0.0.0.0"])?;
    parse_windows_default_route(&output).ok_or(TunError::DefaultRouteNotFound)
}

#[cfg(windows)]
fn parse_windows_default_route(output: &str) -> Option<WindowsDefaultRoute> {
    for line in output.lines() {
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.len() < 5 || parts[0] != "0.0.0.0" || parts[1] != "0.0.0.0" {
            continue;
        }
        let gateway = parts[2].parse::<Ipv4Addr>().ok()?;
        if gateway == Ipv4Addr::UNSPECIFIED {
            continue;
        }
        return Some(WindowsDefaultRoute { gateway });
    }
    None
}

#[cfg(all(not(windows), not(target_os = "linux")))]
fn configure_routes(interface_name: &str, node_ip: IpAddr) -> Result<RouteGuard, TunError> {
    let _ = (interface_name, node_ip);
    Err(TunError::Command(
        "automatic Tun routing is only implemented for Windows and Linux".into(),
    ))
}

#[cfg(all(not(windows), not(target_os = "linux")))]
fn cleanup_routes(interface_name: &str, node_ip: Option<IpAddr>, dns_configured: bool) {
    let _ = (interface_name, node_ip, dns_configured);
}

#[cfg(all(not(windows), not(target_os = "linux")))]
fn configure_dns(_interface_name: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn parses_linux_default_route() {
        let route = super::parse_linux_default_route("default via 192.168.1.1 dev eth0 proto dhcp")
            .expect("route parsed");
        assert_eq!(route.gateway.to_string(), "192.168.1.1");
        assert_eq!(route.interface, "eth0");
    }

    #[test]
    fn extracts_dns_query_name() {
        let query = [
            0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, b'w',
            b'w', b'w', 0x07, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 0x03, b'c', b'o', b'm',
            0x00, 0x00, 0x01, 0x00, 0x01,
        ];

        assert_eq!(super::dns_query_name(&query).as_deref(), Some("www.example.com"));
    }

    #[cfg(windows)]
    #[test]
    fn parses_windows_default_route() {
        let route = super::parse_windows_default_route(
            "Network Destination        Netmask          Gateway       Interface  Metric\n\
             0.0.0.0          0.0.0.0      192.168.1.1   192.168.1.10     25",
        )
        .expect("route parsed");
        assert_eq!(route.gateway.to_string(), "192.168.1.1");
    }
}

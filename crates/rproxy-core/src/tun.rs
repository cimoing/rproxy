use std::{
    env, io,
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

use tracing::{info, warn};

use crate::config::{Config, NodeConfig};

const TUN_ADDR: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
#[cfg(target_os = "linux")]
const TUN_PREFIX: &str = "198.18.0.1/15";
const TUN_NETMASK: &str = "255.254.0.0";

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
    child: Option<Child>,
}

struct RouteGuard {
    interface_name: String,
    node_ip: Option<IpAddr>,
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

        let route_guard = if config.tun.auto_route {
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

        info!(
            interface = %interface_name,
            proxy = %proxy_uri,
            node = %active_node.server,
            "Tun mode started"
        );

        Ok(Some(Self {
            interface_name,
            route_guard,
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
        if self.route_guard.is_none() && self.child.is_none() {
            return;
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
        cleanup_routes(&self.interface_name, self.node_ip);
    }
}

fn socks_proxy_uri(listen: SocketAddr) -> String {
    format!("socks5://{listen}")
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

    run_command("ip", &["route", "add", "0.0.0.0/1", "dev", interface_name])?;
    run_command(
        "ip",
        &["route", "add", "128.0.0.0/1", "dev", interface_name],
    )?;

    Ok(RouteGuard {
        interface_name: interface_name.into(),
        node_ip: Some(node_ip),
    })
}

#[cfg(target_os = "linux")]
fn cleanup_routes(interface_name: &str, node_ip: Option<IpAddr>) {
    let _ = run_command("ip", &["route", "del", "0.0.0.0/1", "dev", interface_name]);
    let _ = run_command(
        "ip",
        &["route", "del", "128.0.0.0/1", "dev", interface_name],
    );
    if let Some(IpAddr::V4(ip)) = node_ip {
        let _ = run_command("ip", &["route", "del", &format!("{ip}/32")]);
    }
    let _ = run_command("ip", &["link", "del", "dev", interface_name]);
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
    })
}

#[cfg(windows)]
fn cleanup_routes(interface_name: &str, node_ip: Option<IpAddr>) {
    let _ = run_command("route", &["delete", "0.0.0.0", "mask", "128.0.0.0"]);
    let _ = run_command("route", &["delete", "128.0.0.0", "mask", "128.0.0.0"]);
    if let Some(IpAddr::V4(ip)) = node_ip {
        let _ = run_command("route", &["delete", &ip.to_string()]);
    }
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
fn cleanup_routes(interface_name: &str, node_ip: Option<IpAddr>) {
    let _ = (interface_name, node_ip);
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

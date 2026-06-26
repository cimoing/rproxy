use std::{
    env, fs, io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
};

const APP_NAME: &str = "RProxy";
const AUTOSTART_FILE: &str = "rproxy.desktop";

pub struct SystemProxy;

#[derive(Debug, Clone)]
pub struct SystemProxySnapshot {
    desktop: DesktopEnvironment,
    gnome: Option<GnomeProxySnapshot>,
    plasma_kioslaverc: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopEnvironment {
    Gnome,
    Plasma,
    Unsupported,
}

#[derive(Debug, Clone)]
struct GnomeProxySnapshot {
    mode: String,
    autoconfig_url: String,
    http_host: String,
    http_port: String,
    https_host: String,
    https_port: String,
}

impl SystemProxy {
    pub fn snapshot() -> io::Result<SystemProxySnapshot> {
        let desktop = detect_desktop();
        Ok(SystemProxySnapshot {
            desktop,
            gnome: (desktop == DesktopEnvironment::Gnome)
                .then(gnome_snapshot)
                .transpose()?,
            plasma_kioslaverc: (desktop == DesktopEnvironment::Plasma)
                .then(|| fs::read_to_string(kioslaverc_path()).ok())
                .flatten(),
        })
    }

    pub fn restore(snapshot: &SystemProxySnapshot) -> io::Result<()> {
        match snapshot.desktop {
            DesktopEnvironment::Gnome => {
                if let Some(gnome) = &snapshot.gnome {
                    restore_gnome(gnome)
                } else {
                    gsettings_set("org.gnome.system.proxy", "mode", "none")
                }
            }
            DesktopEnvironment::Plasma => restore_plasma(snapshot.plasma_kioslaverc.as_deref()),
            DesktopEnvironment::Unsupported => Ok(()),
        }
    }

    pub fn enable_http(addr: SocketAddr) -> io::Result<()> {
        match detect_desktop() {
            DesktopEnvironment::Gnome => enable_gnome_http(addr),
            DesktopEnvironment::Plasma => enable_plasma_http(addr),
            DesktopEnvironment::Unsupported => Err(unsupported_desktop_error()),
        }
    }

    pub fn enable_pac(url: &str) -> io::Result<()> {
        match detect_desktop() {
            DesktopEnvironment::Gnome => enable_gnome_pac(url),
            DesktopEnvironment::Plasma => enable_plasma_pac(url),
            DesktopEnvironment::Unsupported => Err(unsupported_desktop_error()),
        }
    }

    pub fn disable() -> io::Result<()> {
        match detect_desktop() {
            DesktopEnvironment::Gnome => gsettings_set("org.gnome.system.proxy", "mode", "none"),
            DesktopEnvironment::Plasma => disable_plasma(),
            DesktopEnvironment::Unsupported => Ok(()),
        }
    }
}

pub struct Autostart;

impl Autostart {
    pub fn set_enabled(enabled: bool) -> io::Result<()> {
        let path = autostart_path();
        if enabled {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let exe = env::current_exe()?;
            fs::write(path, desktop_entry(&exe))?;
        } else if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

fn detect_desktop() -> DesktopEnvironment {
    let value = [
        env::var("XDG_CURRENT_DESKTOP").ok(),
        env::var("XDG_SESSION_DESKTOP").ok(),
        env::var("DESKTOP_SESSION").ok(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(":")
    .to_ascii_lowercase();

    if value.contains("gnome") {
        DesktopEnvironment::Gnome
    } else if value.contains("kde") || value.contains("plasma") {
        DesktopEnvironment::Plasma
    } else {
        DesktopEnvironment::Unsupported
    }
}

fn gnome_snapshot() -> io::Result<GnomeProxySnapshot> {
    Ok(GnomeProxySnapshot {
        mode: gsettings_get("org.gnome.system.proxy", "mode")?,
        autoconfig_url: gsettings_get("org.gnome.system.proxy", "autoconfig-url")?,
        http_host: gsettings_get("org.gnome.system.proxy.http", "host")?,
        http_port: gsettings_get("org.gnome.system.proxy.http", "port")?,
        https_host: gsettings_get("org.gnome.system.proxy.https", "host")?,
        https_port: gsettings_get("org.gnome.system.proxy.https", "port")?,
    })
}

fn restore_gnome(snapshot: &GnomeProxySnapshot) -> io::Result<()> {
    gsettings_set("org.gnome.system.proxy", "mode", &snapshot.mode)?;
    gsettings_set(
        "org.gnome.system.proxy",
        "autoconfig-url",
        &snapshot.autoconfig_url,
    )?;
    gsettings_set("org.gnome.system.proxy.http", "host", &snapshot.http_host)?;
    gsettings_set("org.gnome.system.proxy.http", "port", &snapshot.http_port)?;
    gsettings_set("org.gnome.system.proxy.https", "host", &snapshot.https_host)?;
    gsettings_set("org.gnome.system.proxy.https", "port", &snapshot.https_port)?;
    Ok(())
}

fn enable_gnome_http(addr: SocketAddr) -> io::Result<()> {
    let host = socket_host(addr);
    let port = addr.port().to_string();
    gsettings_set("org.gnome.system.proxy.http", "host", &host)?;
    gsettings_set("org.gnome.system.proxy.http", "port", &port)?;
    gsettings_set("org.gnome.system.proxy.https", "host", &host)?;
    gsettings_set("org.gnome.system.proxy.https", "port", &port)?;
    gsettings_set("org.gnome.system.proxy", "mode", "manual")
}

fn enable_gnome_pac(url: &str) -> io::Result<()> {
    gsettings_set("org.gnome.system.proxy", "autoconfig-url", url)?;
    gsettings_set("org.gnome.system.proxy", "mode", "auto")
}

fn gsettings_get(schema: &str, key: &str) -> io::Result<String> {
    let output = Command::new("gsettings")
        .args(["get", schema, key])
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim()
            .trim_matches('\'')
            .to_string())
    } else {
        Err(command_error("gsettings get", &output.stderr))
    }
}

fn gsettings_set(schema: &str, key: &str, value: &str) -> io::Result<()> {
    let status = Command::new("gsettings")
        .args(["set", schema, key, value])
        .status()?;
    status.success().then_some(()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("gsettings set {schema} {key} failed"),
        )
    })
}

fn enable_plasma_http(addr: SocketAddr) -> io::Result<()> {
    let proxy = format!("http://{}:{}", socket_host(addr), addr.port());
    plasma_set("ProxyType", "1")?;
    plasma_set("httpProxy", &proxy)?;
    plasma_set("httpsProxy", &proxy)?;
    plasma_notify_proxy_changed();
    Ok(())
}

fn enable_plasma_pac(url: &str) -> io::Result<()> {
    plasma_set("ProxyType", "2")?;
    plasma_set("Proxy Config Script", url)?;
    plasma_notify_proxy_changed();
    Ok(())
}

fn disable_plasma() -> io::Result<()> {
    plasma_set("ProxyType", "0")?;
    plasma_notify_proxy_changed();
    Ok(())
}

fn restore_plasma(contents: Option<&str>) -> io::Result<()> {
    let path = kioslaverc_path();
    if let Some(contents) = contents {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
    } else if path.exists() {
        fs::remove_file(path)?;
    }
    plasma_notify_proxy_changed();
    Ok(())
}

fn plasma_set(key: &str, value: &str) -> io::Result<()> {
    let Some(binary) = find_command(&["kwriteconfig6", "kwriteconfig5", "kwriteconfig"]) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "kwriteconfig6/kwriteconfig5 was not found",
        ));
    };
    let status = Command::new(binary)
        .args([
            "--file",
            "kioslaverc",
            "--group",
            "Proxy Settings",
            "--key",
            key,
            value,
        ])
        .status()?;
    status.success().then_some(()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("failed to write Plasma proxy setting {key}"),
        )
    })
}

fn plasma_notify_proxy_changed() {
    let _ = Command::new("dbus-send")
        .args([
            "--session",
            "--type=signal",
            "/KIO/Scheduler",
            "org.kde.KIO.Scheduler.reparseSlaveConfiguration",
            "string:",
        ])
        .status();
}

fn find_command<'a>(names: &'a [&'a str]) -> Option<&'a str> {
    names
        .iter()
        .copied()
        .find(|name| Command::new(name).arg("--version").output().is_ok())
}

fn socket_host(addr: SocketAddr) -> String {
    match addr.ip() {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    }
}

fn kioslaverc_path() -> PathBuf {
    xdg_config_home().join("kioslaverc")
}

fn autostart_path() -> PathBuf {
    xdg_config_home().join("autostart").join(AUTOSTART_FILE)
}

fn xdg_config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={APP_NAME}\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        shell_escape_path(exe)
    )
}

fn shell_escape_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':' | '\\'))
    {
        value.into_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn unsupported_desktop_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "unsupported Linux desktop environment; GNOME and Plasma are supported",
    )
}

fn command_error(command: &str, stderr: &[u8]) -> io::Error {
    io::Error::new(
        io::ErrorKind::Other,
        format!(
            "{command} failed: {}",
            String::from_utf8_lossy(stderr).trim()
        ),
    )
}

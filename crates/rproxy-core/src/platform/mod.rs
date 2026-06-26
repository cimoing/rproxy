#[cfg(all(not(windows), not(target_os = "linux")))]
use std::net::SocketAddr;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::{Autostart, SystemProxy, SystemProxySnapshot};
#[cfg(windows)]
pub use windows::{Autostart, SystemProxy, SystemProxySnapshot, Tray};

#[cfg(all(not(windows), not(target_os = "linux")))]
pub struct SystemProxy;

#[cfg(all(not(windows), not(target_os = "linux")))]
#[derive(Debug, Clone)]
pub struct SystemProxySnapshot;

#[cfg(all(not(windows), not(target_os = "linux")))]
impl SystemProxy {
    pub fn snapshot() -> std::io::Result<SystemProxySnapshot> {
        Ok(SystemProxySnapshot)
    }

    pub fn restore(_snapshot: &SystemProxySnapshot) -> std::io::Result<()> {
        Ok(())
    }

    pub fn enable_http(_addr: SocketAddr) -> std::io::Result<()> {
        Ok(())
    }

    pub fn enable_pac(_url: &str) -> std::io::Result<()> {
        Ok(())
    }

    pub fn disable() -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub struct Autostart;

#[cfg(all(not(windows), not(target_os = "linux")))]
impl Autostart {
    pub fn set_enabled(_enabled: bool) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(not(windows))]
pub struct Tray;

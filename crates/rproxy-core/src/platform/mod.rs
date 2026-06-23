#[cfg(not(windows))]
use std::net::SocketAddr;

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{Autostart, SystemProxy, Tray};

#[cfg(not(windows))]
pub struct SystemProxy;

#[cfg(not(windows))]
impl SystemProxy {
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

#[cfg(not(windows))]
pub struct Autostart;

#[cfg(not(windows))]
impl Autostart {
    pub fn set_enabled(_enabled: bool) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(not(windows))]
pub struct Tray;

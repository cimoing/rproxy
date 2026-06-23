use std::{env, io, net::SocketAddr};

use winreg::{enums::*, RegKey};

const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP_NAME: &str = "RProxy";

pub struct SystemProxy;

impl SystemProxy {
    pub fn enable_http(addr: SocketAddr) -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (settings, _) = hkcu.create_subkey(INTERNET_SETTINGS)?;
        settings.set_value("ProxyEnable", &1_u32)?;
        settings.set_value("ProxyServer", &addr.to_string())?;
        settings.delete_value("AutoConfigURL").ok();
        Ok(())
    }

    pub fn enable_pac(url: &str) -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (settings, _) = hkcu.create_subkey(INTERNET_SETTINGS)?;
        settings.set_value("ProxyEnable", &0_u32)?;
        settings.set_value("AutoConfigURL", &url)?;
        Ok(())
    }

    pub fn disable() -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (settings, _) = hkcu.create_subkey(INTERNET_SETTINGS)?;
        settings.set_value("ProxyEnable", &0_u32)?;
        settings.delete_value("ProxyServer").ok();
        settings.delete_value("AutoConfigURL").ok();
        Ok(())
    }
}

pub struct Autostart;

impl Autostart {
    pub fn set_enabled(enabled: bool) -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (run, _) = hkcu.create_subkey(RUN_KEY)?;
        if enabled {
            let exe = env::current_exe()?;
            run.set_value(APP_NAME, &exe.to_string_lossy().to_string())?;
        } else {
            run.delete_value(APP_NAME).ok();
        }
        Ok(())
    }
}

pub struct Tray;

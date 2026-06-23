use std::{env, io, net::SocketAddr};

use winreg::{enums::*, RegKey};

const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP_NAME: &str = "RProxy";

pub struct SystemProxy;

#[derive(Debug, Clone)]
pub struct SystemProxySnapshot {
    proxy_enable: Option<u32>,
    proxy_server: Option<String>,
    auto_config_url: Option<String>,
}

impl SystemProxy {
    pub fn snapshot() -> io::Result<SystemProxySnapshot> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let settings = hkcu.open_subkey(INTERNET_SETTINGS)?;
        Ok(SystemProxySnapshot {
            proxy_enable: settings.get_value("ProxyEnable").ok(),
            proxy_server: settings.get_value("ProxyServer").ok(),
            auto_config_url: settings.get_value("AutoConfigURL").ok(),
        })
    }

    pub fn restore(snapshot: &SystemProxySnapshot) -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (settings, _) = hkcu.create_subkey(INTERNET_SETTINGS)?;
        set_optional_value(&settings, "ProxyEnable", snapshot.proxy_enable)?;
        set_optional_value(&settings, "ProxyServer", snapshot.proxy_server.as_deref())?;
        set_optional_value(
            &settings,
            "AutoConfigURL",
            snapshot.auto_config_url.as_deref(),
        )?;
        Ok(())
    }

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

fn set_optional_value<T: winreg::types::ToRegValue>(
    key: &RegKey,
    name: &str,
    value: Option<T>,
) -> io::Result<()> {
    if let Some(value) = value {
        key.set_value(name, &value)
    } else {
        key.delete_value(name).ok();
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

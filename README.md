# RProxy

RProxy is a cross-platform GUI proxy client built with Rust and Slint.

Current focus:

- Desktop GUI shell.
- YAML configuration.
- HTTP and SOCKS local listeners.
- HTTP, SOCKS, and VMess outbound node configuration.
- Automatic routing.
- Built-in geosite rule data.
- PAC generation and local PAC serving.
- Windows system proxy, tray, and autostart integration.
- Linux GNOME/Plasma system proxy, tray, and XDG autostart integration.
- Tun mode via a managed `tun2socks` process.

Linux tray support uses GTK and AppIndicator. Install the desktop development packages before
building on Linux, for example `libgtk-3-dev libxdo-dev libappindicator3-dev` on Debian/Ubuntu.

Tun mode requires `tun2socks` in `PATH`, or set `RPROXY_TUN2SOCKS` to the executable path. It also
requires administrator/root permissions to create the virtual interface and adjust routes.

## Run

```powershell
cargo run -p rproxy-gui
```

## Check

```powershell
cargo fmt
cargo check
```

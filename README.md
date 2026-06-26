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
- Linux GNOME/Plasma system proxy and XDG autostart integration.

## Run

```powershell
cargo run -p rproxy-gui
```

## Check

```powershell
cargo fmt
cargo check
```

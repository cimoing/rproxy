# RProxy

RProxy is a Windows-first GUI proxy client built with Rust and Slint.

Phase one focuses on:

- Windows GUI shell.
- YAML configuration.
- HTTP and SOCKS local listeners.
- VLESS node configuration with TLS and WebSocket options.
- Automatic routing.
- Built-in geosite rule data.
- PAC generation and local PAC serving.
- Windows system proxy, tray, and autostart integration.

## Run

```powershell
cargo run -p rproxy-gui
```

## Check

```powershell
cargo fmt
cargo check
```


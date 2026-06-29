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
- Tun mode via a statically linked `hev-socks5-tunnel` C library.

Linux tray support uses GTK and AppIndicator. Install the desktop development packages before
building on Linux, for example `libgtk-3-dev libxdo-dev libappindicator3-dev` on Debian/Ubuntu.

Tun mode builds a vendored `hev-socks5-tunnel` C static library at build time. Install `make` and a
C toolchain first; set `RPROXY_HEV_MAKE`, `RPROXY_HEV_CC`, or `RPROXY_HEV_AR` if those tools are not
named `make`, `gcc`, and `ar`. It also
requires administrator/root permissions to create the virtual interface and adjust routes.
On Windows, Hev uses Wintun at runtime; bundle
`crates/rproxy-core/vendor/hev-socks5-tunnel/third-part/wintun/bin/wintun.dll` next to the final
executable.

## Run

```powershell
cargo run -p rproxy-gui
```

## Check

```powershell
cargo fmt
cargo check
```

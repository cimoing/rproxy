# RProxy 配置文件说明

RProxy 使用 YAML 作为配置文件格式。默认示例位于 `examples/default.yaml`。

阶段一配置目标是描述一个可启动的 Windows 本地代理客户端，包括代理节点、本地监听地址、系统代理、PAC、路由规则和基础系统选项。

## 1. 完整示例

```yaml
profile:
  id: default
  name: Default
  enabled: true

nodes:
  - id: http-1
    name: Example HTTP
    protocol: http
    server: http.example.com
    port: 8080
    options:
      username: user
      password: pass

  - id: socks-1
    name: Example SOCKS
    protocol: socks
    server: socks.example.com
    port: 1080
    options:
      username: user
      password: pass

  - id: vless-1
    name: Example VLESS
    protocol: vless
    server: example.com
    port: 443
    options:
      uuid: 00000000-0000-0000-0000-000000000000
      tls: true
      transport: websocket
      websocket:
        path: /proxy
        host: example.com

proxy:
  http_listen: 127.0.0.1:7890
  socks_listen: 127.0.0.1:7891

system:
  tray: true
  auto_start: false

tun:
  enabled: false
  interface_name: rproxy-tun
  auto_route: true

pac:
  enabled: true
  listen: 127.0.0.1:7892

routing:
  mode: auto
  default_action: proxy
  geosite:
    enabled: true
    auto_update: false
  rules:
    - type: domain_suffix
      value: example.cn
      action: direct
    - type: ip_cidr
      value: 10.0.0.0/8
      action: direct
```

## 2. 顶层结构

配置文件包含以下顶层字段：

- `profile`：当前配置档案信息。
- `nodes`：代理节点列表。
- `proxy`：本地 HTTP 与 SOCKS 监听地址。
- `system`：系统托盘、开机自启动等系统集成设置。
- `tun`：Tun 模式设置。阶段一保留字段，阶段二实现。
- `pac`：PAC 服务设置。
- `routing`：自动路由规则设置。

## 3. profile

`profile` 描述当前配置档案。

```yaml
profile:
  id: default
  name: Default
  enabled: true
```

字段说明：

- `id`：配置唯一标识。必填，不应为空。
- `name`：配置显示名称。必填。
- `enabled`：配置是否启用。默认值为 `true`。

阶段一为单配置模式。阶段二会在多配置管理中使用 `id` 和 `name` 进行配置索引和切换。

## 4. nodes

`nodes` 是代理节点列表。至少需要配置一个节点。

通用字段：

- `id`：节点唯一标识。建议在配置内唯一。
- `name`：节点显示名称。
- `protocol`：节点协议。阶段一支持 `http`、`socks`、`vless`。
- `server`：远端服务器地址。必填。
- `port`：远端服务器端口。必填。
- `options`：协议相关参数。

阶段一默认使用列表中的第一个节点作为活动节点。后续会增加节点选择、延迟测试和健康检查。

### 4.1 HTTP 节点

```yaml
nodes:
  - id: http-1
    name: Example HTTP
    protocol: http
    server: http.example.com
    port: 8080
    options:
      username: user
      password: pass
```

字段说明：

- `protocol` 固定为 `http`。
- `username`：HTTP 代理认证用户名。可选。
- `password`：HTTP 代理认证密码。可选。

如果远端 HTTP 代理不需要认证，可以省略 `options`，或者只保留空对象。

```yaml
nodes:
  - id: http-1
    name: Example HTTP
    protocol: http
    server: http.example.com
    port: 8080
```

### 4.2 SOCKS 节点

```yaml
nodes:
  - id: socks-1
    name: Example SOCKS
    protocol: socks
    server: socks.example.com
    port: 1080
    options:
      username: user
      password: pass
```

字段说明：

- `protocol` 固定为 `socks`。
- `username`：SOCKS 认证用户名。可选。
- `password`：SOCKS 认证密码。可选。

阶段一协议名称统一使用 `socks`。后续如需区分 SOCKS4、SOCKS4a、SOCKS5，可扩展为独立字段。

### 4.3 VLESS 节点

```yaml
nodes:
  - id: vless-1
    name: Example VLESS
    protocol: vless
    server: example.com
    port: 443
    options:
      uuid: 00000000-0000-0000-0000-000000000000
      tls: true
      transport: websocket
      websocket:
        path: /proxy
        host: example.com
```

字段说明：

- `protocol` 固定为 `vless`。
- `uuid`：VLESS 用户 UUID。必填。
- `tls`：是否启用 TLS。阶段一支持 `true`。
- `transport`：传输方式。阶段一支持 `websocket`，也预留 `tcp`。
- `websocket.path`：WebSocket 请求路径。默认建议以 `/` 开头。
- `websocket.host`：WebSocket Host。可选，通常与节点域名一致。

校验规则：

- 当 `protocol` 为 `vless` 时，`options.uuid` 必填。
- 当 `transport` 为 `websocket` 时，`options.websocket` 必填。

## 5. proxy

`proxy` 配置本地代理监听地址。

```yaml
proxy:
  http_listen: 127.0.0.1:7890
  socks_listen: 127.0.0.1:7891
```

字段说明：

- `http_listen`：本地 HTTP 代理监听地址。
- `socks_listen`：本地 SOCKS 代理监听地址。

建议默认只监听 `127.0.0.1`，避免本地代理端口暴露到局域网或公网。

## 6. system

`system` 配置 Windows 系统集成能力。

```yaml
system:
  tray: true
  auto_start: false
```

字段说明：

- `tray`：是否启用系统托盘。默认值为 `true`。
- `auto_start`：是否开机自启动。默认值为 `false`。

阶段一以 Windows 为首批平台。开机自启动通过当前用户的启动项配置实现。

## 7. tun

`tun` 配置 Tun 模式。

```yaml
tun:
  enabled: false
  interface_name: rproxy-tun
  auto_route: true
```

字段说明：

- `enabled`：是否启用 Tun 模式。阶段一建议保持 `false`。
- `interface_name`：Tun 虚拟网卡名称。
- `auto_route`：是否自动配置路由。

Tun 模式属于第二阶段能力。阶段一保留配置字段，便于后续兼容。

## 8. pac

`pac` 配置 PAC 自动代理服务。

```yaml
pac:
  enabled: true
  listen: 127.0.0.1:7892
```

字段说明：

- `enabled`：是否启用 PAC 服务。默认值为 `true`。
- `listen`：PAC HTTP 服务监听地址。

启用后，RProxy 会根据路由规则生成 PAC 内容，并提供本地访问地址，例如：

```text
http://127.0.0.1:7892/proxy.pac
```

## 9. routing

`routing` 配置自动路由。

```yaml
routing:
  mode: auto
  default_action: proxy
  geosite:
    enabled: true
    auto_update: false
  rules:
    - type: domain_suffix
      value: example.cn
      action: direct
```

### 9.1 mode

`mode` 表示路由模式：

- `auto`：自动分流。按规则匹配，未命中时使用 `default_action`。
- `global_proxy`：全局代理。
- `global_direct`：全局直连。

### 9.2 default_action

`default_action` 表示默认处理动作：

- `proxy`：走代理。
- `direct`：直连。
- `block`：阻断。

### 9.3 geosite

```yaml
geosite:
  enabled: true
  auto_update: false
```

字段说明：

- `enabled`：是否启用内置 geosite 数据。
- `auto_update`：是否自动更新 geosite。阶段一固定建议为 `false`。

阶段一内置 geosite 种子数据，暂不支持在线更新。

### 9.4 rules

`rules` 是用户自定义路由规则列表。

规则字段：

- `type`：规则类型。
- `value`：规则值。
- `action`：匹配后的处理动作。

阶段一支持的规则类型：

- `domain`：完整域名匹配。
- `domain_suffix`：域名后缀匹配。
- `ip_cidr`：IP 段匹配。
- `port`：端口匹配。
- `geosite`：geosite 分类匹配。

规则动作：

- `proxy`：走代理。
- `direct`：直连。
- `block`：阻断。

示例：

```yaml
routing:
  mode: auto
  default_action: proxy
  rules:
    - type: domain
      value: intranet.example.com
      action: direct
    - type: domain_suffix
      value: example.cn
      action: direct
    - type: ip_cidr
      value: 10.0.0.0/8
      action: direct
    - type: geosite
      value: cn
      action: direct
```

## 10. 配置校验规则

当前实现包含以下校验：

- `profile.id` 不允许为空。
- `nodes` 至少需要一个节点。
- 每个节点的 `server` 不允许为空。
- VLESS 节点必须配置 `options.uuid`。
- VLESS WebSocket 节点必须配置 `options.websocket`。

建议额外遵守：

- 节点 `id` 在同一配置内保持唯一。
- 本地监听端口不要与其他程序冲突。
- 敏感信息不要提交到公开仓库。
- PAC 服务和本地代理默认监听 `127.0.0.1`。

## 11. 阶段一边界

阶段一配置文件已经覆盖 HTTP、SOCKS、VLESS、路由、PAC 和 Windows 系统集成字段。

当前已实现的网络协议能力：

- 本地 HTTP 入站支持 `CONNECT` 隧道。
- 本地 SOCKS5 入站支持 `CONNECT` 命令。
- HTTP 出站节点支持 `CONNECT` 隧道，可选 Basic 认证。
- SOCKS5 出站节点支持无认证和用户名密码认证。
- 路由动作支持按规则选择直连、代理或阻断。

当前仍属于后续阶段或待完善能力：

- Tun 模式真实流量接管。
- 多配置完整管理。
- 订阅格式导入。
- geosite 在线更新。
- 普通 HTTP 请求转发，也就是非 `CONNECT` 请求。
- VLESS 出站协议链路。
- 节点健康检查和延迟测试。
- 更多协议和传输方式。

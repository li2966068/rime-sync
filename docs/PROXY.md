# 代理支持（0.6.8）

同步程序支持通过 **HTTP / HTTPS / SOCKS5** 代理访问 WebDAV。
设计上**不做绕过列表（no-proxy bypass）**：一旦启用，连接测试、预演、同步、
备份、远端浏览、写权限探测等所有 WebDAV 请求都走同一个代理。

## 配置

`config.json`（Linux：`~/.local/share/rime-sync/config.json`；Windows：
`%APPDATA%\rime-sync\config.json`）新增 `proxy` 段：

```json
{
  "webdav": { "url": "https://dav.example.com/dav", "username": "alice" },
  "password_ref": "keyring",
  "proxy": {
    "enabled": true,
    "url": "http://127.0.0.1:7890",
    "username": "",
    "password_ref": "none"
  }
}
```

| 字段 | 说明 |
| --- | --- |
| `enabled` | 缺省 `false`。旧配置没有 `proxy` 段时按禁用处理（向后兼容）。 |
| `url` | `http://` / `https://` / `socks5://` / `socks5h://`；可省略端口（默认 80 / 443 / 1080）。不能带路径、查询参数，也不能把用户名密码写进 URL。 |
| `username` | 代理认证用户名，可留空。 |
| `password_ref` | 代理密码的存放位置：`keyring` / `file` / `none`，语义与 WebDAV 密码完全一致。密码本身**绝不写进 config.json**。 |

密码保存在密钥后端，account 固定为 `proxy-password`（与 WebDAV 用户名分开）。
桌面环境下写入系统钥匙串；没有 Secret Service 的 headless 环境回落到
`secrets.json`（0600 权限）。

`socks5h` 与 `socks5` 的区别：`socks5h` 让代理端解析域名（适合代理在另一台机器、
本机没有 DNS 的情况）；`socks5` 本地解析后再连 IP。

### 常见代理软件端口

| 客户端 | 协议 | 地址 |
| --- | --- | --- |
| Clash / Clash Verge / Mihomo | HTTP | `http://127.0.0.1:7890` |
| Clash 混合端口（HTTP + SOCKS5） | 混合 | `http://127.0.0.1:7890` 或 `socks5h://127.0.0.1:7890` |
| v2rayN（默认） | SOCKS5 | `socks5h://127.0.0.1:10808` |
| v2rayN（http 入站） | HTTP | `http://127.0.0.1:10809` |
| sing-box | 混合 | `http://127.0.0.1:2080` |

按自己客户端里实际显示的端口填写；填错会得到「代理认证失败 / 连接失败」这类可读错误。

## 命令行

```bash
# 本次走代理（覆盖配置文件，不落盘）
rime-sync-cli --proxy http://127.0.0.1:7890 test-conn

# socks5 + 认证（用户名/密码来自配置里的 proxy 段与密钥后端）
rime-sync-cli --proxy socks5h://127.0.0.1:10808 transport

# 本次直连（忽略配置文件里的 proxy.enabled）
rime-sync-cli --no-proxy status

# 两个参数互斥
rime-sync-cli --proxy http://127.0.0.1:7890 --no-proxy test-conn   # 报错退出
```

- `--proxy <url>` 只覆盖 `proxy.url` 并把 `enabled` 置为 true；用户名/密码仍取配置文件与密钥后端。
- `--no-proxy` 把 `enabled` 置为 false（配置文件里的地址保留，不删除）。
- 地址非法时命令直接失败，**不会把非法值写进配置文件**。
- 代理认证失败会明确报「代理认证失败（HTTP 407）……」；代理连不上会提示
  「已启用代理 …：请确认代理在运行、地址与认证正确」，避免被误认成 WebDAV 服务器故障。

## 图形界面

「连接」页 WebDAV 区域下方的「代理」小节：

1. 勾选「启用代理」，填写地址（如 `http://127.0.0.1:7890`）。
2. 需要认证时填用户名与密码；密码点「保存配置」后写入系统钥匙串。
3. 「测试连接」会连同代理一起测试（走代理执行 PROPFIND 与写权限探测）。
4. 保存时会校验地址：scheme 白名单、端口范围、不允许路径/查询参数/内嵌凭据。

## 环境变量

未启用配置代理时，保持 reqwest 的默认行为：`HTTP_PROXY` / `HTTPS_PROXY` /
`ALL_PROXY`（以及 `NO_PROXY`）仍然生效，可作为兜底。

启用配置代理后，构建 reqwest 客户端时会显式加入该代理；此时是否仍受
`NO_PROXY` 影响取决于 reqwest 的实现（当前版本对显式代理不套用 NO_PROXY），
也就是说本项目本身仍然没有绕过列表。

## 自动更新

更新检查与下载使用同一份 `proxy` 配置（经 `UpdaterBuilder::proxy` 注入）：

- `http://` / `https://` 代理（含用户名密码）可用；
- `socks5://` / `socks5h://` **不支持**：插件内置的 reqwest 没有编译 socks
  feature，此时会记录一条日志并按直连处理。

## 已知限制

- 更新检查的 socks 代理回退（见上）——网络只能通过 socks 出去时，检查更新
  可能仍然失败，请手动下载安装包升级。
- **安装器 / 打包流程不走代理**：deb / AppImage / NSIS 安装与下载由系统或
  Tauri bundler 完成，与同步程序无关。
- 只有 WebDAV 请求走代理；本地文件读写、librime 触发与版本库操作不经过网络。
- 代理密码只支持一个固定 account（`proxy-password`），不支持按地址区分多套凭据。

## 排错

| 现象 | 处理 |
| --- | --- |
| `unsupported proxy scheme` | 地址必须以 `http://`、`https://`、`socks5://` 或 `socks5h://` 开头。 |
| `代理认证失败（HTTP 407）` | 代理用户名/密码不对；注意代理密码存在密钥后端，换用户名后需要重新输入。 |
| 提示 `认证失败（HTTP 401）` 但 WebDAV 账号没问题 | 有些代理（如 tinyproxy）对错误的代理凭据返回 401 而不是 407；错误信息会同时提示「已启用代理 …」。 |
| 连接超时且提示「已启用代理」 | 代理进程没起、端口不对，或代理被系统防火墙拦截。 |
| 提示 `socks5 is not supported` | 构建时漏了 reqwest 的 `socks` feature（根 `Cargo.toml` 已默认开启）。 |

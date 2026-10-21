# vendor/ — 离线依赖 .deb（不入库）

这个目录由 `../fetch-vendor-debs.sh` 生成，打包时被
`src-tauri/tauri.conf.json` 的 `bundle.linux.deb.files` 原样映射到
`/usr/share/rime-sync/vendor/`，安装期的 postinst 在没有网络时用
`dpkg -i` 装它们。

    packaging/linux/fetch-vendor-debs.sh              # 默认 ubuntu:22.04
    APT_MIRROR=http://mirrors.aliyun.com/ubuntu \
      packaging/linux/fetch-vendor-debs.sh            # 墙内换源（必须 http）

生成物（都被 .gitignore 忽略，不进仓库）：

| 文件 | 说明 |
| --- | --- |
| `*.deb` | fcitx5 / fcitx5-rime / librime-data 及其完整依赖闭包（22.04 为 82 个包，约 28 MB） |
| `VENDOR-INFO` | 发行版、VERSION_ID、生成时间、包版本清单；安装期与 /etc/os-release 比对 |
| `SHA256SUMS` | 安装期 `sha256sum -c` 校验 |

注意：

- 仓库里只保留本 README，所以没跑过 fetch 的机器也能 `tauri build`（deb 里只有
  README，postinst 会发现没有 .deb 并打印手工安装指引，安装不会失败）。
- 发布流水线必须在打包前先跑一次 fetch 脚本，否则打出来的 deb 不具备离线能力。
- 只抓过 ubuntu:22.04。Ubuntu 24.04 / Debian 的包名与版本不同（如 librime1t64），
  需要另跑一次 fetch 并替换 vendor 内容；多发行版并存要改成按目录分
  （当前 VENDOR-INFO 只描述一套）。

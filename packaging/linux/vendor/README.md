# vendor/ — 按发行版分目录的离线依赖闭包（不入库）

由 `scripts/ime/fetch-vendor-deps.sh` 生成，按发行版分目录：

```
vendor/ubuntu-22.04/{*.deb,VENDOR-INFO,SHA256SUMS,DEPS.txt}
vendor/ubuntu-24.04/...
vendor/debian-12/...
vendor/debian-13/...
```

每个目录是该发行版上 `rime-inputmethod` + `rime-sync` 两个 deb 的运行期依赖闭包
（apt 解析、`--no-install-recommends`）。故意排除：

| 包 | 原因 |
| --- | --- |
| librime1 / librime1t64 | rime-inputmethod 自带 librime，同名 so 会冲突 |
| librime-data | rime-inputmethod 自带 /usr/share/rime-data |
| fcitx5-rime | rime-inputmethod Provides/Conflicts 它 |

`fcitx5`（守护进程）**包含**在闭包里：没有它 addon 无处加载，离线安装就不算
可用的输入法。若目标机已确定装有 fcitx5，可用 `ROOT_PKGS=""` 重新抓取以缩小体积。

这些 .deb 由构建流程生成、随离线安装包分发，不提交进仓库（见
`packaging/linux/.gitignore`）。旧的单层 `vendor/*.deb`（ubuntu 22.04 的
fcitx5-rime 闭包）会与新的 rime-inputmethod 冲突，已废弃。

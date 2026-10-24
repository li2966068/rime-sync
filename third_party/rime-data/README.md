# Rime 方案数据（vendored）

这些文件被打包进 `rime-inputmethod` deb（Linux，装到 `/usr/share/rime-data`）与自编译的
小狼毫安装包（Windows，装到 `data\`），使输入法安装后无需联网即可 deploy 并输入中文。

| 目录 | 上游仓库 | 提交（master 快照） | 许可证 | 文件数 | 字节数 |
| --- | --- | --- | --- | --- | --- |
| rime-prelude | <https://github.com/rime/rime-prelude> | `082425ea0684bca36474415d4a0e8db9b016487e` | LGPL-3.0 | 7 | 43,963 |
| rime-essay | <https://github.com/rime/rime-essay> | `054920de4f54c9e5994276a96a4fc2a35cb51aa3` | LGPL-3.0 | 5 | 5,895,745 |
| rime-luna-pinyin | <https://github.com/rime/rime-luna-pinyin> | `56b934b099dfbeab842320f13aa8b461a6ab3e42` | LGPL-3.0 | 10 | 907,752 |
| rime-cangjie | <https://github.com/rime/rime-cangjie> | `52d90a1b1312e74042b38c1cbc8142defbc53171` | LGPL-3.0 | 10 | 811,576 |
| rime-stroke | <https://github.com/rime/rime-stroke> | `1e8fff9b9494ddec23b0cbc526bcfd8171a6fd48` | LGPL-3.0 | 5 | 3,407,589 |
| rime-terra-pinyin | <https://github.com/rime/rime-terra-pinyin> | `723e51bc266cf9464530c1ddedb856aa18e3da34` | LGPL-3.0 | 6 | 1,814,794 |
| rime-bopomofo | <https://github.com/rime/rime-bopomofo> | `6085c9a38a4a728047862b33d67eee18aa86f3b9` | LGPL-3.0 | 8 | 28,186 |
| rime-wubi | <https://github.com/rime/rime-wubi> | `152a0d3f3efe40cae216d1e3b338242446848d07` | LGPL-3.0 | 7 | 2,302,709 |
| rime-pinyin-simp | <https://github.com/rime/rime-pinyin-simp> | `0c6861ef7420ee780270ca6d993d18d4101049d0` | Apache-2.0 | 4 | 1,279,668 |

合计 62 个文件、16,491,982 字节；其中 2026-10-24 新增的 6 个 recipe 共 40 个文件、
9,644,522 字节（约 9.20 MiB）。新增仓库都以 `master` 分支 zip 取得，提交号是同一天的
`git ls-remote` 结果。

## 打包规则

- 每个 recipe 目录只保留 `*.yaml`、`*.txt`、`LICENSE`、`AUTHORS`、`README.md`；
  上游的 `Makefile`、`scripts/`、`check.py`、`.github/` 等一律不入库。
- 三个构建入口都把**所有含 `*.yaml` / `*.txt` 的 recipe 目录**平铺到目标目录
  （`rime-essay` 只有 `essay.txt`）：
  `scripts/ime/build-weasel.ps1`（Windows `output\data`）、
  `scripts/ime/container-build.sh`（容器内 `/usr/share/rime-data`）、
  `scripts/ime/package-ime-deb.sh`（deb 的 `/usr/share/rime-data`）。
  新增方案只需在这里加一个目录，脚本不用改。
- `rime-prelude/default.yaml` 的 `schema_list` 只列随包方案：Windows 构建缺一个就 throw，
  deb 打包用 shell 断言。当前 16 个：luna_pinyin（+simp/fluency/tw/quanpin）、
  wubi86（+wubi_pinyin/wubi_trad）、cangjie5（+express）、stroke、
  terra_pinyin（+terra_pinyin_12345）、bopomofo（+tw/express）。
- `rime-bopomofo/detenele.schema.yaml`（实验方案「動態能力注音」）随仓库入库，但**不**列入
  `schema_list`，因此不会被部署。
- 官方小狼毫 0.17.4 的 data 不含五笔；`rime-wubi` 是本项目额外加入的。
- `rime-pinyin-simp`（`pinyin_simp`，袖珍简化字拼音）随文件入库，但**不列入** `schema_list`：
  它是 wubi86 / wubi_pinyin / wubi_trad 的 `dependencies`，librime 部署时会自动构建
  （实测生成 `pinyin_simp.table.bin` / `.reverse.bin`），因此五笔的拼音反查可用
  （wubi86 敲 `znihao` 出「你好」）。它的 LICENSE 是 Apache-2.0，与其他 recipe 不同。

## 许可证

9 个 recipe 中 8 个为 LGPL-3.0（LICENSE 文本一致，SHA-256
`da7eabb7bafdf7d3ae5e9f223aa5bdc1eece45ac569dc21b3b037520b4464768`），
`rime-pinyin-simp` 为 Apache-2.0（`LICENSE` SHA-256
`cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`）。
deb 会把每个 recipe 的 `LICENSE` 装到
`/usr/share/doc/rime-inputmethod/copyright.<repo>`；来源与提交见上表，
完整说明另见 `docs/THIRD-PARTY.md`。

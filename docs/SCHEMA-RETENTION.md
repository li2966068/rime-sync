# 随包输入方案：保留清单与清理（0.6.21）

## 保留清单

随安装包只提供 3 个方案（顺序就是写入用户目录的 `default.custom.yaml` 的
`schema_list` 顺序）：

| 方案 id | 名称 | 说明 |
|---|---|---|
| `luna_pinyin_simp` | 朙月拼音·简化字 | 默认方案；`__include: luna_pinyin.schema:/` |
| `wubi_pinyin` | 五笔拼音 | `dictionary: wubi86`，`dependencies: [pinyin_simp]` |
| `wubi86` | 五笔86 | `dependencies: [pinyin_simp]` |

**单一真相源**：`packaging/rime/retained-schemas.txt`（一行一个 id，`#` 注释）。

读取它的地方：

| 位置 | 用途 |
|---|---|
| `crates/core/src/prune.rs` | `include_str!` 编译进 `rime-sync-cli` / 应用，安装与自举时清理 |
| `scripts/ime/rime_common.py` | `gen-schema-custom.py` / `gen-prebuilt-build.py` / `gen-rime-data.py` 的清单与依赖闭包 |
| `packaging/linux/postinst`、`install.sh` | 不抄列表，直接调 `rime-sync-cli prune-schemas`（root 期） |
| `packaging/nsis-hooks.nsh` | POSTINSTALL 调 `rime-sync-cli.exe prune-schemas` |
| `packaging/windows/check-nsis-hooks.sh` | 断言模板目录与清单逐项一致、且正好 3 个 |
| `scripts/ime/package-ime-deb.sh` | 断言随包模板 3 个、非保留方案的 data 不在 deb 里 |

> 依赖方案不需要写进清单。闭包由 schema 的 `__include: <base>.schema:/`、
> `dependencies:` 与 `dictionary:` 自动解析，当前得到
> `luna_pinyin`（luna_pinyin_simp 的基类）、`pinyin_simp`、`stroke`。

## 清理什么

安装（deb postinst / `.run` install.sh / NSIS POSTINSTALL）与每次启动自举都会跑
`rime-sync-cli prune-schemas`，删除**不在保留集合**（清单 + 依赖闭包）里的：

1. **data 目录**（Linux `/usr/share/rime-data`，Windows `<WeaselRoot>\data`）：
   `<schema>.schema.yaml` 与配套的 `<schema>.prism.bin` / `<schema>.table.bin` /
   `<schema>.reverse.bin`；以及 `default.yaml` 里指向已删方案的 `schema_list` 条目
   （不裁这个，下一次 librime 部署会因「schema 不存在」整次失败）。
2. **用户目录**（`%APPDATA%\Rime` / `~/.local/share/fcitx5/rime`）：
   `<schema>.custom.yaml`（`default` / `weasel` / `squirrel` / `ibus_rime` /
   `fcitx5_rime` 这几个不是方案模板，永不删），以及 `default.custom.yaml` 的
   `schema_list` 条目。
3. **build 目录**：`<schema>.schema.yaml` 与 `*.prism.bin` / `*.table.bin` /
   `*.reverse.bin`。只删能确认归属（同目录有同名 schema 文件或在 data 目录登记过）
   的产物，避免误删 `luna_pinyin.extended.table.bin` 这类码表。

## 备份与回滚

* 每个被删文件同目录改名为 `<原文件名>.rimesync-removed-<时间戳>`
  （时间戳 = Unix 秒；同一秒重跑会追加 `.1`、`.2`，不覆盖旧备份）。
* 重写 `default.yaml` / `default.custom.yaml` 前先复制一份同样的备份，再原地改写。
* 报告字段（`--json`）：`removed` / `backups` / `failures` / `keep` /
  `timestamp` / `disabled` / `dry_run`。

回滚某个方案（示例：terra_pinyin 的 data 方案文件）：

```bash
sudo mv /usr/share/rime-data/terra_pinyin.schema.yaml.rimesync-removed-* \
        /usr/share/rime-data/terra_pinyin.schema.yaml
```

Windows 对应 `<WeaselRoot>\data\terra_pinyin.schema.yaml.rimesync-removed-*`。

## 开关

* `RIMESYNC_PRUNE_SCHEMAS=0`（也认 `no` / `off` / `false`）：关闭清理，
  `prune-schemas` 只输出 `disabled=true`。
* `rime-sync-cli prune-schemas --dry-run`：只报告将删除什么。
* 排错时可用 `--data-dir DIR` / `--user-dir DIR` 指定临时目录（不碰真实数据）。
* 清理失败（例如 Linux 上以普通用户跑、`/usr/share/rime-data` 属 root）只记
  `failures` 并提示「请以管理员身份重试」，不会中断安装。

## 打包侧（同一份清单的产物）

* **deb**：`/usr/share/rime-data` 只装 3 个方案 + 依赖闭包（
  `scripts/ime/gen-rime-data.py` 生成，`default.yaml` 的 `schema_list` 同步裁剪）；
  `/usr/share/rime-sync/defaults/schemas/` 只有 3 个 `<schema>.custom.yaml`；
  `/usr/share/rime-sync/retained-schemas.txt` 随包（排错/审计用）。
* **预编译产物**：`packaging/rime/prebuilt` 由 `gen-prebuilt-build.py` 生成，
  `build/` 只含保留方案 + 依赖方案的产物，`config/` 只含 3 个模板，
  `MANIFEST.json` 的 `schemas` / `retained_schemas` / `dependencies` 记录同一套。
* **Windows**：`resources/schemas/` 只放 3 个模板，`resources/retained-schemas.txt`
  随包；POSTINSTALL 在部署自检之前清理，避免部署把删掉的方案又建回来。

## 验证

```bash
scripts/ime/gen-schema-custom.py --check
scripts/ime/gen-prebuilt-build.py --check
bash packaging/windows/check-nsis-hooks.sh --strict
cargo test -p rime-sync-core --lib prune
cargo test -p rime-sync-cli --test prune_schemas
```

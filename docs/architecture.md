# rime-sync 架构（引擎层）

本文只描述 crates/core 与 crates/cli；Tauri 薄壳、前端与打包由其他模块负责。

## 模块职责

| 模块 | 职责 |
|---|---|
| config | config.json 读写（0600）、环境变量/CLI 覆盖、device_id 解析与生成 |
| rime_dir | Rime 用户目录探测（参数 > RIME_USER_DATA_DIR > 平台已知路径）、installation.yaml 解析、sync_dir 解析 |
| webdav | reqwest 实现的最小 WebDAV 客户端：PROPFIND/GET/PUT/DELETE/MKCOL/HEAD，Basic Auth，30s 超时，3 次指数退避，ETag + If-Match，无 ETag 时 size+mtime 降级 |
| plan | 文件分类、shared/snapshot 路径形状校验、硬红线断言、路径穿越防护、glob |
| merge3 | 文件级三向合并决策（基线/本地/远端），内容版与摘要版 |
| index | index.json（本地基线）与 shared/manifest.json（远端清单）共用的 FileIndex |
| platform | RimeTrigger trait：Linux 调 rime_dict_manager/rime_deployer，Windows 调 WeaselDeployer，FakeTrigger 供测试 |
| engine | transport（只搬文件）与 apply（transport + 触发 librime + 再 push），进度回调与 Report |

## 远端布局

```
<remote_dir>/
├─ sync/<device_id>/owner.json
├─ sync/<device_id>/<dict>.userdb.txt
└─ shared/manifest.json
   shared/files/<相对 rime 根目录的路径>
```

本地状态放在 config.json 同目录：`index.json`（shared 基线 + 已上传快照摘要）。

## transport 流程（永远 pull → merge → push）

1. 确保 `<remote_dir>`、`sync/<own>`、`shared/files` 存在（逐级 MKCOL）。
2. 写 `sync/<own>/owner.json`（心跳/会话，不参与文件计数）。
3. pull：PROPFIND `sync`，对每个其他设备目录只接受 `*.userdb.txt`，路径过
   `assert_remote_snapshot_path`，写入 `<rime_dir>/sync/<device>/`；`owner.json` 静默跳过。
4. merge/push shared：GET manifest，与本地 index、本地文件做三向决策。
5. push：扫描 `<rime_dir>/sync/<own>/`，只上传 `*.userdb.txt`；index 中摘要一致且
   远端未变（ETag 或 size+mtime）时跳过。

## apply 流程

transport（合并到本地）→ stop IME（Linux 可选）→ `rime_dict_manager --sync` →
`rime_deployer --build <rime_dir> <shared_data_dir>` → start IME → 再次 push 新快照。
任何一步失败都会恢复输入法并写回 Report.error。

## 硬红线（代码闸门 + 单测）

- `*.userdb/` 目录（任何层级）禁止读写上传：`plan::assert_no_userdb_dir`。
- shared 树只允许 CustomYaml/DictSource：`plan::assert_shared_allowed` 会拒绝
  `*.userdb.txt`、`installation.yaml`、`build/`、引擎产物。
- `installation.yaml` 不上传：分类为 Private 且 `assert_upload_allowed` 拒绝。
- 远端 href / manifest key / device_id 全部过 `plan::validate_rel_path`；`..`、
  绝对路径、盘符、反斜杠一律拒绝（percent-decode 之后再校验）。

## 设计取舍 / 假设

- 三向合并是**文件级**取舍，不做行级合并：Rime YAML 行级合并后仍可能语义冲突。
  冲突时保留本地并写 `<name>.conflict-<device>-<ts>`，同时上报 conflicts，不覆盖远端。
- index 只存哈希，不存基线内容，因此合并决策用摘要比较（`merge3_digests`）；
  内容版 `merge3` 用于单测与未来扩展。
- 本地删除会传播为远端删除；远端删除且本地未改则删本地，本地改过则保留并告警。
- device_id 优先级：config > installation.yaml > `<hostname>-<6hex>` 并写回。
- CLI 的本地状态目录 = config.json 所在目录；同机模拟两台设备时要用不同目录。
- Windows 触发器按 `%ProgramFiles%\Rime\*\WeaselDeployer.exe` 查找，取不到时
  apply 报错并提示仅 transport 可用；Linux 用 PATH 中的 librime-bin 命令。
- 真实 librime 合并、Weasel IPC 未在本机验证的部分由 FakeTrigger 单测覆盖流程顺序。

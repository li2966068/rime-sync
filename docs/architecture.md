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
| engine | transport（只搬文件）与 apply（备份 + transport + 触发 librime + 再 push），进度回调与 Report |
| backup | BackupManager：apply 前快照、manifest（sha256/size/reason/app_version）、restore 校验后写回、prune 保留 10 份 |
| secret | SecretStore trait：KeyringStore（keyring crate）+ FileStore（0600）兜底，detect() 首选钥匙串并探测可用性 |
| lock | 同步锁（pid/hostname/started_at/boot_id），/proc 判活 + 同 pid started_at 校验 + 6h 兜底超时，RAII LockGuard |
| watch | notify 递归监听 + 去抖 + 写盘静音窗口，只发事件不直接跑 engine |
| presets | 服务商预设（AList/坚果云/Nextcloud/群晖/自定义）：默认路径、URL 归一化、响应头识别 |

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

拿锁 → **备份**（失败即中止，一个字都不写）→ transport（合并到本地）→ stop IME
（Linux 可选）→ `rime_dict_manager --sync` → `rime_deployer --build <rime_dir>
<shared_data_dir>` → start IME → 再次 push 新快照。任何一步失败都会恢复输入法并写回
Report.error；Report.backup_id 记录本次 apply 前生成的备份。

transport 与 apply 都持同一把锁（`<state_dir>/sync.lock`）：并发的第二次调用会拿到
"another sync is running"，而不是两个进程同时写 rime 目录。

## 备份与回滚（backup）

`<state_dir>/backups/<YYYYmmdd-HHMMSS>/files/<相对路径>` + `manifest.json`。
备份范围：`<rime_dir>/sync/**/*.userdb.txt`（递归）、根目录下 `*.custom.yaml`、
`*.dict.yaml`、`*.txt`、`user.yaml`。manifest 记录 rel/sha256/size、reason、
created_at 与 app_version。

`restore(id)` 先逐文件校验 sha256 与 size，任何一个不匹配就整体中止（不写回）；
校验通过后先自动创建一份 `pre-restore` 备份，再把文件覆盖回 rime 目录。`prune(keep)`
按 id 倒序保留最新 10 份。

## 密码存储（secret）

config.json 只保留 `password_ref`（keyring / file / none），**不再写入明文 password**。
`detect()` 先对钥匙串做一次真实的写-读-删探测（避免"Entry 能创建但 Secret Service
不存在"的假可用），失败则回落到 `<state_dir>/secrets.json`（0600）。headless 环境可用
`RIME_SYNC_SECRET_BACKEND=file` 显式强制文件后端。

旧配置里读到明文 `password` 时，CLI 会把它写进后端、把 `password_ref` 改为对应值，
然后重写一份不含 password 的 JSON。`--dav-pass` 与环境变量只在内存中使用，不落盘。

## 残留锁回收（lock）

锁文件内容是 `{pid, hostname, started_at, boot_id}`。以下任一条件成立即判 stale 并
接管（记录 warn 日志后删除锁文件重试）：

1. `boot_id` 与当前系统不一致（机器重启过）；
2. 锁存在超过 6 小时（兜底，覆盖无法判定的平台）；
3. 同 hostname 下 `/proc/<pid>` 不存在；
4. 同 hostname 下 pid 存活但 `/proc/<pid>/stat` 推出的启动时刻与记录值相差 > 2s
   （pid 被复用）。

不同 hostname 的锁无法验 pid，只能靠 6h 兜底。`Drop` 只删除仍是自己写的那份锁，
失败只 warn 不 panic。

## 段落级 YAML 合并（merge3::merge_yaml_patch）

只对 `*.custom.yaml` 尝试：解析 base/local/remote，对顶层 `patch:` 映射做 key 级三向
合并。一侧相对基线未变的 key 取另一侧（含删除），两侧都变且不同 → 保留本地并记录
`patch.<key>` 冲突（引擎继续按原命名生成 `*.conflict-<device>-<ts>` 副本）。
任何解析失败、缺少 `patch:` 映射、远端不存在等情况一律返回 `Fallback`，由调用方静默
回落文件级合并——绝不中断同步。

基线内容来自 `<state_dir>/base/<sha256>`（内容寻址缓存，只缓存 *.custom.yaml）；
缓存缺失时按"无基线"保守处理：同 key 双改判冲突。

## 事件驱动（watch）

`watch::start(rime_dir, debounce)` 起一个后台线程持有 notify watcher，事件先做路径过滤
（build/、*.userdb/、.rime-sync/、sync/、*.bin、*.gram、临时文件全部忽略），再按
debounce 窗口合并成一条 WatchEvent 发给调用方。

调用方（CLI `watch`）在跑 transport 之前调用 `mute(15s)`：静音窗口内的事件直接丢弃，
避免"engine 写文件 → watcher 触发 → 再 transport"的自触发死循环。watch 只发事件、
不自己调 engine。

## 服务商预设与写权限探测（presets / webdav::check_write）

`Preset` 提供 default_path（/dav、/dav、/remote.php/dav/、/webdav）、`normalize()`
（补 scheme、去尾斜杠、AList 补 5244、补默认路径）与 `detect()`（Server 含 alist，或
/dav 路径 + Basic 认证）。`check_write()` 依次 MKCOL 临时目录 → PUT 1 字节 → DELETE
清理，失败时把状态码翻译成中文可执行建议（403+AList 指向 Webdav Read / Webdav
Manage 权限）。MKCOL 的 405 在 RFC 里既可能是"已存在"也可能是"不允许"，因此失败提示
会单独给出"只读挂载或权限不足"的判断。

## 硬红线（代码闸门 + 单测）

- `*.userdb/` 目录（任何层级）禁止读写上传：`plan::assert_no_userdb_dir`。
- shared 树只允许 CustomYaml/DictSource：`plan::assert_shared_allowed` 会拒绝
  `*.userdb.txt`、`installation.yaml`、`build/`、引擎产物。
- `installation.yaml` 不上传：分类为 Private 且 `assert_upload_allowed` 拒绝。
- 远端 href / manifest key / device_id 全部过 `plan::validate_rel_path`；`..`、
  绝对路径、盘符、反斜杠一律拒绝（percent-decode 之后再校验）。

## 设计取舍 / 假设

- 三向合并默认是**文件级**取舍：Rime YAML 行级合并后仍可能语义冲突。冲突时保留本地并
  写 `<name>.conflict-<device>-<ts>`，同时上报 conflicts，不覆盖远端。
  唯一的例外是 `*.custom.yaml` 的 `patch:` 段落级合并（见上），且失败即静默回落。
- index 只存哈希，不存基线内容，因此合并决策用摘要比较（`merge3_digests`）；
  内容版 `merge3` 用于单测与未来扩展。
- 本地删除会传播为远端删除；远端删除且本地未改则删本地，本地改过则保留并告警。
- device_id 优先级：config > installation.yaml > `<hostname>-<6hex>` 并写回。
- CLI 的本地状态目录 = config.json 所在目录；同机模拟两台设备时要用不同目录。
- Windows 触发器按 `%ProgramFiles%\Rime\*\WeaselDeployer.exe` 查找，取不到时
  apply 报错并提示仅 transport 可用；Linux 用 PATH 中的 librime-bin 命令。
- 真实 librime 合并、Weasel IPC 未在本机验证的部分由 FakeTrigger 单测覆盖流程顺序。
- 钥匙串在 headless/无 Secret Service 时的回落路径由 `detect_with` + 失败后端单测
  覆盖；本机若存在 Secret Service，CLI 默认走 Keyring，E2E 用
  `RIME_SYNC_SECRET_BACKEND=file` 固定为文件后端。
- 锁的 pid 判活只有 Linux 走 `/proc`；Windows 分支只依赖 6h 兜底（未在 Windows 编译验证）。
- watch 依赖 inotify（Linux）语义；网络文件系统上可能收不到事件，此时仍可手动触发。

## CLI 命令

`detect` / `test-conn` / `status`（含锁状态与后端）/ `transport` / `apply` /
`backup [--reason]` / `backups` / `rollback <id>` / `watch [--debounce-secs]` /
`probe`（写权限探测）。全部支持 `--json`；`--preset` 会在建连前归一化 `--dav-url`。
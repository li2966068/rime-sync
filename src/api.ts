import { invoke } from "@tauri-apps/api/core";

export interface WebDavConfig {
  url: string;
  username: string;
  password: string;
  remote_dir: string;
}

/** 出站代理（0.6.8）：不做绕过列表，启用后所有 WebDAV 请求统一走它。 */
export interface ProxyConfig {
  enabled: boolean;
  url: string;
  username: string;
  /** 后端序列化时跳过；留空 = 沿用后端已保存的密码。 */
  password?: string;
  password_ref: "keyring" | "file" | "none";
}

export interface SyncKinds {
  userdict: boolean;
  custom_yaml: boolean;
  dict_sources: boolean;
}

export type FirstSyncPolicy = "preview" | "pull-only" | "normal";

/** 开机自启机制的三种形态（后端 AutostartView.mechanism）。 */
export type AutostartMechanism = "xdg-desktop" | "registry" | "launch-agent";

export interface AutostartState {
  supported: boolean;
  enabled: boolean;
  mechanism: AutostartMechanism;
  /** 实际写入自启条目的可执行路径（AppImage 为 $APPIMAGE 的真实路径）。 */
  target: string;
  error: string | null;
}

export interface Config {
  webdav: WebDavConfig;
  proxy: ProxyConfig;
  password_ref: "keyring" | "file" | "none";
  rime_dir: string | null;
  device_id: string;
  transport_interval_minutes: number;
  // 备份保留份数（后端加载/保存时夹到 1..=100）。
  backup_keep: number;
  sync_kinds: SyncKinds;
  exclude_globs: string[];
  stop_fcitx5_during_apply: boolean;
  schema_version: number;
  first_sync_policy: FirstSyncPolicy;
  // 只存本机、不上传的 patch 键（默认 ["schema_list"]）；后端缺失时按默认值展示。
  device_local_keys: string[];
}

export interface Report {
  pulled: number;
  pushed: number;
  merged: number;
  conflicts: string[];
  skipped: string[];
  duration_ms: number;
  ok: boolean;
  error: string | null;
  backup_id?: string | null;
  first_sync?: boolean;
  mode?: string;
  pending_merge?: boolean;
  merged_entries?: number | null;
  conflict_details?: {
    path: string;
    only_local: number;
    only_remote: number;
    both_changed: number;
    sample: string[];
  }[];
}

export type PlanKind =
  | "pull"
  | "push"
  | "conflict"
  | "skip"
  | "delete_local"
  | "delete_remote";

/// 预览里的一条动作；kind 与真实运输的写操作一一对应。
export interface PlanAction {
  kind: PlanKind;
  path: string;
  detail: string;
}

export interface PlanCounts {
  pull: number;
  push: number;
  conflict: number;
  skip: number;
  delete_local: number;
  delete_remote: number;
}

/// 同步预览（dry-run）结果：actions 就是「立即搬运」接下来会做的事。
export interface PlanReport {
  actions: PlanAction[];
  counts: PlanCounts;
  total: number;
  mode: string;
  first_sync: boolean;
  dry_run: boolean;
  ok: boolean;
  error: string | null;
  duration_ms: number;
}

export interface Progress {
  phase: string;
  current: number;
  total: number;
  message: string;
}

export interface Detection {
  path: string;
  source: string;
  exists?: boolean;
}

export interface ProbeResult {
  ok: boolean;
  latency_ms: number;
  server: string | null;
  dav_class: string;
  server_type: string;
  status: number;
  remote_dir: string;
  remote_dir_exists: boolean;
  remote_dir_missing: boolean;
  preset: string | null;
  message: string;
  hint: string | null;
}

export interface NetworkStatus {
  state: "online" | "metered" | "offline";
  message: string;
  available: boolean;
  metered: boolean;
  failures: number;
  next_retry_at: number | null;
  next_retry_in_secs: number | null;
}

export interface RemoteDevice {
  id: string;
  files: number;
}

export interface RemoteShared {
  path: string;
  size: number | null;
}

export interface RemoteList {
  devices: RemoteDevice[];
  shared: RemoteShared[];
}

export type ConflictChoice = "local" | "conflict" | "remote";

export interface ConflictEntry {
  path: string;
  target: string;
  conflict_file: string;
  target_content: string | null;
  conflict_content: string | null;
  modified_at: number;
  target_size: number;
  conflict_size: number;
  remote_size: number | null;
  keys: string[];
  note: string | null;
}

export interface ConflictRemoteSizes {
  reachable: boolean;
  sizes: Record<string, number>;
}

export interface ConflictResolve {
  target: string;
  choice: string;
  removed: boolean;
  bytes: number | null;
}

/** 离线依赖安装告警（Linux；来源 ~/.config/com.rimesync.app/install-warning.json）。 */
export interface InstallWarning {
  present: boolean;
  path: string;
  written_at?: string | null;
  package?: string | null;
  reason?: string | null;
  log_path?: string | null;
  retry_command?: string | null;
}

export interface DeviceInfo {
  id: string;
  hostname: string;
  os: string;
  session: string;
  heartbeat_ts: number;
  /** 目录名 == 本机 installation_id。 */
  is_self: boolean;
  /** 旧版目录：目录名 != 本机 ID，且没有 owner.json 或 owner.device_id 就是目录名。 */
  legacy: boolean;
  reachable: boolean;
}

/** 重置远端身份（删除本机 sync/<installation_id>）的结果。 */
export interface ResetRemoteDirResult {
  id: string;
  removed: number;
  freed_bytes: number;
}

/** 设置页「显示名称 vs 本机目录名」提示用。 */
export interface LocalIdentity {
  device_id: string;
  installation_id: string;
  differs: boolean;
}

export interface MigrateOwner {
  device_id: string | null;
  hostname: string | null;
  os: string | null;
  session: string | null;
  heartbeat_ts: number;
}

/** 迁移预检发现目标目录已存在时的结构化状态。 */
export interface MigrateConflict {
  target: string;
  target_snapshot_count: number;
  target_owner: MigrateOwner | null;
  /** 目录名 = owner.device_id（0.6.0 及更早的命名）或没有 owner.json。 */
  target_legacy: boolean;
  target_in_use: boolean;
  /** 允许「删除目标后迁移」：遗留目录且没有其他在线设备在用。 */
  can_replace: boolean;
  replace_blocked_reason: string | null;
}

export type MigrateMode = "cancel" | "merge" | "replace";

export interface RevokeResult {
  id: string;
  removed: boolean;
}

export interface DeviceIdCheck {
  id: string;
  /// 语法可用（远端是否已有同名目录由 remote_exists 单独回报）。
  ok: boolean;
  error: string | null;
  remote_exists: boolean | null;
  remote_error: string | null;
}

export interface MigrateStep {
  step: string;
  ok: boolean;
  detail: string;
}

export interface MigrateResult {
  old_id: string;
  new_id: string;
  copied_files: number;
  copied_dirs: number;
  removed: number;
  freed_bytes: number;
  steps: MigrateStep[];
  /** 远端旧目录存在并已复制（false = 只改了本地目录与名称）。 */
  remote_renamed: boolean;
  /** 本地与远端都没有旧目录：无需搬运，只改了 installation_id 与标签。 */
  skipped: boolean;
  /** 迁移已完成但旧目录清理失败等非致命问题。 */
  warnings: string[];
  /** 本次实际模式：direct（目标不存在）/ cancel / merge / replace。 */
  mode: string;
  /** 预检发现目标已存在且 mode=cancel 时的冲突状态；其余为 null。 */
  conflict: MigrateConflict | null;
  /** merge 模式从目标目录补进源目录的快照数。 */
  merged_files: number;
}

/** 本机身份登记结果（只写 sync/<id>/owner.json，不搬快照）。 */
export interface PublishIdentityResult {
  device_id: string;
  remote_path: string;
  wrote: boolean;
}

export interface DedupeItem {
  id: string;
  label: string;
  size: number;
  suggested: boolean;
  is_keep: boolean;
  heartbeat_ts: number;
  hostname: string;
  os: string;
}

export type DedupeKind = "devices" | "snapshots" | "backups";

export interface DedupeGroup {
  key: string;
  tag: "device" | "unknown" | "same_dir" | "cross_dir" | "backup" | string;
  items: DedupeItem[];
}

export interface DedupeSection {
  groups: DedupeGroup[];
  suggested: string[];
  reclaimable: number;
  suggested_bytes: number;
  warnings: string[];
}

export interface DedupePreview {
  self_id: string;
  generated_at: number;
  devices: DedupeSection;
  snapshots: DedupeSection;
  backups: DedupeSection;
}

export interface DedupeFailure {
  id: string;
  error: string;
}

export interface DedupeResult {
  removed: string[];
  freed_bytes: number;
  failed: DedupeFailure[];
}

export interface BackupEntry {
  id: string;
  created_at: number;
  created_at_text: string;
  files: number;
  bytes: number;
  reason: string;
  is_pre_restore: boolean;
}

export interface CreatedBackup {
  id: string;
}

export interface RestoreResult {
  backup_id: string;
  pre_restore_id: string | null;
  restored: number;
}

export interface PruneResult {
  keep: number;
  removed: string[];
  remaining: number;
}

export interface PresetInfo {
  id: string;
  label: string;
  default_path: string;
  hint: string;
}

export interface PresetApply {
  normalized_url: string;
  hint: string;
}

export interface ProbeStep {
  step: string;
  ok: boolean;
  status: number;
  detail: string;
}

export interface WriteProbe {
  ok: boolean;
  step: string;
  http_status: number;
  hint: string | null;
  steps: ProbeStep[];
}

export interface WatchStatus {
  running: boolean;
  last_event_at: number | null;
}

export interface WatchEvent {
  kind: string;
  path: string;
  at: number;
}

export interface SecretStatus {
  backend: string;
  has_password: boolean;
  note: string | null;
}

export interface UpdateInfo {
  available: boolean;
  current_version: string;
  version: string | null;
  notes: string | null;
  date: string | null;
}

export interface InstallResult {
  installed: boolean;
  version: string;
  message: string;
}

export interface FirstRunResult {
  report: Report;
  strategy: string;
  note: string | null;
}

/// 一个可用的 Rime 输入方案（*.schema.yaml）。
/// source：local = 用户 rime 目录，shared = 随包 / 发行版 shared data 目录。
export interface SchemaEntry {
  id: string;
  name: string;
  enabled: boolean;
  source: "local" | "shared";
}

export interface SetSchemasResult {
  schemas: SchemaEntry[];
  backup: string | null;
  deployed: boolean;
  /// 配置已写入但部署失败时的可读原因。
  deploy_error: string | null;
}

/// build/ 产物状态：none = 没有判据文件；deploy = 有文件但没插件标记（官方部署）；
/// prebuilt = 含 rimesync_vnum / rimesync_punct，可用。
export type BuildProbe = "none" | "deploy" | "prebuilt";

/// 预编译产物自举报告（cmd_fix_ime）。
/// copied / kept / overwritten 只统计 build/；config_* 是 Rime 用户目录根下的 *.yaml。
export interface FixImeReport {
  rime_dir: string;
  payload: string;
  payload_source: string;
  probe_before: BuildProbe;
  probe_after: BuildProbe;
  copied: number;
  kept: number;
  overwritten: number;
  config_copied: number;
  config_kept: number;
  failed: number;
  source: string;
  build_ok: boolean;
  escalated: boolean;
  log_path: string | null;
  message: string;
  /// data 补丁或输入法安装需要管理员权限。
  needs_elevation: boolean;
  /// 输入法（Weasel）本体状态与自举结果（0.6.20）。
  weasel_present: boolean;
  weasel_installed: boolean;
  weasel_version: string;
  weasel_installer: string;
  weasel_installer_source: string;
  /// installed / already-present / skipped(platform) / disabled / failed
  weasel_install: string;
  weasel_error: string | null;
  weasel_needs_elevation: boolean;
  weasel_log: string;
  weasel_message: string;
}

export const api = {
  getConfig: () => invoke<Config>("cmd_get_config"),
  setConfig: (config: Config) => invoke<Config>("cmd_set_config", { config }),
  detectRimeDir: () => invoke<Detection>("cmd_detect_rime_dir"),
  testConnection: () => invoke<ProbeResult>("cmd_test_connection"),
  networkStatus: () => invoke<NetworkStatus>("cmd_network_status"),
  listRemote: () => invoke<RemoteList>("cmd_list_remote"),
  transport: () => invoke<Report>("cmd_transport"),
  planSync: () => invoke<PlanReport>("cmd_plan_sync"),
  apply: () => invoke<Report>("cmd_apply"),
  cancel: () => invoke<void>("cmd_cancel"),
  getLog: (limit = 200) => invoke<string[]>("cmd_get_log", { limit }),

  needsSetup: () => invoke<boolean>("cmd_needs_setup"),
  // --setup 启动参数：读取即清除，true = 本次强制进入同步设置向导。
  takeSetupRequest: () => invoke<boolean>("cmd_take_setup_request"),
  secretStatus: () => invoke<SecretStatus>("cmd_secret_status"),
  firstTransport: (strategy: "pull" | "bidirectional") =>
    invoke<FirstRunResult>("cmd_first_transport", { strategy }),

  listConflicts: () => invoke<ConflictEntry[]>("cmd_list_conflicts"),
  conflictRemoteSizes: () => invoke<ConflictRemoteSizes>("cmd_conflict_remote_sizes"),
  resolveConflict: (targetPath: string, choice: ConflictChoice) =>
    invoke<ConflictResolve>("cmd_resolve_conflict", { targetPath, choice }),

  listDevices: () => invoke<DeviceInfo[]>("cmd_list_devices"),
  // 本机目录（installation_id）必须带 confirm 才能注销；其它设备不需要。
  revokeDevice: (id: string, confirm?: string) =>
    invoke<RevokeResult>("cmd_revoke_device", { id, confirm: confirm ?? null }),
  resetLocalRemoteDir: (confirm: string) =>
    invoke<ResetRemoteDirResult>("cmd_reset_local_remote_dir", { confirm }),
  localIdentity: () => invoke<LocalIdentity>("cmd_local_identity"),
  checkDeviceId: (id: string) => invoke<DeviceIdCheck>("cmd_check_device_id", { id }),
  migrateDevice: (oldId: string, newId: string, mode: MigrateMode = "cancel") =>
    invoke<MigrateResult>("cmd_migrate_device", { old: oldId, new: newId, mode }),
  publishIdentity: () => invoke<PublishIdentityResult>("cmd_publish_identity"),

  dedupePreview: () => invoke<DedupePreview>("cmd_dedupe_preview"),
  dedupeApply: (kind: DedupeKind, ids: string[]) =>
    invoke<DedupeResult>("cmd_dedupe_apply", { kind, ids }),

  listBackups: () => invoke<BackupEntry[]>("cmd_list_backups"),
  createBackup: () => invoke<CreatedBackup>("cmd_create_backup"),
  restoreBackup: (id: string) => invoke<RestoreResult>("cmd_restore_backup", { id }),
  deleteBackup: (id: string) => invoke<void>("cmd_delete_backup", { id }),
  pruneBackups: () => invoke<PruneResult>("cmd_prune_backups"),

  listPresets: () => invoke<PresetInfo[]>("cmd_list_presets"),
  applyPreset: (presetId: string, rawUrl: string) =>
    invoke<PresetApply>("cmd_apply_preset", { presetId, rawUrl }),
  probeWrite: () => invoke<WriteProbe>("cmd_probe_write"),

  listSchemas: () => invoke<SchemaEntry[]>("cmd_list_schemas"),
  setSchemas: (ids: string[]) => invoke<SetSchemasResult>("cmd_set_schemas", { ids }),
  // 输入法产物自举：force = 覆盖已有 build/（config/*.yaml 永不覆盖）。
  fixIme: (force = false) => invoke<FixImeReport>("cmd_fix_ime", { force }),

  pickFolder: (title?: string) => invoke<string | null>("cmd_pick_folder", { title }),

  exportDiagnostics: () => invoke<{ path: string }>("cmd_export_diagnostics"),

  installWarning: () => invoke<InstallWarning>("cmd_install_warning"),
  retryInstallDeps: () => invoke<string>("cmd_retry_install_deps"),

  watchStart: () => invoke<WatchStatus>("cmd_watch_start"),
  watchStop: () => invoke<WatchStatus>("cmd_watch_stop"),
  watchStatus: () => invoke<WatchStatus>("cmd_watch_status"),

  checkUpdate: () => invoke<UpdateInfo>("cmd_check_update"),
  installUpdate: () => invoke<InstallResult>("cmd_install_update"),

  autostartStatus: () => invoke<AutostartState>("cmd_autostart_status"),
  // 开关自启：成功返回写入后的真实状态，失败 reject 可读错误（权限、路径变化等）。
  setAutostart: (enabled: boolean) => invoke<AutostartState>("cmd_set_autostart", { enabled }),
};

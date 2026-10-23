import { invoke } from "@tauri-apps/api/core";

export interface WebDavConfig {
  url: string;
  username: string;
  password: string;
  remote_dir: string;
}

export interface SyncKinds {
  userdict: boolean;
  custom_yaml: boolean;
  dict_sources: boolean;
}

export type FirstSyncPolicy = "preview" | "pull-only" | "normal";

export interface Config {
  webdav: WebDavConfig;
  password_ref: "keyring" | "file" | "none";
  rime_dir: string | null;
  device_id: string;
  transport_interval_minutes: number;
  sync_kinds: SyncKinds;
  exclude_globs: string[];
  stop_fcitx5_during_apply: boolean;
  schema_version: number;
  first_sync_policy: FirstSyncPolicy;
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

export interface DeviceInfo {
  id: string;
  hostname: string;
  os: string;
  session: string;
  heartbeat_ts: number;
  is_self: boolean;
  reachable: boolean;
}

export interface RevokeResult {
  id: string;
  removed: boolean;
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

export const api = {
  getConfig: () => invoke<Config>("cmd_get_config"),
  setConfig: (config: Config) => invoke<Config>("cmd_set_config", { config }),
  detectRimeDir: () => invoke<Detection>("cmd_detect_rime_dir"),
  testConnection: () => invoke<ProbeResult>("cmd_test_connection"),
  networkStatus: () => invoke<NetworkStatus>("cmd_network_status"),
  listRemote: () => invoke<RemoteList>("cmd_list_remote"),
  transport: () => invoke<Report>("cmd_transport"),
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
  revokeDevice: (id: string) => invoke<RevokeResult>("cmd_revoke_device", { id }),

  listBackups: () => invoke<BackupEntry[]>("cmd_list_backups"),
  createBackup: () => invoke<CreatedBackup>("cmd_create_backup"),
  restoreBackup: (id: string) => invoke<RestoreResult>("cmd_restore_backup", { id }),

  listPresets: () => invoke<PresetInfo[]>("cmd_list_presets"),
  applyPreset: (presetId: string, rawUrl: string) =>
    invoke<PresetApply>("cmd_apply_preset", { presetId, rawUrl }),
  probeWrite: () => invoke<WriteProbe>("cmd_probe_write"),

  pickFolder: (title?: string) => invoke<string | null>("cmd_pick_folder", { title }),

  exportDiagnostics: () => invoke<{ path: string }>("cmd_export_diagnostics"),

  watchStart: () => invoke<WatchStatus>("cmd_watch_start"),
  watchStop: () => invoke<WatchStatus>("cmd_watch_stop"),
  watchStatus: () => invoke<WatchStatus>("cmd_watch_status"),

  checkUpdate: () => invoke<UpdateInfo>("cmd_check_update"),
  installUpdate: () => invoke<InstallResult>("cmd_install_update"),
};

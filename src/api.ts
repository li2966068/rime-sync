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

export interface Config {
  webdav: WebDavConfig;
  rime_dir: string | null;
  device_id: string;
  transport_interval_minutes: number;
  sync_kinds: SyncKinds;
  exclude_globs: string[];
  stop_fcitx5_during_apply: boolean;
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
}

export interface ProbeResult {
  ok: boolean;
  latency_ms: number;
  server: string | null;
  dav_class: string;
  server_type: string;
  status: number;
  remote_dir_exists: boolean;
  message: string;
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

export const api = {
  getConfig: () => invoke<Config>("cmd_get_config"),
  setConfig: (config: Config) => invoke<Config>("cmd_set_config", { config }),
  detectRimeDir: () => invoke<Detection>("cmd_detect_rime_dir"),
  testConnection: () => invoke<ProbeResult>("cmd_test_connection"),
  listRemote: () => invoke<RemoteList>("cmd_list_remote"),
  transport: () => invoke<Report>("cmd_transport"),
  apply: () => invoke<Report>("cmd_apply"),
  cancel: () => invoke<void>("cmd_cancel"),
  getLog: (limit = 200) => invoke<string[]>("cmd_get_log", { limit }),
};

import "./styles.css";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  type BackupEntry,
  type Config,
  type ConflictEntry,
  type DeviceInfo,
  type NetworkStatus,
  type PresetInfo,
  type ProbeResult,
  type Progress,
  type Report,
  type SecretStatus,
  type WatchEvent,
  type WatchStatus,
  type WriteProbe,
} from "./api";
import { applyI18n, getLang, presetHint, presetLabel, setLang, t } from "./i18n";

function byId<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) {
    throw new Error("缺少界面元素 #" + id);
  }
  return node as T;
}

const el = {
  tabs: Array.from(document.querySelectorAll<HTMLButtonElement>(".tab")),
  panels: Array.from(document.querySelectorAll<HTMLElement>(".panel")),
  busy: byId<HTMLDivElement>("busy"),
  busyText: byId<HTMLSpanElement>("busy-text"),

  preset: byId<HTMLSelectElement>("preset"),
  presetHint: byId<HTMLParagraphElement>("preset-hint"),
  url: byId<HTMLInputElement>("dav-url"),
  user: byId<HTMLInputElement>("dav-user"),
  pass: byId<HTMLInputElement>("dav-pass"),
  dir: byId<HTMLInputElement>("dav-dir"),
  togglePass: byId<HTMLButtonElement>("toggle-pass"),
  saveConn: byId<HTMLButtonElement>("btn-save-conn"),
  test: byId<HTMLButtonElement>("btn-test"),
  probe: byId<HTMLButtonElement>("btn-probe"),
  listRemote: byId<HTMLButtonElement>("btn-list-remote"),
  connStatus: byId<HTMLDivElement>("conn-status"),
  networkStatus: byId<HTMLParagraphElement>("network-status"),
  remoteList: byId<HTMLDivElement>("remote-list"),
  secretStatus: byId<HTMLSpanElement>("secret-status"),

  rimeDir: byId<HTMLInputElement>("rime-dir"),
  rimeDirSource: byId<HTMLParagraphElement>("rime-dir-source"),
  detect: byId<HTMLButtonElement>("btn-detect"),
  browseRime: byId<HTMLButtonElement>("btn-browse-rime"),
  wizardBrowse: byId<HTMLButtonElement>("wizard-browse"),
  watchToggle: byId<HTMLInputElement>("watch-toggle"),
  watchStatus: byId<HTMLSpanElement>("watch-status"),
  watchLast: byId<HTMLSpanElement>("watch-last"),
  transport: byId<HTMLButtonElement>("btn-transport"),
  apply: byId<HTMLButtonElement>("btn-apply"),
  cancel: byId<HTMLButtonElement>("btn-cancel"),

  progress: byId<HTMLDivElement>("progress"),
  progressFill: byId<HTMLDivElement>("progress-fill"),
  progressLabel: byId<HTMLSpanElement>("progress-label"),
  progressCount: byId<HTMLSpanElement>("progress-count"),

  reportPulled: byId<HTMLSpanElement>("report-pulled"),
  reportPushed: byId<HTMLSpanElement>("report-pushed"),
  reportMerged: byId<HTMLSpanElement>("report-merged"),
  reportConflicts: byId<HTMLSpanElement>("report-conflicts"),
  reportDuration: byId<HTMLSpanElement>("report-duration"),
  reportBackup: byId<HTMLSpanElement>("report-backup"),
  reportMode: byId<HTMLDivElement>("report-mode"),
  reportNote: byId<HTMLDivElement>("report-note"),

  firstSyncBanner: byId<HTMLDivElement>("first-sync-banner"),
  firstSyncError: byId<HTMLParagraphElement>("first-sync-error"),
  firstSyncSwitch: byId<HTMLButtonElement>("btn-first-sync-normal"),

  log: byId<HTMLPreElement>("log"),
  refreshLog: byId<HTMLButtonElement>("btn-refresh-log"),
  clearLog: byId<HTMLButtonElement>("btn-clear-log"),

  conflictsCount: byId<HTMLSpanElement>("conflicts-count"),
  conflictList: byId<HTMLDivElement>("conflict-list"),
  conflictEmpty: byId<HTMLParagraphElement>("conflict-empty"),
  conflictBody: byId<HTMLDivElement>("conflict-body"),
  conflictMeta: byId<HTMLDivElement>("conflict-meta"),
  diffLeft: byId<HTMLDivElement>("diff-left"),
  diffRight: byId<HTMLDivElement>("diff-right"),
  conflictsRefresh: byId<HTMLButtonElement>("btn-conflicts-refresh"),
  conflictsKeepAll: byId<HTMLButtonElement>("btn-conflicts-keep-all"),
  conflictsTakeAll: byId<HTMLButtonElement>("btn-conflicts-take-all"),
  conflictLocal: byId<HTMLButtonElement>("btn-conflict-local"),
  conflictCopy: byId<HTMLButtonElement>("btn-conflict-copy"),

  devicesRefresh: byId<HTMLButtonElement>("btn-devices-refresh"),
  deviceBody: byId<HTMLTableSectionElement>("device-body"),
  deviceEmpty: byId<HTMLParagraphElement>("device-empty"),

  interval: byId<HTMLInputElement>("interval"),
  deviceId: byId<HTMLInputElement>("device-id"),
  regenDevice: byId<HTMLButtonElement>("btn-regen-device"),
  kindUserdict: byId<HTMLInputElement>("kind-userdict"),
  kindYaml: byId<HTMLInputElement>("kind-yaml"),
  kindDict: byId<HTMLInputElement>("kind-dict"),
  excludes: byId<HTMLTextAreaElement>("excludes"),
  stopFcitx5: byId<HTMLInputElement>("stop-fcitx5"),
  saveSettings: byId<HTMLButtonElement>("btn-save-settings"),
  settingsHint: byId<HTMLSpanElement>("settings-hint"),

  backupList: byId<HTMLDivElement>("backup-list"),
  backupCreate: byId<HTMLButtonElement>("btn-backup-create"),
  diag: byId<HTMLButtonElement>("btn-diag"),
  diagResult: byId<HTMLDivElement>("diag-result"),
  diagPath: byId<HTMLElement>("diag-path"),
  diagCopy: byId<HTMLButtonElement>("btn-diag-copy"),

  language: byId<HTMLSelectElement>("language"),
  updateStatus: byId<HTMLElement>("update-status"),
  updateVersion: byId<HTMLParagraphElement>("update-version"),
  updatePanel: byId<HTMLDivElement>("update-panel"),
  updateNotes: byId<HTMLParagraphElement>("update-notes"),
  updateCheck: byId<HTMLButtonElement>("btn-update-check"),
  updateInstall: byId<HTMLButtonElement>("btn-update-install"),

  footerStatus: byId<HTMLSpanElement>("footer-status"),
  footerConfig: byId<HTMLSpanElement>("footer-config"),
  toast: byId<HTMLDivElement>("toast"),

  modal: byId<HTMLDivElement>("modal"),
  modalTitle: byId<HTMLHeadingElement>("modal-title"),
  modalBody: byId<HTMLParagraphElement>("modal-body"),
  modalCancel: byId<HTMLButtonElement>("modal-cancel"),
  modalConfirm: byId<HTMLButtonElement>("modal-confirm"),

  wizard: byId<HTMLDivElement>("wizard"),
  wizardStepLabel: byId<HTMLSpanElement>("wizard-step-label"),
  wizardBar: byId<HTMLDivElement>("wizard-bar"),
  wizardSteps: Array.from(document.querySelectorAll<HTMLElement>(".wizard-step")),
  wizardBack: byId<HTMLButtonElement>("wizard-back"),
  wizardSkip: byId<HTMLButtonElement>("wizard-skip"),
  wizardNext: byId<HTMLButtonElement>("wizard-next"),
  wizardRime: byId<HTMLInputElement>("wizard-rime"),
  wizardDetect: byId<HTMLButtonElement>("wizard-detect"),
  wizardRimeStatus: byId<HTMLParagraphElement>("wizard-rime-status"),
  wizardPreset: byId<HTMLSelectElement>("wizard-preset"),
  wizardPresetHint: byId<HTMLParagraphElement>("wizard-preset-hint"),
  wizardTitle: byId<HTMLHeadingElement>("wizard-title"),
  wizardDevice: byId<HTMLInputElement>("wizard-device"),
  wizardDetails: byId<HTMLDivElement>("wizard-details"),
  wizardSummary: byId<HTMLDivElement>("wizard-summary"),
  wizardPolicyHint: byId<HTMLParagraphElement>("wizard-policy-hint"),
  wizardRunFirst: byId<HTMLInputElement>("wizard-run-first"),
  wizardUrl: byId<HTMLInputElement>("wizard-url"),
  wizardUser: byId<HTMLInputElement>("wizard-user"),
  wizardPass: byId<HTMLInputElement>("wizard-pass"),
  wizardDir: byId<HTMLInputElement>("wizard-dir"),
  wizardTest: byId<HTMLButtonElement>("wizard-test"),
  wizardStatus: byId<HTMLDivElement>("wizard-status"),
  wizardStrategy: byId<HTMLDivElement>("wizard-strategy"),
  wizardResult: byId<HTMLDivElement>("wizard-result"),
};

const PRESET_KEY = "rime-sync.preset";

let config: Config | null = null;
let presets: PresetInfo[] = [];
let busy = false;
let lastRun = Date.now();
let logCache: string[] = [];
let toastTimer = 0;
let modalResolve: ((value: boolean) => void) | null = null;
let conflicts: ConflictEntry[] = [];
let activeConflict: string | null = null;
let devices: DeviceInfo[] = [];
let backups: BackupEntry[] = [];
let watchState: WatchStatus = { running: false, last_event_at: null };
let secretState: SecretStatus | null = null;
let pendingUpdate: string | null = null;
let appVersion = "0.2.0";

interface WizardState {
  step: number;
  policy: "preview" | "normal";
  mode: "first-run" | "settings";
  finished: boolean;
}

let wizard: WizardState = { step: 1, policy: "preview", mode: "first-run", finished: false };

function errorText(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === "string") {
    return error;
  }
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}

function timeStamp(date = new Date()): string {
  return [date.getHours(), date.getMinutes(), date.getSeconds()]
    .map((part) => String(part).padStart(2, "0"))
    .join(":");
}

function locale(): string {
  return getLang() === "zh" ? "zh-CN" : "en-US";
}

function formatTimestamp(seconds: number | null | undefined): string {
  if (!seconds) {
    return t("common.unknown");
  }
  return new Date(seconds * 1000).toLocaleString(locale());
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return bytes + " B";
  }
  if (bytes < 1024 * 1024) {
    return (bytes / 1024).toFixed(1) + " KB";
  }
  return (bytes / (1024 * 1024)).toFixed(1) + " MB";
}

function toast(message: string): void {
  el.toast.textContent = message;
  el.toast.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    el.toast.hidden = true;
  }, 2600);
}

function setFooter(text: string): void {
  el.footerStatus.textContent = text;
}

function appendLog(line: string): void {
  logCache.push(line);
  if (logCache.length > 400) {
    logCache = logCache.slice(-400);
  }
  el.log.textContent = logCache.join("\n");
  el.log.scrollTop = el.log.scrollHeight;
}

function localLog(message: string): void {
  appendLog("[" + timeStamp() + "] " + message);
}

function setStatus(node: HTMLDivElement, kind: "ok" | "err" | "idle", text: string): void {
  node.hidden = false;
  node.className = "status" + (kind === "idle" ? "" : " " + kind);
  node.textContent = text;
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function confirmDialog(title: string, body: string): Promise<boolean> {
  el.modalTitle.textContent = title;
  el.modalBody.textContent = body;
  el.modal.hidden = false;
  el.modalConfirm.focus();
  return new Promise((resolve) => {
    modalResolve = resolve;
  });
}

function settleModal(value: boolean): void {
  el.modal.hidden = true;
  const resolve = modalResolve;
  modalResolve = null;
  resolve?.(value);
}

async function copyText(value: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(value);
    return true;
  } catch {
    // 没有剪贴板权限时退回 execCommand。
  }
  try {
    const area = document.createElement("textarea");
    area.value = value;
    document.body.appendChild(area);
    area.select();
    const ok = document.execCommand("copy");
    area.remove();
    return ok;
  } catch {
    return false;
  }
}

function switchPanel(name: string): void {
  for (const tab of el.tabs) {
    const active = tab.dataset.panel === name;
    tab.classList.toggle("is-active", active);
    tab.setAttribute("aria-selected", String(active));
  }
  for (const panel of el.panels) {
    panel.classList.toggle("is-active", panel.id === "panel-" + name);
  }
  if (name === "conflicts") {
    void loadConflicts();
  }
  if (name === "devices") {
    void loadDevices();
  }
  if (name === "settings") {
    void loadBackups();
  }
}

function renderConfig(next: Config): void {
  config = next;
  el.url.value = next.webdav.url;
  el.user.value = next.webdav.username;
  // 后端序列化时跳过 password，这里可能是 undefined。
  el.pass.value = next.webdav.password ?? "";
  el.dir.value = next.webdav.remote_dir;
  el.rimeDir.value = next.rime_dir ?? "";
  el.interval.value = String(next.transport_interval_minutes);
  el.deviceId.value = next.device_id;
  el.kindUserdict.checked = next.sync_kinds.userdict;
  el.kindYaml.checked = next.sync_kinds.custom_yaml;
  el.kindDict.checked = next.sync_kinds.dict_sources;
  el.excludes.value = next.exclude_globs.join("\n");
  el.stopFcitx5.checked = next.stop_fcitx5_during_apply;
  el.footerConfig.textContent = next.device_id ? t("footer.device", { id: next.device_id }) : "";
  renderFirstSyncBanner();
}

/* ---------- 首同步保护（SPEC-4 §A1） ---------- */

function renderFirstSyncBanner(): void {
  // 字段缺失时按后端默认值 preview 处理（宁可按只读展示，也不静默放开上传）。
  const policy = config?.first_sync_policy ?? "preview";
  const visible = config !== null && policy !== "normal";
  el.firstSyncBanner.hidden = !visible;
  el.firstSyncSwitch.disabled = !visible || busy;
  if (!visible) {
    el.firstSyncError.hidden = true;
    el.firstSyncError.textContent = "";
  }
}

async function switchToTwoWay(): Promise<void> {
  if (!config || busy) {
    return;
  }
  // 只改 first_sync_policy；其余字段（含 schema_version / password_ref）原样写回。
  const draft: Config = { ...config, first_sync_policy: "normal" };
  el.firstSyncSwitch.disabled = true;
  try {
    const saved = await api.setConfig(draft);
    renderConfig(saved);
    toast(t("firstSync.switched"));
    localLog(t("firstSync.switched"));
  } catch (error) {
    const text = errorText(error);
    el.firstSyncError.textContent = t("firstSync.switchFail", { error: text });
    el.firstSyncError.hidden = false;
    toast(t("firstSync.switchFail", { error: text }));
    localLog(t("firstSync.switchFail", { error: text }));
    el.firstSyncSwitch.disabled = false;
  }
}

function requireConfig(): Config {
  if (!config) {
    throw new Error("配置尚未加载");
  }
  return config;
}

function connectionDraft(): Config {
  const base = requireConfig();
  return {
    ...base,
    webdav: {
      url: el.url.value.trim(),
      username: el.user.value,
      password: el.pass.value,
      // 留空 = 服务器根（SPEC-3 §A4）。
      remote_dir: el.dir.value.trim(),
    },
  };
}

function settingsDraft(): Config {
  const base = requireConfig();
  const minutes = Number.parseInt(el.interval.value, 10);
  return {
    ...base,
    transport_interval_minutes: Number.isFinite(minutes)
      ? Math.min(1440, Math.max(0, minutes))
      : base.transport_interval_minutes,
    sync_kinds: {
      userdict: el.kindUserdict.checked,
      custom_yaml: el.kindYaml.checked,
      dict_sources: el.kindDict.checked,
    },
    exclude_globs: el.excludes.value
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line.length > 0),
    stop_fcitx5_during_apply: el.stopFcitx5.checked,
  };
}

async function persist(draft: Config, message: string): Promise<void> {
  const saved = await api.setConfig(draft);
  renderConfig(saved);
  void refreshSecretStatus();
  toast(message);
}

async function saveConnection(notify = true): Promise<void> {
  try {
    const saved = await api.setConfig(connectionDraft());
    renderConfig(saved);
    if (notify) {
      toast(t("conn.saved"));
    }
    void refreshSecretStatus();
  } catch (error) {
    setStatus(el.connStatus, "err", t("conn.saveFail", { error: errorText(error) }));
    throw error;
  }
}

function renderProgress(progress: Progress): void {
  if (progress.phase === "done") {
    el.progressLabel.textContent = t("sync.progressDone");
  } else {
    el.progressLabel.textContent = progress.message
      ? progress.phase + " · " + progress.message
      : progress.phase;
  }
  el.progressCount.textContent =
    progress.total > 0 ? progress.current + "/" + progress.total : "";
  const percent =
    progress.total > 0
      ? Math.min(100, Math.round((progress.current / progress.total) * 100))
      : progress.phase === "done"
        ? 100
        : 15;
  el.progressFill.style.width = percent + "%";
  el.progress.classList.toggle("is-running", progress.phase !== "done");
  el.progress.setAttribute("aria-valuenow", String(percent));
  if (progress.message) {
    appendLog("[" + progress.phase + "] " + progress.message);
  }
}

function renderReport(report: Report): void {
  el.reportPulled.textContent = String(report.pulled);
  el.reportPushed.textContent = String(report.pushed);
  el.reportMerged.textContent = String(report.merged);
  el.reportConflicts.textContent = String(report.conflicts.length);
  el.reportDuration.textContent = report.duration_ms + " ms";
  el.reportBackup.textContent = report.backup_id ?? t("sync.none");

  const mode = report.mode ?? "";
  const pullOnly = mode === "preview" || mode === "pull-only";
  el.reportMode.hidden = !pullOnly;
  el.reportMode.textContent = pullOnly
    ? t("sync.modePullOnly", { mode, pushed: report.pushed })
    : "";

  const notes: string[] = [];
  if (report.pending_merge) {
    notes.push(t("sync.pendingMerge"));
  }
  if (report.merged_entries != null) {
    notes.push(t("sync.mergedEntries", { count: report.merged_entries }));
  }
  if (report.error) {
    notes.push(t("sync.reportError", { error: report.error }));
  }
  if (report.conflicts.length > 0) {
    notes.push(t("sync.reportConflictPrefix", { list: report.conflicts.slice(0, 4).join(", ") }));
  }
  if (report.skipped.length > 0) {
    notes.push(t("sync.reportSkip", { count: report.skipped.length }));
  }
  el.reportNote.hidden = notes.length === 0;
  el.reportNote.textContent = notes.join("\n");

  const verdict = report.ok ? t("sync.verdictOk") : t("sync.verdictFail");
  setFooter(t("footer.last", { time: timeStamp(), verdict, ms: report.duration_ms }));
  localLog(
    t("sync.result", {
      verdict,
      pulled: report.pulled,
      pushed: report.pushed,
      merged: report.merged,
      conflicts: report.conflicts.length,
    }),
  );
  if (!report.ok) {
    el.progressLabel.textContent = t("sync.progressFailed");
  }
}

function setBusy(value: boolean, label = t("busy.default")): void {
  busy = value;
  el.busy.hidden = !value;
  el.busyText.textContent = label;
  el.transport.disabled = value;
  el.apply.disabled = value;
  el.cancel.disabled = !value;
  el.detect.disabled = value;
  el.saveConn.disabled = value;
  el.test.disabled = value;
  el.probe.disabled = value;
  el.conflictsKeepAll.disabled = value;
  el.conflictsTakeAll.disabled = value;
  el.firstSyncSwitch.disabled = value || !config || config.first_sync_policy === "normal";
}

async function run(mode: "transport" | "apply", automatic = false): Promise<void> {
  if (busy) {
    return;
  }
  if (mode === "apply") {
    const ok = await confirmDialog(t("sync.applyConfirmTitle"), t("sync.applyConfirmBody"));
    if (!ok) {
      return;
    }
  }
  const label = mode === "apply" ? t("busy.apply") : t("busy.transport");
  setBusy(true, label);
  el.progress.classList.add("is-running");
  el.progressLabel.textContent = mode === "apply" ? t("sync.startedApply") : t("sync.startedTransport");
  el.progressCount.textContent = "";
  el.progressFill.style.width = "8%";
  localLog(
    automatic
      ? t("sync.auto")
      : mode === "apply"
        ? t("sync.startedApply")
        : t("sync.startedTransport"),
  );

  try {
    const report = mode === "apply" ? await api.apply() : await api.transport();
    renderReport(report);
    if (!report.ok && report.error === "cancelled by user") {
      setFooter(t("sync.cancelled"));
    }
    if (mode === "apply" && report.backup_id) {
      localLog(t("sync.lastBackup", { id: report.backup_id }));
    }
  } catch (error) {
    const text = errorText(error);
    setStatus(el.connStatus, "err", t("sync.failed", { error: text }));
    localLog(t("sync.failed", { error: text }));
    setFooter(t("sync.progressFailed"));
    toast(t("sync.failed", { error: text }));
  } finally {
    lastRun = Date.now();
    setBusy(false);
    el.progress.classList.remove("is-running");
    void refreshNetwork();
  }
}

async function saveRimeDir(): Promise<void> {
  try {
    const value = el.rimeDir.value.trim();
    const saved = await api.setConfig({
      ...requireConfig(),
      rime_dir: value.length > 0 ? value : null,
    });
    renderConfig(saved);
    el.rimeDirSource.textContent = value ? t("sync.manual") : t("sync.notDetected");
  } catch (error) {
    toast(t("conn.saveFail", { error: errorText(error) }));
  }
}

/// 原生目录选择器：设置页保存，向导只填输入框（点「下一步」时统一保存）。
async function pickRimeDir(
  target: HTMLInputElement,
  status: HTMLParagraphElement,
  save: boolean,
): Promise<void> {
  try {
    const picked = await api.pickFolder(t("sync.rimeDir"));
    if (!picked) {
      return;
    }
    target.value = picked;
    status.textContent = t("sync.manual");
    if (save) {
      const saved = await api.setConfig({ ...requireConfig(), rime_dir: picked });
      renderConfig(saved);
      toast(t("sync.saved"));
    }
  } catch (error) {
    toast(t("sync.detectFail") + "：" + errorText(error));
  }
}

async function detectRimeDir(
  target = el.rimeDir,
  status = el.rimeDirSource,
  silent = false,
): Promise<void> {
  el.detect.disabled = true;
  status.textContent = t("sync.detecting");
  try {
    const detection = await api.detectRimeDir();
    target.value = detection.path;
    status.textContent = t("sync.source", { source: detection.source });
    const saved = await api.setConfig({ ...requireConfig(), rime_dir: detection.path });
    renderConfig(saved);
    if (!silent) {
      toast(t("sync.detected"));
    }
  } catch (error) {
    status.textContent = t("sync.detectFail");
    toast(t("sync.detectFail") + "：" + errorText(error));
  } finally {
    el.detect.disabled = busy;
  }
}

async function testConnection(notify = true): Promise<WriteProbe | null> {
  el.test.disabled = true;
  setStatus(el.connStatus, "idle", t("conn.testing"));
  try {
    await saveConnection(false);
    const result = await api.testConnection();
    const lines = [
      result.ok ? t("conn.testOk") : t("conn.testFail"),
      t("conn.latency", { ms: result.latency_ms }),
      t("conn.server", { name: result.server_type }),
      t("conn.dav", { value: result.dav_class || t("conn.davNone") }),
      result.remote_dir_missing
        ? t("conn.dirWillCreate")
        : result.remote_dir_exists
          ? t("conn.dirExists")
          : t("conn.dirMissing"),
    ];
    if (result.message) {
      lines.push(result.message);
    }
    if (result.hint) {
      lines.push(result.hint);
    }
    const probe = await api.probeWrite();
    lines.push(
      probe.ok
        ? t("conn.probeOk")
        : t("conn.probeFail") + " · " + t("conn.probeStep", { step: probe.step, status: probe.http_status }),
    );
    if (!probe.ok && probe.hint) {
      lines.push(probe.hint);
    }
    setStatus(el.connStatus, result.ok && probe.ok ? "ok" : "err", lines.join(" · "));
    localLog("test: " + lines.join(" · "));
    if (!probe.ok && probe.hint) {
      toast(probe.hint);
    } else if (notify) {
      toast(result.ok ? t("conn.testOk") : t("conn.testFail"));
    }
    return probe;
  } catch (error) {
    const text = errorText(error);
    setStatus(el.connStatus, "err", t("conn.testFail") + "：" + text);
    localLog(t("conn.testFail") + "：" + text);
    toast(t("conn.testFail") + "：" + text);
    return null;
  } finally {
    el.test.disabled = busy;
  }
}

async function probeWrite(): Promise<void> {
  el.probe.disabled = true;
  setStatus(el.connStatus, "idle", t("conn.probeTesting"));
  try {
    const probe = await api.probeWrite();
    const lines = [
      probe.ok ? t("conn.probeOk") : t("conn.probeFail"),
      t("conn.probeStep", { step: probe.step, status: probe.http_status }),
    ];
    if (probe.hint) {
      lines.push(probe.hint);
    }
    setStatus(el.connStatus, probe.ok ? "ok" : "err", lines.join(" · "));
    localLog("probe: " + lines.join(" · "));
    if (probe.hint) {
      toast(probe.hint);
    }
  } catch (error) {
    setStatus(el.connStatus, "err", t("conn.probeFail") + "：" + errorText(error));
  } finally {
    el.probe.disabled = busy;
  }
}

async function listRemote(): Promise<void> {
  el.listRemote.disabled = true;
  try {
    const remote = await api.listRemote();
    const parts: string[] = [];
    parts.push("<h3>" + escapeHtml(t("conn.remoteDevices", { count: remote.devices.length })) + "</h3>");
    if (remote.devices.length === 0) {
      parts.push('<p class="muted">' + escapeHtml(t("conn.remoteDevicesEmpty")) + "</p>");
    } else {
      parts.push(
        "<ul>" +
          remote.devices
            .map((device) => "<li>" + escapeHtml(device.id) + " · " + device.files + "</li>")
            .join("") +
          "</ul>",
      );
    }
    parts.push("<h3>" + escapeHtml(t("conn.remoteShared", { count: remote.shared.length })) + "</h3>");
    if (remote.shared.length === 0) {
      parts.push('<p class="muted">' + escapeHtml(t("conn.remoteSharedEmpty")) + "</p>");
    } else {
      parts.push(
        "<ul>" +
          remote.shared
            .slice(0, 30)
            .map(
              (file) =>
                "<li>" +
                escapeHtml(file.path) +
                (file.size === null ? "" : " · " + formatBytes(file.size)) +
                "</li>",
            )
            .join("") +
          "</ul>",
      );
    }
    el.remoteList.innerHTML = parts.join("");
    el.remoteList.hidden = false;
  } catch (error) {
    setStatus(el.connStatus, "err", t("conn.remoteFail", { error: errorText(error) }));
  } finally {
    el.listRemote.disabled = busy;
  }
}

async function refreshSecretStatus(): Promise<void> {
  try {
    const status = await api.secretStatus();
    secretState = status;
    el.secretStatus.textContent =
      (status.backend === "keyring" ? t("conn.secretKeyring") : t("conn.secretFile")) +
      " · " +
      (status.has_password ? t("conn.secretSaved") : t("conn.secretMissing"));
    el.secretStatus.className = "pill" + (status.has_password ? " ok" : "");
  } catch {
    el.secretStatus.textContent = "";
  }
}

/* ---------- 网络状态（SPEC-3 §C10） ---------- */

function formatRetry(seconds: number): string {
  if (seconds <= 0) {
    return t("network.retryDue");
  }
  if (seconds < 60) {
    return t("network.retrySoon");
  }
  const minutes = Math.ceil(seconds / 60);
  return t("network.retryIn", { minutes });
}

async function refreshNetwork(): Promise<void> {
  try {
    const status: NetworkStatus = await api.networkStatus();
    const lines = [t("network.label", { state: status.message })];
    if (status.state === "metered") {
      lines.push(t("network.meteredNote"));
    }
    if (typeof status.next_retry_in_secs === "number") {
      lines.push(formatRetry(status.next_retry_in_secs));
    }
    el.networkStatus.textContent = lines.join(" · ");
    el.networkStatus.className =
      "hint network-" + status.state + (status.state === "online" ? "" : " warn");
  } catch {
    el.networkStatus.textContent = "";
  }
}

/* ---------- 冲突 ---------- */

interface DiffRow {
  text: string;
  cls: "same" | "del" | "add" | "pad";
  n: number | null;
}

function splitLines(text: string): string[] {
  const normalized = text.replace(/\r\n/g, "\n");
  const lines = normalized.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") {
    lines.pop();
  }
  return lines;
}

function plainRows(left: string[], right: string[]): { left: DiffRow[]; right: DiffRow[] } {
  const outLeft: DiffRow[] = [];
  const outRight: DiffRow[] = [];
  const total = Math.max(left.length, right.length);
  for (let index = 0; index < total; index += 1) {
    const hasLeft = index < left.length;
    const hasRight = index < right.length;
    outLeft.push(
      hasLeft
        ? { text: left[index], cls: "same", n: index + 1 }
        : { text: "", cls: "pad", n: null },
    );
    outRight.push(
      hasRight
        ? { text: right[index], cls: "same", n: index + 1 }
        : { text: "", cls: "pad", n: null },
    );
  }
  return { left: outLeft, right: outRight };
}

function diffRows(left: string[], right: string[]): { left: DiffRow[]; right: DiffRow[] } {
  const n = left.length;
  const m = right.length;
  if (n === 0 && m === 0) {
    return { left: [], right: [] };
  }
  if (n * m > 2_000_000) {
    return plainRows(left, right);
  }
  const width = m + 1;
  const dp = new Uint32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      dp[i * width + j] =
        left[i] === right[j]
          ? dp[(i + 1) * width + j + 1] + 1
          : Math.max(dp[(i + 1) * width + j], dp[i * width + j + 1]);
    }
  }
  const outLeft: DiffRow[] = [];
  const outRight: DiffRow[] = [];
  let i = 0;
  let j = 0;
  let lnLeft = 1;
  let lnRight = 1;
  while (i < n && j < m) {
    if (left[i] === right[j]) {
      outLeft.push({ text: left[i], cls: "same", n: lnLeft++ });
      outRight.push({ text: right[j], cls: "same", n: lnRight++ });
      i += 1;
      j += 1;
    } else if (dp[(i + 1) * width + j] >= dp[i * width + j + 1]) {
      outLeft.push({ text: left[i], cls: "del", n: lnLeft++ });
      outRight.push({ text: "", cls: "pad", n: null });
      i += 1;
    } else {
      outLeft.push({ text: "", cls: "pad", n: null });
      outRight.push({ text: right[j], cls: "add", n: lnRight++ });
      j += 1;
    }
  }
  while (i < n) {
    outLeft.push({ text: left[i], cls: "del", n: lnLeft++ });
    outRight.push({ text: "", cls: "pad", n: null });
    i += 1;
  }
  while (j < m) {
    outLeft.push({ text: "", cls: "pad", n: null });
    outRight.push({ text: right[j], cls: "add", n: lnRight++ });
    j += 1;
  }
  return { left: outLeft, right: outRight };
}

function rowsHtml(rows: DiffRow[]): string {
  return rows
    .map((row) =>
      row.cls === "pad"
        ? '<div class="diff-line pad"></div>'
        : '<div class="diff-line ' +
          row.cls +
          '"><span class="ln">' +
          String(row.n ?? "") +
          '</span><span class="code">' +
          escapeHtml(row.text) +
          "</span></div>",
    )
    .join("");
}

function renderConflictList(): void {
  el.conflictsCount.textContent = t("conflicts.count", { count: conflicts.length });
  if (conflicts.length === 0) {
    el.conflictList.innerHTML = '<p class="muted empty">' + escapeHtml(t("conflicts.empty")) + "</p>";
    el.conflictEmpty.hidden = true;
    el.conflictBody.hidden = true;
    return;
  }
  el.conflictList.innerHTML = conflicts
    .map(
      (entry) =>
        '<button type="button" role="listitem" class="conflict-item' +
        (entry.path === activeConflict ? " is-active" : "") +
        '" data-path="' +
        escapeHtml(entry.path) +
        '">' +
        '<span class="conflict-target">' +
        escapeHtml(entry.target) +
        "</span>" +
        '<span class="muted small">' +
        escapeHtml(entry.path) +
        "</span>" +
        '<span class="muted small">' +
        escapeHtml(formatTimestamp(entry.modified_at)) +
        "</span>" +
        "</button>",
    )
    .join("");
  el.conflictEmpty.hidden = activeConflict !== null;
  if (!activeConflict || !conflicts.some((entry) => entry.path === activeConflict)) {
    el.conflictBody.hidden = true;
  }
}

function renderDiff(entry: ConflictEntry): void {
  activeConflict = entry.path;
  renderConflictList();
  el.conflictBody.hidden = false;
  el.conflictEmpty.hidden = true;
  el.conflictMeta.innerHTML =
    '<div><span class="muted">' +
    escapeHtml(t("conflicts.target")) +
    '</span> <code>' +
    escapeHtml(entry.target) +
    "</code></div>" +
    '<div><span class="muted">' +
    escapeHtml(t("conflicts.copyFile")) +
    '</span> <code>' +
    escapeHtml(entry.path) +
    "</code></div>" +
    '<div><span class="muted">' +
    escapeHtml(t("conflicts.modified")) +
    "</span> " +
    escapeHtml(formatTimestamp(entry.modified_at)) +
    "</div>";

  if (entry.target_content === null || entry.conflict_content === null) {
    const notice = '<p class="muted pad-note">' + escapeHtml(t("conflicts.binary")) + "</p>";
    el.diffLeft.innerHTML = notice;
    el.diffRight.innerHTML = notice;
    return;
  }
  const rows = diffRows(splitLines(entry.target_content), splitLines(entry.conflict_content));
  el.diffLeft.innerHTML = rowsHtml(rows.left);
  el.diffRight.innerHTML = rowsHtml(rows.right);
  el.diffLeft.scrollTop = 0;
  el.diffRight.scrollTop = 0;
}

async function loadConflicts(): Promise<void> {
  try {
    const list = await api.listConflicts();
    conflicts = list;
    if (activeConflict && !list.some((entry) => entry.path === activeConflict)) {
      activeConflict = null;
    }
    renderConflictList();
    if (!activeConflict && list.length > 0) {
      const first = list[0];
      if (first) {
        renderDiff(first);
      }
    }
  } catch (error) {
    el.conflictList.innerHTML =
      '<p class="muted empty">' + escapeHtml(t("conflicts.loadFail", { error: errorText(error) })) + "</p>";
    el.conflictEmpty.hidden = true;
    el.conflictBody.hidden = true;
  }
}

async function resolveOne(path: string, choice: "local" | "conflict"): Promise<void> {
  const entry = conflicts.find((item) => item.path === path);
  try {
    await api.resolveConflict(path, choice);
    toast(t("conflicts.resolveOk", { target: entry?.target ?? path }));
  } catch (error) {
    toast(t("conflicts.resolveFail", { error: errorText(error) }));
  }
}

async function resolveAll(choice: "local" | "conflict"): Promise<void> {
  const targets = conflicts.map((entry) => entry.path);
  if (targets.length === 0) {
    return;
  }
  const confirmed = await confirmDialog(
    t("modal.confirmTitle"),
    choice === "local"
      ? t("conflicts.keepAllConfirm", { count: targets.length })
      : t("conflicts.takeAllConfirm", { count: targets.length }),
  );
  if (!confirmed) {
    return;
  }
  let done = 0;
  let failed = 0;
  let lastError = "";
  for (const path of targets) {
    try {
      await api.resolveConflict(path, choice);
      done += 1;
    } catch (error) {
      failed += 1;
      lastError = errorText(error);
    }
  }
  if (failed > 0) {
    toast(t("conflicts.failed", { count: failed, error: lastError }));
  } else {
    toast(t("conflicts.done", { count: done }));
  }
  activeConflict = null;
  await loadConflicts();
}

/* ---------- 设备 ---------- */

function renderDevices(): void {
  el.deviceEmpty.hidden = devices.length > 0;
  el.deviceEmpty.textContent = t("devices.empty");
  el.deviceBody.innerHTML = devices
    .map((device) => {
      const state = device.reachable
        ? '<span class="dot ok"></span>' + escapeHtml(t("devices.reachable"))
        : '<span class="dot"></span>' + escapeHtml(t("devices.offline"));
      const self = device.is_self
        ? ' <span class="badge">' + escapeHtml(t("devices.self")) + "</span>"
        : "";
      return (
        "<tr" +
        (device.is_self ? ' class="is-self"' : "") +
        "><td><code>" +
        escapeHtml(device.id) +
        "</code>" +
        self +
        "</td><td>" +
        escapeHtml(device.hostname) +
        "</td><td>" +
        escapeHtml(device.os) +
        "</td><td>" +
        escapeHtml(formatTimestamp(device.heartbeat_ts)) +
        "<br />" +
        state +
        '</td><td><button type="button" class="ghost small danger-text" data-revoke="' +
        escapeHtml(device.id) +
        '"' +
        (device.is_self ? " disabled" : "") +
        ">" +
        escapeHtml(t("devices.revoke")) +
        "</button></td></tr>"
      );
    })
    .join("");
}

async function loadDevices(): Promise<void> {
  try {
    devices = await api.listDevices();
    renderDevices();
  } catch (error) {
    el.deviceBody.innerHTML = "";
    el.deviceEmpty.hidden = false;
    el.deviceEmpty.textContent = t("devices.loadFail", { error: errorText(error) });
  }
}

async function revokeDevice(id: string): Promise<void> {
  const confirmed = await confirmDialog(
    t("devices.revokeConfirmTitle"),
    t("devices.revokeConfirmBody", { id }),
  );
  if (!confirmed) {
    return;
  }
  try {
    await api.revokeDevice(id);
    toast(t("devices.revokeOk", { id }));
    await loadDevices();
  } catch (error) {
    toast(t("devices.revokeFail", { error: errorText(error) }));
  }
}

/* ---------- 备份 ---------- */

function reasonLabel(reason: string): string {
  switch (reason) {
    case "manual":
      return t("settings.backupManual");
    case "pre-restore":
      return t("settings.backupPreRestore");
    default:
      return reason;
  }
}

function renderBackups(): void {
  if (backups.length === 0) {
    el.backupList.innerHTML = '<p class="muted empty">' + escapeHtml(t("settings.backupEmpty")) + "</p>";
    return;
  }
  el.backupList.innerHTML = backups
    .map(
      (backup) =>
        '<div class="backup-row">' +
        "<div><code>" +
        escapeHtml(backup.id) +
        "</code> " +
        '<span class="badge' +
        (backup.is_pre_restore ? " warn" : "") +
        '">' +
        escapeHtml(reasonLabel(backup.reason)) +
        "</span></div>" +
        '<div class="muted small">' +
        escapeHtml(backup.created_at_text || formatTimestamp(backup.created_at)) +
        " · " +
        escapeHtml(t("settings.backupFiles", { count: backup.files })) +
        " · " +
        escapeHtml(formatBytes(backup.bytes)) +
        "</div>" +
        '<button type="button" class="ghost small" data-restore="' +
        escapeHtml(backup.id) +
        '">' +
        escapeHtml(t("settings.backupRestore")) +
        "</button>" +
        "</div>",
    )
    .join("");
}

async function loadBackups(): Promise<void> {
  try {
    backups = await api.listBackups();
    renderBackups();
  } catch (error) {
    el.backupList.innerHTML =
      '<p class="muted empty">' + escapeHtml(t("settings.backupFail", { error: errorText(error) })) + "</p>";
  }
}

async function createBackup(): Promise<void> {
  el.backupCreate.disabled = true;
  try {
    const created = await api.createBackup();
    toast(t("settings.backupCreated", { id: created.id }));
    await loadBackups();
  } catch (error) {
    toast(t("settings.backupFail", { error: errorText(error) }));
  } finally {
    el.backupCreate.disabled = false;
  }
}

async function restoreBackup(id: string): Promise<void> {
  const confirmed = await confirmDialog(
    t("settings.restoreConfirmTitle"),
    t("settings.restoreConfirmBody", { id }),
  );
  if (!confirmed) {
    return;
  }
  try {
    const result = await api.restoreBackup(id);
    toast(t("settings.restoreOk", { id: result.backup_id, count: result.restored }));
    if (result.pre_restore_id) {
      localLog(t("settings.restorePre", { id: result.pre_restore_id }));
    }
    await loadBackups();
  } catch (error) {
    toast(t("settings.backupFail", { error: errorText(error) }));
  }
}

/* ---------- 诊断包 / 更新 ---------- */

async function exportDiagnostics(): Promise<void> {
  el.diag.disabled = true;
  try {
    const result = await api.exportDiagnostics();
    el.diagPath.textContent = result.path;
    el.diagResult.hidden = false;
    el.diagCopy.textContent = t("settings.diagCopy");
    toast(t("settings.diagDone", { path: result.path }));
  } catch (error) {
    toast(t("settings.diagFail", { error: errorText(error) }));
  } finally {
    el.diag.disabled = false;
  }
}

function renderUpdate(): void {
  el.updateVersion.textContent = t("settings.version", { version: appVersion });
}

async function checkUpdate(): Promise<void> {
  el.updateCheck.disabled = true;
  el.updateStatus.textContent = t("settings.updateChecking");
  el.updatePanel.hidden = true;
  try {
    const info = await api.checkUpdate();
    if (info.available && info.version) {
      pendingUpdate = info.version;
      el.updateStatus.textContent = t("settings.updateAvailable", {
        version: info.version,
        current: info.current_version,
      });
      el.updateNotes.textContent = info.notes ?? "";
      el.updatePanel.hidden = false;
    } else {
      pendingUpdate = null;
      el.updateStatus.textContent = t("settings.updateNone", { version: info.current_version });
    }
  } catch (error) {
    pendingUpdate = null;
    el.updateStatus.textContent = t("settings.updateFail", { error: errorText(error) });
  } finally {
    el.updateCheck.disabled = false;
  }
}

async function installUpdate(): Promise<void> {
  if (!pendingUpdate) {
    return;
  }
  const confirmed = await confirmDialog(
    t("settings.updateConfirmTitle"),
    t("settings.updateConfirmBody", { version: pendingUpdate }),
  );
  if (!confirmed) {
    return;
  }
  el.updateInstall.disabled = true;
  el.updateStatus.textContent = t("settings.updateInstalling");
  try {
    const result = await api.installUpdate();
    el.updateStatus.textContent = t("settings.updateDone", { message: result.message });
    el.updatePanel.hidden = true;
  } catch (error) {
    el.updateStatus.textContent = t("settings.updateFail", { error: errorText(error) });
  } finally {
    el.updateInstall.disabled = false;
  }
}

/* ---------- 实时同步 ---------- */

function renderWatch(): void {
  el.watchToggle.checked = watchState.running;
  el.watchStatus.textContent = watchState.running ? t("sync.watchOn") : t("sync.watchOff");
  el.watchStatus.className = "watch-state" + (watchState.running ? " on" : "");
  el.watchLast.textContent = watchState.last_event_at
    ? t("sync.watchLast", { time: formatTimestamp(watchState.last_event_at) })
    : t("sync.watchNever");
}

async function toggleWatch(enabled: boolean): Promise<void> {
  el.watchToggle.disabled = true;
  try {
    watchState = enabled ? await api.watchStart() : await api.watchStop();
    renderWatch();
    localLog(watchState.running ? t("sync.watchOn") : t("sync.watchOff"));
  } catch (error) {
    el.watchToggle.checked = !enabled;
    toast(t("sync.watchFail", { error: errorText(error) }));
  } finally {
    el.watchToggle.disabled = false;
  }
}

async function loadWatchStatus(): Promise<void> {
  try {
    watchState = await api.watchStatus();
    renderWatch();
  } catch {
    renderWatch();
  }
}

/* ---------- 服务商预设 ---------- */

function guessPreset(url: string): string {
  const raw = url.toLowerCase();
  if (!raw) {
    try {
      return localStorage.getItem(PRESET_KEY) ?? "custom";
    } catch {
      return "custom";
    }
  }
  if (raw.includes("jianguoyun") || raw.includes("nutstore")) {
    return "jianguoyun";
  }
  if (raw.includes("remote.php/dav")) {
    return "nextcloud";
  }
  if (raw.includes("/webdav")) {
    return "synology";
  }
  if (raw.includes(":5244")) {
    return "alist";
  }
  return "custom";
}

function fillPresetSelect(select: HTMLSelectElement, selected: string): void {
  select.innerHTML = presets
    .map(
      (preset) =>
        '<option value="' + escapeHtml(preset.id) + '">' + escapeHtml(presetLabel(preset.id)) + "</option>",
    )
    .join("");
  if (presets.some((preset) => preset.id === selected)) {
    select.value = selected;
  } else if (select.options.length > 0) {
    select.selectedIndex = 0;
  }
}

async function loadPresets(): Promise<void> {
  try {
    presets = await api.listPresets();
  } catch {
    presets = [];
  }
  const selected = config ? guessPreset(config.webdav.url) : "custom";
  fillPresetSelect(el.preset, selected);
  fillPresetSelect(el.wizardPreset, selected);
  el.presetHint.textContent = presetHint(selected);
  el.wizardPresetHint.textContent = presetHint(selected);
}

async function applyPresetTo(
  presetId: string,
  urlInput: HTMLInputElement,
  hintNode: HTMLElement,
  select: HTMLSelectElement,
): Promise<void> {
  try {
    localStorage.setItem(PRESET_KEY, presetId);
  } catch {
    // ignore storage failures
  }
  hintNode.textContent = presetHint(presetId);
  const raw = urlInput.value.trim();
  if (!raw) {
    select.value = presetId;
    return;
  }
  try {
    const applied = await api.applyPreset(presetId, raw);
    if (applied.normalized_url) {
      urlInput.value = applied.normalized_url;
      toast(t("conn.normalized", { url: applied.normalized_url }));
    }
  } catch (error) {
    toast(t("conn.presetUnknown") + "：" + errorText(error));
  }
  select.value = presetId;
}

/* ---------- 同步设置向导（首启 / --setup 共用） ---------- */

function wizardTitleText(): string {
  return t(wizard.mode === "settings" ? "wizard.titleSetup" : "wizard.titleFirstRun");
}

function setWizardPolicy(policy: "preview" | "normal"): void {
  wizard.policy = policy;
  for (const input of el.wizardStrategy.querySelectorAll<HTMLInputElement>(
    'input[name="wizard-strategy"]',
  )) {
    input.checked = input.value === policy;
  }
  el.wizardPolicyHint.textContent = t(policy === "normal" ? "wizard.normalHint" : "wizard.previewHint");
}

function setWizardStep(step: number): void {
  wizard.step = step;
  for (const section of el.wizardSteps) {
    section.classList.toggle("is-active", section.dataset.wizardStep === String(step));
  }
  el.wizardStepLabel.textContent = step >= 5 ? "" : t("wizard.step", { current: step, total: 4 });
  el.wizardBar.style.width = Math.min(100, (Math.min(step, 4) / 4) * 100) + "%";
  el.wizardBack.disabled = step <= 1 || step >= 5;
  el.wizardSkip.hidden = step !== 3;
  el.wizardNext.disabled = false;
  el.wizardNext.textContent =
    step >= 5 ? t("wizard.enter") : step === 4 ? t("wizard.finish") : t("common.next");
}

function openWizard(mode: "first-run" | "settings" = "first-run"): void {
  wizard = { step: 1, policy: "preview", mode, finished: false };
  el.wizardTitle.textContent = wizardTitleText();
  const base = config;
  if (base) {
    el.wizardRime.value = base.rime_dir ?? "";
    el.wizardDevice.value = base.device_id;
    el.wizardUrl.value = base.webdav.url;
    el.wizardUser.value = base.webdav.username;
    el.wizardPass.value = "";
    el.wizardDir.value = base.webdav.remote_dir;
    const selected = guessPreset(base.webdav.url);
    el.wizardPreset.value = selected;
    el.wizardPresetHint.textContent = presetHint(selected);
    setWizardPolicy(base.first_sync_policy === "normal" ? "normal" : "preview");
  } else {
    el.wizardDevice.value = "";
    setWizardPolicy("preview");
  }
  el.wizardResult.hidden = true;
  el.wizardSummary.hidden = true;
  el.wizardDetails.hidden = true;
  el.wizardStatus.hidden = true;
  el.wizard.hidden = false;
  setWizardStep(1);
  void autoDetectWizard();
}

async function autoDetectWizard(): Promise<void> {
  el.wizardRimeStatus.textContent = t("sync.detecting");
  try {
    const detection = await api.detectRimeDir();
    if (!el.wizardRime.value) {
      el.wizardRime.value = detection.path;
    }
    el.wizardRimeStatus.textContent = t("sync.source", { source: detection.source });
  } catch {
    el.wizardRimeStatus.textContent = t("sync.notDetected");
  }
}

function wizardDraft(): Config {
  return {
    ...requireConfig(),
    rime_dir: el.wizardRime.value.trim() || null,
    device_id: el.wizardDevice.value.trim(),
    first_sync_policy: wizard.policy,
    webdav: {
      url: el.wizardUrl.value.trim(),
      username: el.wizardUser.value.trim(),
      password: el.wizardPass.value,
      remote_dir: el.wizardDir.value.trim(),
    },
  };
}

async function saveWizardDraft(): Promise<boolean> {
  try {
    const saved = await api.setConfig(wizardDraft());
    renderConfig(saved);
    // 留空设备名时后端会自动生成，回填给界面避免下次保存又生成新的。
    el.wizardDevice.value = saved.device_id;
    void refreshSecretStatus();
    return true;
  } catch (error) {
    toast(t("conn.saveFail", { error: errorText(error) }));
    return false;
  }
}

function wizardDetailsText(result: ProbeResult): string[] {
  const lines = [
    t("wizard.testLatency", { ms: result.latency_ms }),
    t("wizard.testServer", { server: result.server_type || result.server || "?" }),
    t("wizard.testRemoteDir", { dir: result.remote_dir || "/" }),
  ];
  if (result.remote_dir_missing) {
    lines.push(t("wizard.testDirMissing"));
  }
  return lines;
}

async function wizardTest(): Promise<void> {
  if (!el.wizardUrl.value.trim() || !el.wizardUser.value.trim()) {
    setStatus(el.wizardStatus, "err", t("wizard.required"));
    return;
  }
  if (!(await saveWizardDraft())) {
    return;
  }
  el.wizardTest.disabled = true;
  el.wizardDetails.hidden = true;
  setStatus(el.wizardStatus, "idle", t("wizard.testing"));
  try {
    const result = await api.testConnection();
    const lines = wizardDetailsText(result);
    if (!result.ok) {
      setStatus(el.wizardStatus, "err", t("wizard.testFail", { error: result.message || t("conn.testFail") }));
    } else {
      // 写权限只在 normal 下必需；preview 下写失败只提示，不阻塞流程。
      const probe = await api.probeWrite();
      if (probe.ok) {
        setStatus(el.wizardStatus, "ok", t("wizard.testOk"));
      } else {
        lines.push(
          t("wizard.testWriteFail", {
            step: probe.step,
            status: probe.http_status,
            hint: probe.hint ? " · " + probe.hint : "",
          }),
        );
        setStatus(el.wizardStatus, "err", t("wizard.testWriteOnlyFail"));
      }
    }
    el.wizardDetails.hidden = false;
    el.wizardDetails.textContent = lines.join("\n");
  } catch (error) {
    setStatus(el.wizardStatus, "err", t("wizard.testFail", { error: errorText(error) }));
  } finally {
    el.wizardTest.disabled = false;
  }
}

function closeWizard(): void {
  el.wizard.hidden = true;
  el.wizardPass.value = "";
  wizard.finished = true;
  localLog(t("wizard.done"));
  renderFirstSyncBanner();
  void loadWatchStatus();
  void loadBackups();
  void loadConflicts();
}

// 「完成」：保存完整配置（含 first_sync_policy），按需执行一次首次同步并展示 pulled/pushed/mode。
async function wizardFinish(): Promise<void> {
  if (!(await saveWizardDraft())) {
    return;
  }
  el.wizardNext.disabled = true;
  el.wizardBack.disabled = true;
  el.wizardResult.hidden = true;
  let summary: string | null = null;
  let failed = false;
  if (el.wizardRunFirst.checked) {
    el.wizardResult.hidden = false;
    el.wizardResult.textContent = t("wizard.running");
    try {
      // preview 走只拉分支；normal 是用户显式选择的双向。
      const result = await api.firstTransport(wizard.policy === "normal" ? "bidirectional" : "pull");
      const report = result.report;
      const lines = [
        report.ok
          ? t("wizard.firstDone")
          : t("wizard.firstFail", { error: report.error ?? t("sync.verdictFail") }),
        t("wizard.firstStats", {
          pulled: report.pulled,
          pushed: report.pushed,
          mode: report.mode ?? result.strategy,
        }),
      ];
      if (report.backup_id) {
        lines.push(t("sync.lastBackup", { id: report.backup_id }));
      }
      if (result.note) {
        lines.push(result.note);
      }
      el.wizardResult.textContent = lines.join("\n");
      summary = lines.slice(0, 2).join("\n");
      failed = !report.ok;
    } catch (error) {
      failed = true;
      el.wizardResult.textContent = t("wizard.firstFail", { error: errorText(error) });
    }
  } else {
    summary = t("wizard.firstSkipped", {
      mode: t(wizard.policy === "normal" ? "wizard.normal" : "wizard.preview"),
    });
  }
  el.wizardNext.disabled = false;
  if (failed) {
    el.wizardBack.disabled = false;
    return;
  }
  wizard.finished = true;
  const notes: string[] = [];
  if (summary) {
    notes.push(summary);
  }
  if (wizard.policy !== "normal") {
    notes.push(t("wizard.switchLater"));
  }
  el.wizardSummary.hidden = false;
  el.wizardSummary.textContent = notes.join("\n\n");
  setWizardStep(5);
}

async function wizardNext(): Promise<void> {
  switch (wizard.step) {
    case 1: {
      const value = el.wizardRime.value.trim();
      if (!value) {
        el.wizardRimeStatus.textContent = t("wizard.pathMissing");
        return;
      }
      setWizardStep(2);
      return;
    }
    case 2: {
      if (!el.wizardUrl.value.trim() || !el.wizardUser.value.trim()) {
        toast(t("wizard.required"));
        return;
      }
      if (!(await saveWizardDraft())) {
        return;
      }
      setWizardStep(3);
      return;
    }
    case 3:
      setWizardStep(4);
      return;
    case 4:
      await wizardFinish();
      return;
    default:
      closeWizard();
      return;
  }
}

function wizardBack(): void {
  if (wizard.step > 1 && wizard.step < 5) {
    setWizardStep(wizard.step - 1);
  }
}

/* ---------- 事件绑定 ---------- */

function bindEvents(): void {
  for (const tab of el.tabs) {
    tab.addEventListener("click", () => {
      const name = tab.dataset.panel;
      if (name) {
        switchPanel(name);
      }
    });
  }

  el.togglePass.addEventListener("click", () => {
    const showing = el.pass.type === "text";
    el.pass.type = showing ? "password" : "text";
    el.togglePass.textContent = showing ? t("conn.show") : t("conn.hide");
    el.togglePass.setAttribute("aria-pressed", String(!showing));
  });

  el.saveConn.addEventListener("click", () => {
    void saveConnection().catch(() => undefined);
  });
  el.test.addEventListener("click", () => {
    void testConnection();
  });
  el.probe.addEventListener("click", () => {
    void probeWrite();
  });
  el.listRemote.addEventListener("click", () => {
    void listRemote();
  });
  el.preset.addEventListener("change", () => {
    void applyPresetTo(el.preset.value, el.url, el.presetHint, el.preset);
  });

  el.detect.addEventListener("click", () => {
    void detectRimeDir();
  });
  el.browseRime.addEventListener("click", () => {
    void pickRimeDir(el.rimeDir, el.rimeDirSource, true);
  });
  el.wizardBrowse.addEventListener("click", () => {
    void pickRimeDir(el.wizardRime, el.wizardRimeStatus, false);
  });
  el.rimeDir.addEventListener("change", () => {
    void saveRimeDir();
  });
  el.watchToggle.addEventListener("change", () => {
    void toggleWatch(el.watchToggle.checked);
  });
  el.firstSyncSwitch.addEventListener("click", () => {
    void switchToTwoWay();
  });
  el.transport.addEventListener("click", () => {
    void run("transport");
  });
  el.apply.addEventListener("click", () => {
    void run("apply");
  });
  el.cancel.addEventListener("click", () => {
    void api.cancel().then(
      () => {
        localLog(t("sync.cancelRequested"));
        setFooter(t("sync.cancelled"));
      },
      (error: unknown) => {
        toast(t("sync.cancelFail", { error: errorText(error) }));
      },
    );
  });

  el.refreshLog.addEventListener("click", () => {
    void loadLog();
  });
  el.clearLog.addEventListener("click", () => {
    logCache = [];
    el.log.textContent = "";
  });

  el.conflictsRefresh.addEventListener("click", () => {
    void loadConflicts();
  });
  el.conflictsKeepAll.addEventListener("click", () => {
    void resolveAll("local");
  });
  el.conflictsTakeAll.addEventListener("click", () => {
    void resolveAll("conflict");
  });
  el.conflictList.addEventListener("click", (event) => {
    const target = (event.target as HTMLElement).closest<HTMLElement>("[data-path]");
    const path = target?.dataset.path;
    if (!path) {
      return;
    }
    const entry = conflicts.find((item) => item.path === path);
    if (entry) {
      renderDiff(entry);
    }
  });
  el.conflictLocal.addEventListener("click", () => {
    if (activeConflict) {
      void resolveOne(activeConflict, "local").then(loadConflicts);
    }
  });
  el.conflictCopy.addEventListener("click", () => {
    if (!activeConflict) {
      return;
    }
    const entry = conflicts.find((item) => item.path === activeConflict);
    if (!entry) {
      return;
    }
    void confirmDialog(t("modal.confirmTitle"), t("conflicts.takeConfirm", { target: entry.target })).then(
      (confirmed) => {
        if (confirmed && activeConflict) {
          void resolveOne(activeConflict, "conflict").then(loadConflicts);
        }
      },
    );
  });

  let syncingScroll = false;
  const syncScroll = (source: HTMLDivElement, follower: HTMLDivElement) => {
    source.addEventListener("scroll", () => {
      if (syncingScroll) {
        return;
      }
      syncingScroll = true;
      follower.scrollTop = source.scrollTop;
      syncingScroll = false;
    });
  };
  syncScroll(el.diffLeft, el.diffRight);
  syncScroll(el.diffRight, el.diffLeft);

  el.devicesRefresh.addEventListener("click", () => {
    void loadDevices();
  });
  el.deviceBody.addEventListener("click", (event) => {
    const target = (event.target as HTMLElement).closest<HTMLElement>("[data-revoke]");
    const id = target?.dataset.revoke;
    if (id) {
      void revokeDevice(id);
    }
  });

  el.saveSettings.addEventListener("click", () => {
    void persist(settingsDraft(), t("settings.saved")).catch((error: unknown) => {
      toast(t("conn.saveFail", { error: errorText(error) }));
    });
  });
  el.regenDevice.addEventListener("click", () => {
    void confirmDialog(t("modal.confirmTitle"), t("settings.regenConfirm")).then((confirmed) => {
      if (!confirmed) {
        return;
      }
      void api
        .setConfig({ ...requireConfig(), device_id: "" })
        .then((saved) => {
          renderConfig(saved);
          toast(t("settings.regenDone"));
          localLog(t("settings.regenDone") + ": " + saved.device_id);
        })
        .catch((error: unknown) => {
          toast(t("settings.regenFail", { error: errorText(error) }));
        });
    });
  });

  el.backupCreate.addEventListener("click", () => {
    void createBackup();
  });
  el.backupList.addEventListener("click", (event) => {
    const target = (event.target as HTMLElement).closest<HTMLElement>("[data-restore]");
    const id = target?.dataset.restore;
    if (id) {
      void restoreBackup(id);
    }
  });

  el.diag.addEventListener("click", () => {
    void exportDiagnostics();
  });
  el.diagCopy.addEventListener("click", () => {
    void copyText(el.diagPath.textContent ?? "").then((ok) => {
      toast(ok ? t("common.copied") : t("settings.diagFail", { error: "clipboard" }));
    });
  });

  el.language.addEventListener("change", () => {
    setLang(el.language.value === "en" ? "en" : "zh");
    applyI18n();
    refreshLanguageDependent();
  });

  el.updateCheck.addEventListener("click", () => {
    void checkUpdate();
  });
  el.updateInstall.addEventListener("click", () => {
    void installUpdate();
  });

  el.modalCancel.addEventListener("click", () => settleModal(false));
  el.modalConfirm.addEventListener("click", () => settleModal(true));
  el.modal.addEventListener("click", (event) => {
    if (event.target === el.modal) {
      settleModal(false);
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && !el.modal.hidden) {
      settleModal(false);
    }
  });

  el.wizardDetect.addEventListener("click", () => {
    void autoDetectWizard();
  });
  el.wizardPreset.addEventListener("change", () => {
    void applyPresetTo(el.wizardPreset.value, el.wizardUrl, el.wizardPresetHint, el.wizardPreset);
  });
  el.wizardTest.addEventListener("click", () => {
    void wizardTest();
  });
  el.wizardBack.addEventListener("click", wizardBack);
  el.wizardNext.addEventListener("click", () => {
    void wizardNext();
  });
  el.wizardSkip.addEventListener("click", () => {
    setWizardStep(4);
  });
  el.wizardStrategy.addEventListener("change", () => {
    const checked = el.wizardStrategy.querySelector<HTMLInputElement>(
      'input[name="wizard-strategy"]:checked',
    );
    setWizardPolicy(checked?.value === "normal" ? "normal" : "preview");
  });

  window.setInterval(() => {
    if (!config || busy) {
      return;
    }
    const minutes = config.transport_interval_minutes;
    if (minutes <= 0) {
      return;
    }
    if (Date.now() - lastRun < minutes * 60_000) {
      return;
    }
    void run("transport", true);
  }, 20_000);
}

/* ---------- 语言切换后重绘动态文案 ---------- */

let lastReport: Report | null = null;

function refreshLanguageDependent(): void {
  document.title = t("app.name");
  el.togglePass.textContent = el.pass.type === "text" ? t("conn.hide") : t("conn.show");
  el.diagCopy.textContent = t("settings.diagCopy");
  el.language.value = getLang();
  el.presetHint.textContent = presetHint(el.preset.value);
  el.wizardPresetHint.textContent = presetHint(el.wizardPreset.value);
  el.rimeDirSource.textContent = config?.rime_dir ? t("sync.saved") : t("sync.notDetected");
  el.progressLabel.textContent = t("sync.progressWaiting");
  el.conflictsCount.textContent = t("conflicts.count", { count: conflicts.length });
  renderFirstSyncBanner();
  renderWatch();
  renderConflictList();
  renderDevices();
  renderBackups();
  renderUpdate();
  if (lastReport) {
    renderReport(lastReport);
  }
  if (secretState) {
    el.secretStatus.textContent =
      (secretState.backend === "keyring" ? t("conn.secretKeyring") : t("conn.secretFile")) +
      " · " +
      (secretState.has_password ? t("conn.secretSaved") : t("conn.secretMissing"));
  }
  if (!el.wizard.hidden) {
    el.wizardTitle.textContent = wizardTitleText();
    el.wizardPolicyHint.textContent = t(
      wizard.policy === "normal" ? "wizard.normalHint" : "wizard.previewHint",
    );
    el.wizardNext.textContent =
      wizard.step >= 5
        ? t("wizard.enter")
        : wizard.step === 4
          ? t("wizard.finish")
          : t("common.next");
    el.wizardStepLabel.textContent =
      wizard.step >= 5 ? "" : t("wizard.step", { current: wizard.step, total: 4 });
  }
  if (config) {
    el.footerConfig.textContent = config.device_id ? t("footer.device", { id: config.device_id }) : "";
  }
  if (!lastReport) {
    setFooter(t("footer.ready"));
  }
}

/* ---------- 订阅与启动 ---------- */

async function loadLog(): Promise<void> {
  try {
    const lines = await api.getLog(200);
    logCache = lines.slice(-400);
    el.log.textContent = logCache.join("\n");
    el.log.scrollTop = el.log.scrollHeight;
  } catch (error) {
    localLog("读取日志失败：" + errorText(error));
  }
}

async function subscribe(): Promise<void> {
  await listen<Progress>("sync://progress", (event) => {
    renderProgress(event.payload);
  });
  await listen<Report>("sync://done", (event) => {
    lastReport = event.payload;
    renderReport(event.payload);
  });
  await listen<WatchEvent>("sync://watch", (event) => {
    const payload = event.payload;
    watchState = { running: true, last_event_at: payload.at };
    renderWatch();
    appendLog("[" + timeStamp() + "] " + payload.kind + " " + payload.path);
  });
}

async function init(): Promise<void> {
  bindEvents();
  el.language.value = getLang();
  applyI18n();
  document.title = t("app.name");
  el.progressLabel.textContent = t("sync.progressWaiting");
  el.rimeDirSource.textContent = t("sync.notDetected");
  renderWatch();
  el.diagCopy.textContent = t("settings.diagCopy");
  el.togglePass.textContent = t("conn.show");
  renderUpdate();

  void getVersion().then((version) => {
    appVersion = version;
    renderUpdate();
  });

  try {
    // --setup：安装完成后由安装器带参数启动，无论是否首次运行都强制进设置向导。
    const forcedSetup = await api.takeSetupRequest();
    const needsSetup = forcedSetup || (await api.needsSetup());
    const loaded = await api.getConfig();
    renderConfig(loaded);
    el.rimeDirSource.textContent = loaded.rime_dir ? t("sync.saved") : t("sync.notDetected");
    // 启动时校验：配置里的目录被删掉后（后端已重置为 null），自动重新探测一次。
    if (!loaded.rime_dir) {
      void detectRimeDir(el.rimeDir, el.rimeDirSource, true);
    }
    await loadPresets();
    await subscribe();
    await loadLog();
    await refreshSecretStatus();
    await refreshNetwork();
    await loadWatchStatus();
    await loadBackups();
    localLog("界面已就绪");
    setFooter(t("footer.ready"));
    if (needsSetup) {
      openWizard(forcedSetup ? "settings" : "first-run");
    }
  } catch (error) {
    const text = errorText(error);
    setStatus(el.connStatus, "err", t("footer.backendDown") + ": " + text);
    localLog("初始化失败：" + text);
    setFooter(t("footer.backendDown"));
  }
  lastRun = Date.now();
}

void init();

import "./styles.css";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  type Config,
  type Progress,
  type Report,
} from "./api";

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

  url: byId<HTMLInputElement>("dav-url"),
  user: byId<HTMLInputElement>("dav-user"),
  pass: byId<HTMLInputElement>("dav-pass"),
  dir: byId<HTMLInputElement>("dav-dir"),
  togglePass: byId<HTMLButtonElement>("toggle-pass"),
  saveConn: byId<HTMLButtonElement>("btn-save-conn"),
  test: byId<HTMLButtonElement>("btn-test"),
  listRemote: byId<HTMLButtonElement>("btn-list-remote"),
  connStatus: byId<HTMLDivElement>("conn-status"),
  remoteList: byId<HTMLDivElement>("remote-list"),

  rimeDir: byId<HTMLInputElement>("rime-dir"),
  rimeDirSource: byId<HTMLParagraphElement>("rime-dir-source"),
  detect: byId<HTMLButtonElement>("btn-detect"),
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
  reportNote: byId<HTMLDivElement>("report-note"),

  log: byId<HTMLPreElement>("log"),
  refreshLog: byId<HTMLButtonElement>("btn-refresh-log"),
  clearLog: byId<HTMLButtonElement>("btn-clear-log"),

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

  footerStatus: byId<HTMLSpanElement>("footer-status"),
  footerConfig: byId<HTMLSpanElement>("footer-config"),
  toast: byId<HTMLDivElement>("toast"),
};

const PHASES: Record<string, string> = {
  start: "开始同步",
  pull: "拉取快照",
  merge: "合并配置",
  push: "推送快照",
  deploy: "部署生效",
  done: "完成",
};

let config: Config | null = null;
let busy = false;
let lastRun = Date.now();
let logCache: string[] = [];
let toastTimer = 0;

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

function timeStamp(): string {
  const now = new Date();
  return [now.getHours(), now.getMinutes(), now.getSeconds()]
    .map((part) => String(part).padStart(2, "0"))
    .join(":");
}

function toast(message: string): void {
  el.toast.textContent = message;
  el.toast.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    el.toast.hidden = true;
  }, 2200);
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

function switchPanel(name: string): void {
  for (const tab of el.tabs) {
    const active = tab.dataset.panel === name;
    tab.classList.toggle("is-active", active);
    tab.setAttribute("aria-selected", String(active));
  }
  for (const panel of el.panels) {
    panel.classList.toggle("is-active", panel.id === "panel-" + name);
  }
}

function renderConfig(next: Config): void {
  config = next;
  el.url.value = next.webdav.url;
  el.user.value = next.webdav.username;
  el.pass.value = next.webdav.password;
  el.dir.value = next.webdav.remote_dir;
  el.rimeDir.value = next.rime_dir ?? "";
  el.interval.value = String(next.transport_interval_minutes);
  el.deviceId.value = next.device_id;
  el.kindUserdict.checked = next.sync_kinds.userdict;
  el.kindYaml.checked = next.sync_kinds.custom_yaml;
  el.kindDict.checked = next.sync_kinds.dict_sources;
  el.excludes.value = next.exclude_globs.join("\n");
  el.stopFcitx5.checked = next.stop_fcitx5_during_apply;
  el.footerConfig.textContent = next.device_id ? "设备 " + next.device_id : "";
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
      remote_dir: el.dir.value.trim() || "rime-sync",
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
  toast(message);
}

async function saveConnection(notify = true): Promise<void> {
  try {
    const saved = await api.setConfig(connectionDraft());
    renderConfig(saved);
    if (notify) {
      toast("连接配置已保存");
    }
  } catch (error) {
    setStatus(el.connStatus, "err", "保存失败：" + errorText(error));
    throw error;
  }
}

function renderProgress(progress: Progress): void {
  const label = PHASES[progress.phase] ?? progress.phase;
  el.progressLabel.textContent =
    progress.phase === "done"
      ? "完成"
      : label + (progress.message ? " · " + progress.message : "");
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

  const notes: string[] = [];
  if (report.error) {
    notes.push("错误：" + report.error);
  }
  if (report.conflicts.length > 0) {
    notes.push("冲突：" + report.conflicts.slice(0, 4).join("，"));
  }
  if (report.skipped.length > 0) {
    notes.push("跳过 " + report.skipped.length + " 项（详见日志）");
  }
  el.reportNote.hidden = notes.length === 0;
  el.reportNote.textContent = notes.join("\n");

  const verdict = report.ok ? "成功" : "失败";
  setFooter("上次同步 " + timeStamp() + " · " + verdict + " · " + report.duration_ms + " ms");
  localLog("同步" + verdict + "：拉取 " + report.pulled + "，推送 " + report.pushed + "，合并 " + report.merged + "，冲突 " + report.conflicts.length);
  if (!report.ok) {
    el.progressLabel.textContent = "同步失败";
  }
}

function setBusy(value: boolean, label = "同步中…"): void {
  busy = value;
  el.busy.hidden = !value;
  el.busyText.textContent = label;
  el.transport.disabled = value;
  el.apply.disabled = value;
  el.cancel.disabled = !value;
  el.detect.disabled = value;
  el.saveConn.disabled = value;
  el.test.disabled = value;
}

function confirmApply(): boolean {
  return window.confirm(
    "「立即合并生效」会执行：\n" +
      "1. 先搬运远端变更（pull → merge → push）\n" +
      "2. 触发 librime 真正合并并重新部署\n" +
      "3. Linux 下会临时停止 fcitx5，结束后再启动\n\n" +
      "输入法会短暂中断，期间无法输入中文。继续？",
  );
}

async function run(mode: "transport" | "apply", automatic = false): Promise<void> {
  if (busy) {
    return;
  }
  if (mode === "apply" && !confirmApply()) {
    return;
  }
  setBusy(true, mode === "apply" ? "合并生效中…" : "搬运中…");
  el.progress.classList.add("is-running");
  el.progressLabel.textContent = mode === "apply" ? "准备合并…" : "准备搬运…";
  el.progressCount.textContent = "";
  el.progressFill.style.width = "8%";
  localLog(mode === "apply" ? "开始合并生效" : automatic ? "开始定时搬运" : "开始搬运");

  try {
    const report = mode === "apply" ? await api.apply() : await api.transport();
    renderReport(report);
    if (!report.ok && report.error === "cancelled by user") {
      setFooter("已取消");
    }
  } catch (error) {
    const text = errorText(error);
    setStatus(el.connStatus, "err", "同步失败：" + text);
    localLog("同步失败：" + text);
    setFooter("同步失败");
    toast("同步失败：" + text);
  } finally {
    lastRun = Date.now();
    setBusy(false);
    el.progress.classList.remove("is-running");
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
    el.rimeDirSource.textContent = value ? "已手动保存目录" : "尚未设置目录";
  } catch (error) {
    toast("保存目录失败：" + errorText(error));
  }
}

async function detectRimeDir(): Promise<void> {
  el.detect.disabled = true;
  el.rimeDirSource.textContent = "正在探测…";
  try {
    const detection = await api.detectRimeDir();
    el.rimeDir.value = detection.path;
    el.rimeDirSource.textContent = "来源：" + detection.source;
    const saved = await api.setConfig({ ...requireConfig(), rime_dir: detection.path });
    renderConfig(saved);
    toast("已探测到 Rime 目录");
  } catch (error) {
    el.rimeDirSource.textContent = "探测失败";
    toast("探测失败：" + errorText(error));
  } finally {
    el.detect.disabled = busy;
  }
}

async function testConnection(): Promise<void> {
  el.test.disabled = true;
  setStatus(el.connStatus, "idle", "正在测试连接…");
  try {
    await saveConnection(false);
    const result = await api.testConnection();
    const lines = [
      result.ok ? "连接成功" : "连接失败",
      "延迟 " + result.latency_ms + " ms",
      "服务器 " + result.server_type,
      "DAV " + (result.dav_class || "未声明"),
      result.remote_dir_exists ? "远端目录已存在" : "远端目录将在首次同步时创建",
    ];
    if (result.message) {
      lines.push(result.message);
    }
    setStatus(el.connStatus, result.ok ? "ok" : "err", lines.join(" · "));
    localLog("测试连接：" + lines.join(" · "));
  } catch (error) {
    const text = errorText(error);
    setStatus(el.connStatus, "err", "连接失败：" + text);
    localLog("连接失败：" + text);
  } finally {
    el.test.disabled = busy;
  }
}

async function listRemote(): Promise<void> {
  el.listRemote.disabled = true;
  try {
    const remote = await api.listRemote();
    const parts: string[] = [];
    parts.push("<h3>远端设备（" + remote.devices.length + "）</h3>");
    if (remote.devices.length === 0) {
      parts.push("<p class=\"muted\">暂无设备目录</p>");
    } else {
      parts.push("<ul>" + remote.devices.map((device) => "<li>" + escapeHtml(device.id) + " · " + device.files + " 个快照</li>").join("") + "</ul>");
    }
    parts.push("<h3>共享文件（" + remote.shared.length + "）</h3>");
    if (remote.shared.length === 0) {
      parts.push("<p class=\"muted\">暂无共享文件</p>");
    } else {
      parts.push("<ul>" + remote.shared.slice(0, 30).map((file) => "<li>" + escapeHtml(file.path) + (file.size === null ? "" : " · " + file.size + " B") + "</li>").join("") + "</ul>");
    }
    el.remoteList.innerHTML = parts.join("");
    el.remoteList.hidden = false;
  } catch (error) {
    setStatus(el.connStatus, "err", "读取远端失败：" + errorText(error));
  } finally {
    el.listRemote.disabled = busy;
  }
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

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
    el.togglePass.textContent = showing ? "显示" : "隐藏";
    el.togglePass.setAttribute("aria-pressed", String(!showing));
  });

  el.saveConn.addEventListener("click", () => {
    void saveConnection().catch((error) => {
      toast("保存失败：" + errorText(error));
    });
  });
  el.test.addEventListener("click", () => {
    void testConnection();
  });
  el.listRemote.addEventListener("click", () => {
    void listRemote();
  });

  el.detect.addEventListener("click", () => {
    void detectRimeDir();
  });
  el.rimeDir.addEventListener("change", () => {
    void saveRimeDir();
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
        localLog("已请求取消，等待当前文件处理完成");
        setFooter("已请求取消");
      },
      (error: unknown) => {
        toast("取消失败：" + errorText(error));
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

  el.saveSettings.addEventListener("click", () => {
    void persist(settingsDraft(), "设置已保存").catch((error) => {
      toast("保存失败：" + errorText(error));
    });
  });

  el.regenDevice.addEventListener("click", () => {
    const confirmed = window.confirm(
      "重新生成设备 ID 后，本机会以新的 sync/<设备>/ 目录参与同步。\n" +
        "旧目录不会被删除。继续？",
    );
    if (!confirmed) {
      return;
    }
    void api
      .setConfig({ ...requireConfig(), device_id: "" })
      .then((saved) => {
        renderConfig(saved);
        toast("设备 ID 已重新生成");
        localLog("设备 ID 已重新生成：" + saved.device_id);
      })
      .catch((error: unknown) => {
        toast("生成失败：" + errorText(error));
      });
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

async function subscribe(): Promise<void> {
  await listen<Progress>("sync://progress", (event) => {
    renderProgress(event.payload);
  });
  await listen<Report>("sync://done", (event) => {
    renderReport(event.payload);
  });
}

async function init(): Promise<void> {
  bindEvents();
  try {
    const loaded = await api.getConfig();
    renderConfig(loaded);
    el.rimeDirSource.textContent = loaded.rime_dir ? "已保存的目录" : "尚未探测";
    await subscribe();
    await loadLog();
    localLog("界面已就绪");
    setFooter("就绪");
  } catch (error) {
    const text = errorText(error);
    setStatus(el.connStatus, "err", "无法连接应用后端：" + text);
    localLog("初始化失败：" + text);
    setFooter("后端不可用");
  }
  lastRun = Date.now();
}

void init();

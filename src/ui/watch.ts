import { api, type WatchStatus, type WatchEvent } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText, timeStamp, formatTimestamp } from "./format";
import { toast } from "./toast";
import { appendLog, localLog } from "./logs";

// 实时同步开关与事件回填。

let watchState: WatchStatus = { running: false, last_event_at: null };

export function applyWatchEvent(payload: WatchEvent): void {
  watchState = { running: true, last_event_at: payload.at };
  renderWatch();
  appendLog("[" + timeStamp() + "] " + payload.kind + " " + payload.path);
}

/* ---------- 实时同步 ---------- */

export function renderWatch(): void {
  el.watchToggle.checked = watchState.running;
  el.watchStatus.textContent = watchState.running ? t("sync.watchOn") : t("sync.watchOff");
  el.watchStatus.className = "watch-state" + (watchState.running ? " on" : "");
  el.watchLast.textContent = watchState.last_event_at
    ? t("sync.watchLast", { time: formatTimestamp(watchState.last_event_at) })
    : t("sync.watchNever");
}

export async function toggleWatch(enabled: boolean): Promise<void> {
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

export async function loadWatchStatus(): Promise<void> {
  try {
    watchState = await api.watchStatus();
    renderWatch();
  } catch {
    renderWatch();
  }
}

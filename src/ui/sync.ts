import { api, type Report, type Progress } from "../api";
import { t } from "../i18n";
import { el, setFooter, setStatus } from "./dom";
import { errorText, timeStamp } from "./format";
import { toast } from "./toast";
import { appendLog, localLog } from "./logs";
import { confirmDialog } from "./modal";
import { refreshNetwork } from "./connection";
import { config, busy, plannedActions, setBusyFlag, touchLastRun } from "./state";

// 同步执行：忙碌态、进度与结果报告。

export function renderProgress(progress: Progress): void {
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

export function renderReport(report: Report): void {
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
    notes.push(t("sync.reportConflicts", { count: report.conflicts.length }));
    const shown = report.conflicts.slice(0, 5);
    for (const entry of shown) {
      notes.push("· " + entry);
    }
    if (report.conflicts.length > shown.length) {
      notes.push(t("sync.reportConflictMore", { count: report.conflicts.length - shown.length }));
    }
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

export function setBusy(value: boolean, label = t("busy.default")): void {
  setBusyFlag(value);
  el.busy.hidden = !value;
  el.busyText.textContent = label;
  el.transport.disabled = value;
  el.apply.disabled = value;
  el.plan.disabled = value;
  el.planRun.disabled = value || plannedActions === 0;
  el.planClose.disabled = value;
  el.cancel.disabled = !value;
  el.detect.disabled = value;
  el.saveConn.disabled = value;
  el.test.disabled = value;
  el.probe.disabled = value;
  el.conflictsKeepAll.disabled = value;
  el.conflictsTakeAll.disabled = value;
  el.conflictsTakeRemoteAll.disabled = value;
  el.firstSyncSwitch.disabled = value || !config || config.first_sync_policy === "normal";
}

export async function run(mode: "transport" | "apply", automatic = false): Promise<void> {
  if (busy) {
    return;
  }
  if (mode === "apply") {
    const ok = await confirmDialog(t("sync.applyConfirmTitle"), t("sync.applyConfirmBody"));
    if (!ok) {
      return;
    }
  }
  // 真实同步开始：预览结果已经过期，直接收起。
  el.planCard.hidden = true;
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
    touchLastRun();
    setBusy(false);
    el.progress.classList.remove("is-running");
    void refreshNetwork();
  }
}

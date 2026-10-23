import { api, type BackupEntry } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText, formatTimestamp, formatBytes, escapeHtml } from "./format";
import { confirmDialog } from "./modal";
import { toast } from "./toast";
import { localLog } from "./logs";
import { config, setConfig, requireConfig } from "./state";

// 备份页：列表、创建、删除、清理与恢复。

let backups: BackupEntry[] = [];
let backupKeepPending: Promise<void> = Promise.resolve();

export function beginSaveBackupKeep(): void {
  backupKeepPending = saveBackupKeep();
  void backupKeepPending;
}

export const BACKUP_KEEP_DEFAULT = 10;

export function clampBackupKeep(value: number): number {
  if (!Number.isFinite(value)) {
    return BACKUP_KEEP_DEFAULT;
  }
  return Math.min(100, Math.max(1, Math.trunc(value)));
}

/** 下拉固定 5/10/20/50；配置里是其它值时补一个选项，避免静默改配置。 */
export function renderBackupKeep(value: number): void {
  const keep = clampBackupKeep(value);
  const text = String(keep);
  if (!Array.from(el.backupKeep.options).some((option) => option.value === text)) {
    const option = document.createElement("option");
    option.value = text;
    option.textContent = text;
    el.backupKeep.append(option);
  }
  el.backupKeep.value = text;
}

/* ---------- 备份 ---------- */

export function reasonLabel(reason: string): string {
  switch (reason) {
    case "manual":
      return t("settings.backupManual");
    case "pre-restore":
      return t("settings.backupPreRestore");
    default:
      return reason;
  }
}

export function renderBackups(): void {
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
        '<div class="backup-actions">' +
        '<button type="button" class="ghost small" data-restore="' +
        escapeHtml(backup.id) +
        '">' +
        escapeHtml(t("settings.backupRestore")) +
        "</button>" +
        '<button type="button" class="ghost small danger-text" data-delete="' +
        escapeHtml(backup.id) +
        '">' +
        escapeHtml(t("settings.backupDelete")) +
        "</button>" +
        "</div>" +
        "</div>",
    )
    .join("");
}

export async function loadBackups(): Promise<void> {
  try {
    backups = await api.listBackups();
    renderBackups();
  } catch (error) {
    el.backupList.innerHTML =
      '<p class="muted empty">' + escapeHtml(t("settings.backupFail", { error: errorText(error) })) + "</p>";
  }
}

export async function createBackup(): Promise<void> {
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

export async function deleteBackup(id: string): Promise<void> {
  const confirmed = await confirmDialog(
    t("settings.backupDeleteConfirmTitle"),
    t("settings.backupDeleteConfirmBody", { id }),
  );
  if (!confirmed) {
    return;
  }
  try {
    await api.deleteBackup(id);
    toast(t("settings.backupDeleted", { id }));
    await loadBackups();
  } catch (error) {
    toast(t("settings.backupFail", { error: errorText(error) }));
  }
}

/** 下拉改动立即落盘，保证「清理旧备份」按同一个份数执行。 */
export async function saveBackupKeep(): Promise<void> {
  const keep = clampBackupKeep(Number.parseInt(el.backupKeep.value, 10));
  try {
    const saved = await api.setConfig({ ...requireConfig(), backup_keep: keep });
    // 只回写份数：renderConfig 会重置设置页里尚未保存的其它输入。
    setConfig(saved);
    el.backupKeep.value = String(clampBackupKeep(saved.backup_keep));
    toast(t("settings.backupKeepSaved", { keep }));
  } catch (error) {
    toast(t("conn.saveFail", { error: errorText(error) }));
  }
}

export async function pruneBackups(): Promise<void> {
  if (!config) {
    return;
  }
  // 等一次可能还在飞行的「保留份数」保存，避免按旧配置清理。
  await backupKeepPending;
  const keep = clampBackupKeep(requireConfig().backup_keep);
  const removable = Math.max(0, backups.length - keep);
  if (removable === 0) {
    toast(t("settings.backupPruneNone", { keep }));
    return;
  }
  const confirmed = await confirmDialog(
    t("settings.backupPruneConfirmTitle"),
    t("settings.backupPruneConfirmBody", { count: removable, keep }),
  );
  if (!confirmed) {
    return;
  }
  el.backupPrune.disabled = true;
  try {
    const result = await api.pruneBackups();
    toast(t("settings.backupPruneOk", { count: result.removed.length, remaining: result.remaining }));
    await loadBackups();
  } catch (error) {
    toast(t("settings.backupFail", { error: errorText(error) }));
  } finally {
    el.backupPrune.disabled = false;
  }
}

export async function restoreBackup(id: string): Promise<void> {
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

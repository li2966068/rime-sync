import { api, type ConflictChoice, type ConflictEntry } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText, formatTimestamp, formatBytes, escapeHtml } from "./format";
import { toast } from "./toast";
import { confirmDialog } from "./modal";

// 冲突页：列表、行级 diff 与逐条/批量解决。

export let conflicts: ConflictEntry[] = [];
/// 远端大小是否已成功取到（false 时冲突页显示“远端未知”而不是“不存在”）。
let remoteSizesKnown = false;
export let activeConflict: string | null = null;

/* ---------- 冲突 ---------- */

export interface DiffRow {
  text: string;
  cls: "same" | "del" | "add" | "pad";
  n: number | null;
}

export function splitLines(text: string): string[] {
  const normalized = text.replace(/\r\n/g, "\n");
  const lines = normalized.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") {
    lines.pop();
  }
  return lines;
}

export function plainRows(left: string[], right: string[]): { left: DiffRow[]; right: DiffRow[] } {
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

export function diffRows(left: string[], right: string[]): { left: DiffRow[]; right: DiffRow[] } {
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

export function rowsHtml(rows: DiffRow[]): string {
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

export function metaLine(label: string, valueHtml: string): string {
  return '<div><span class="muted">' + escapeHtml(label) + "</span> " + valueHtml + "</div>";
}

export function remoteSizeText(entry: ConflictEntry): string {
  if (!remoteSizesKnown) {
    return t("conflicts.remoteUnknown");
  }
  return entry.remote_size === null ? t("conflicts.remoteMissing") : formatBytes(entry.remote_size);
}

export function keysHtml(keys: string[]): string {
  return keys.length > 0
    ? keys.map((key) => "<code>" + escapeHtml(key) + "</code>").join(" ")
    : '<span class="muted">' + escapeHtml(t("conflicts.keysNone")) + "</span>";
}

export function renderConflictList(): void {
  el.conflictsCount.textContent = t("conflicts.count", { count: conflicts.length });
  if (conflicts.length === 0) {
    el.conflictList.innerHTML = '<p class="muted empty">' + escapeHtml(t("conflicts.empty")) + "</p>";
    el.conflictEmpty.hidden = true;
    el.conflictBody.hidden = true;
    return;
  }
  el.conflictList.innerHTML = conflicts
    .map((entry) => {
      const remote =
        remoteSizesKnown && entry.remote_size !== null
          ? t("conflicts.remoteShort", { size: formatBytes(entry.remote_size) })
          : remoteSizeText(entry);
      return (
        '<div role="listitem" tabindex="0" class="conflict-item' +
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
        " · " +
        escapeHtml(t("conflicts.localShort", { size: formatBytes(entry.target_size) })) +
        " / " +
        escapeHtml(t("conflicts.remoteShort", { size: remote })) +
        "</span>" +
        (entry.keys.length > 0 ? '<span class="conflict-keys">' + keysHtml(entry.keys) + "</span>" : "") +
        '<span class="conflict-actions">' +
        '<button type="button" class="ghost small" data-path="' +
        escapeHtml(entry.path) +
        '" data-choice="remote">' +
        escapeHtml(t("conflicts.takeRemoteShort")) +
        "</button>" +
        '<button type="button" class="ghost small" data-path="' +
        escapeHtml(entry.path) +
        '" data-choice="local">' +
        escapeHtml(t("conflicts.keepLocal")) +
        "</button>" +
        "</span>" +
        "</div>"
      );
    })
    .join("");
  el.conflictEmpty.hidden = activeConflict !== null;
  if (!activeConflict || !conflicts.some((entry) => entry.path === activeConflict)) {
    el.conflictBody.hidden = true;
  }
}

export function renderDiff(entry: ConflictEntry): void {
  activeConflict = entry.path;
  renderConflictList();
  el.conflictBody.hidden = false;
  el.conflictEmpty.hidden = true;
  const remote = escapeHtml(remoteSizeText(entry));
  el.conflictMeta.innerHTML =
    metaLine(t("conflicts.target"), "<code>" + escapeHtml(entry.target) + "</code>") +
    metaLine(t("conflicts.copyFile"), "<code>" + escapeHtml(entry.path) + "</code>") +
    metaLine(t("conflicts.modified"), escapeHtml(formatTimestamp(entry.modified_at))) +
    metaLine(t("conflicts.localSize"), escapeHtml(formatBytes(entry.target_size))) +
    metaLine(t("conflicts.remoteSize"), remote) +
    metaLine(t("conflicts.copySize"), escapeHtml(formatBytes(entry.conflict_size))) +
    metaLine(t("conflicts.keys"), keysHtml(entry.keys)) +
    (entry.note ? metaLine(t("conflicts.note"), escapeHtml(entry.note)) : "");

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

export async function loadConflicts(): Promise<void> {
  try {
    remoteSizesKnown = false;
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
    void loadRemoteSizes();
  } catch (error) {
    el.conflictList.innerHTML =
      '<p class="muted empty">' + escapeHtml(t("conflicts.loadFail", { error: errorText(error) })) + "</p>";
    el.conflictEmpty.hidden = true;
    el.conflictBody.hidden = true;
  }
}

export async function loadRemoteSizes(): Promise<void> {
  const snapshot = conflicts;
  try {
    const info = await api.conflictRemoteSizes();
    if (snapshot !== conflicts || !info.reachable) {
      return;
    }
    remoteSizesKnown = true;
    for (const entry of conflicts) {
      entry.remote_size = info.sizes[entry.target] ?? null;
    }
    renderConflictList();
    if (activeConflict) {
      const entry = conflicts.find((item) => item.path === activeConflict);
      if (entry) {
        renderDiff(entry);
      }
    }
  } catch {
    // 远端不可达：保持“远端未知”，不影响本地冲突处理。
  }
}

export async function resolveOne(path: string, choice: ConflictChoice): Promise<void> {
  const entry = conflicts.find((item) => item.path === path);
  try {
    await api.resolveConflict(path, choice);
    toast(
      choice === "remote"
        ? t("conflicts.resolveRemoteOk", { target: entry?.target ?? path })
        : t("conflicts.resolveOk", { target: entry?.target ?? path }),
    );
  } catch (error) {
    toast(t("conflicts.resolveFail", { error: errorText(error) }));
  }
}

export function confirmResolve(entry: ConflictEntry, choice: ConflictChoice): Promise<boolean> {
  const body =
    choice === "remote"
      ? t("conflicts.takeRemoteConfirm", { target: entry.target })
      : t("conflicts.takeConfirm", { target: entry.target });
  return confirmDialog(t("modal.confirmTitle"), body);
}

export async function resolveFromList(path: string, choice: ConflictChoice): Promise<void> {
  const entry = conflicts.find((item) => item.path === path);
  if (!entry) {
    return;
  }
  if (choice !== "local" && !(await confirmResolve(entry, choice))) {
    return;
  }
  await resolveOne(path, choice);
  await loadConflicts();
}

export async function resolveAll(choice: ConflictChoice): Promise<void> {
  const targets = conflicts.map((entry) => entry.path);
  if (targets.length === 0) {
    return;
  }
  const count = targets.length;
  const body =
    choice === "remote"
      ? t("conflicts.takeRemoteAllConfirm", { count })
      : choice === "local"
        ? t("conflicts.keepAllConfirm", { count })
        : t("conflicts.takeAllConfirm", { count });
  const confirmed = await confirmDialog(t("modal.confirmTitle"), body);
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

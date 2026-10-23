import { api, type DedupeItem, type DedupeKind, type DedupeGroup, type DedupePreview } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText, formatTimestamp, formatBytes, escapeHtml } from "./format";
import { toast } from "./toast";
import { confirmDialog } from "./modal";
import { busy } from "./state";

// 去重页：扫描预览、勾选与清理。

let dedupe: DedupePreview | null = null;

export const DEDUPE_KINDS: DedupeKind[] = ["devices", "snapshots", "backups"];

export function dedupeSectionLabel(kind: DedupeKind): string {
  switch (kind) {
    case "devices":
      return t("settings.dedupeDeviceSection");
    case "snapshots":
      return t("settings.dedupeSnapshotSection");
    default:
      return t("settings.dedupeBackupSection");
  }
}

export function dedupeItemDetail(kind: DedupeKind, item: DedupeItem): string {
  const parts: string[] = [];
  if (kind === "devices") {
    if (item.hostname) {
      parts.push(item.hostname);
    }
    if (item.os) {
      parts.push(item.os);
    }
    if (item.heartbeat_ts) {
      parts.push(formatTimestamp(item.heartbeat_ts));
    }
    if (dedupe && item.id === dedupe.self_id) {
      parts.push(t("settings.dedupeSelf"));
    }
  } else if (item.label) {
    parts.push(item.label);
  }
  parts.push(formatBytes(item.size));
  return parts.join(" · ");
}

export function dedupeGroupNote(group: DedupeGroup): string {
  if (group.tag === "cross_dir") {
    return '<p class="muted small">' + escapeHtml(t("settings.dedupeCrossDir")) + "</p>";
  }
  if (group.tag === "unknown") {
    return '<p class="muted small">' + escapeHtml(t("settings.dedupeUnknown")) + "</p>";
  }
  return "";
}

export function dedupeGroupHtml(kind: DedupeKind, group: DedupeGroup): string {
  const items = group.items
    .map((item) => {
      const keep = item.is_keep
        ? ' <span class="badge">' + escapeHtml(t("settings.dedupeKeep")) + "</span>"
        : "";
      const checked = item.suggested ? " checked" : "";
      return (
        '<label class="check dedupe-item">' +
        '<input type="checkbox" data-dedupe-kind="' +
        kind +
        '" data-dedupe-size="' +
        item.size +
        '" value="' +
        escapeHtml(item.id) +
        '"' +
        checked +
        " />" +
        "<span><code>" +
        escapeHtml(item.id) +
        "</code>" +
        keep +
        "<em>" +
        escapeHtml(dedupeItemDetail(kind, item)) +
        "</em></span></label>"
      );
    })
    .join("");
  return '<div class="dedupe-group">' + dedupeGroupNote(group) + '<div class="checks">' + items + "</div></div>";
}

export function renderDedupe(): void {
  if (!dedupe) {
    el.dedupeSections.innerHTML = "";
    updateDedupeApply();
    return;
  }
  const preview = dedupe;
  el.dedupeSections.innerHTML = DEDUPE_KINDS.map((kind) => {
    const section = preview[kind];
    const itemCount = section.groups.reduce((sum, group) => sum + group.items.length, 0);
    const groups = section.groups.map((group) => dedupeGroupHtml(kind, group)).join("");
    const warnings =
      section.warnings.length > 0
        ? '<p class="muted small">' +
          escapeHtml(
            t("settings.dedupeWarnings", {
              count: section.warnings.length,
              detail: section.warnings.join("; "),
            }),
          ) +
          "</p>"
        : "";
    const body =
      section.groups.length === 0
        ? section.warnings.length === 0
          ? '<p class="muted empty">' + escapeHtml(t("settings.dedupeEmpty")) + "</p>"
          : ""
        : '<details class="dedupe-details"><summary>' +
          escapeHtml(t("settings.dedupeExpand", { count: itemCount })) +
          "</summary>" +
          groups +
          "</details>";
    return (
      '<div class="dedupe-section" data-section="' +
      kind +
      '">' +
      '<div class="dedupe-head"><strong>' +
      escapeHtml(dedupeSectionLabel(kind)) +
      "</strong>" +
      '<span class="muted small">' +
      escapeHtml(t("settings.dedupeGroups", { count: section.groups.length })) +
      " · " +
      escapeHtml(t("settings.dedupeReclaimable", { size: formatBytes(section.reclaimable) })) +
      "</span></div>" +
      body +
      warnings +
      "</div>"
    );
  }).join("");
  updateDedupeApply();
}

export function selectedDedupe(): { kind: DedupeKind; ids: string[] }[] {
  const nodes = Array.from(
    el.dedupeSections.querySelectorAll<HTMLInputElement>("input[data-dedupe-kind]:checked"),
  );
  const grouped = new Map<DedupeKind, string[]>();
  for (const node of nodes) {
    const kind = node.dataset.dedupeKind as DedupeKind | undefined;
    if (!kind || !DEDUPE_KINDS.includes(kind)) {
      continue;
    }
    const list = grouped.get(kind) ?? [];
    list.push(node.value);
    grouped.set(kind, list);
  }
  return Array.from(grouped, ([kind, ids]) => ({ kind, ids }));
}

export function selectedDedupeBytes(): number {
  const nodes = Array.from(
    el.dedupeSections.querySelectorAll<HTMLInputElement>("input[data-dedupe-kind]:checked"),
  );
  return nodes.reduce((sum, node) => sum + Number(node.dataset.dedupeSize ?? "0"), 0);
}

export function updateDedupeApply(): void {
  el.dedupeApply.disabled = selectedDedupe().length === 0;
}

export async function loadDedupe(): Promise<void> {
  if (busy) {
    return;
  }
  el.dedupeScan.disabled = true;
  el.dedupeApply.disabled = true;
  el.dedupeStatus.textContent = t("settings.dedupeScanning");
  try {
    dedupe = await api.dedupePreview();
    el.dedupeStatus.textContent = t("settings.dedupeScanned", {
      time: formatTimestamp(dedupe.generated_at),
    });
    renderDedupe();
  } catch (error) {
    el.dedupeStatus.textContent = t("settings.dedupeScanFail", { error: errorText(error) });
  } finally {
    el.dedupeScan.disabled = false;
    updateDedupeApply();
  }
}

export async function applyDedupe(): Promise<void> {
  if (busy) {
    return;
  }
  const selection = selectedDedupe();
  const count = selection.reduce((sum, group) => sum + group.ids.length, 0);
  if (count === 0) {
    toast(t("settings.dedupeNone"));
    return;
  }
  const confirmed = await confirmDialog(
    t("settings.dedupeConfirmTitle"),
    t("settings.dedupeConfirmBody", { count, size: formatBytes(selectedDedupeBytes()) }),
  );
  if (!confirmed) {
    return;
  }
  el.dedupeApply.disabled = true;
  el.dedupeScan.disabled = true;
  let removed = 0;
  let freed = 0;
  const failures: string[] = [];
  for (const group of selection) {
    try {
      const result = await api.dedupeApply(group.kind, group.ids);
      removed += result.removed.length;
      freed += result.freed_bytes;
      for (const failure of result.failed) {
        failures.push(failure.error);
      }
    } catch (error) {
      failures.push(errorText(error));
    }
  }
  await loadDedupe();
  if (failures.length > 0) {
    const text = t("settings.dedupePartial", { count: failures.length, error: failures[0] });
    el.dedupeStatus.textContent = text;
    toast(text);
  } else if (removed > 0) {
    toast(t("settings.dedupeDone", { count: removed, size: formatBytes(freed) }));
  }
}

import { api, type PresetInfo } from "../api";
import { t, presetLabel, presetHint } from "../i18n";
import { el } from "./dom";
import { errorText, escapeHtml } from "./format";
import { toast } from "./toast";
import { config } from "./state";

// 服务商预设：下拉填充与 URL 归一化。

let presets: PresetInfo[] = [];

export const PRESET_KEY = "rime-sync.preset";

/* ---------- 服务商预设 ---------- */

export function guessPreset(url: string): string {
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

export function fillPresetSelect(select: HTMLSelectElement, selected: string): void {
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

export async function loadPresets(): Promise<void> {
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

export async function applyPresetTo(
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

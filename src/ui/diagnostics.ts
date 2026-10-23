import { api } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText } from "./format";
import { toast } from "./toast";

// 诊断包导出与路径复制。

export async function copyDiagPath(): Promise<void> {
  const ok = await copyText(el.diagPath.textContent ?? "");
  toast(ok ? t("common.copied") : t("settings.diagFail", { error: "clipboard" }));
}

export async function copyText(value: string): Promise<boolean> {
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

/* ---------- 诊断包 / 更新 ---------- */

export async function exportDiagnostics(): Promise<void> {
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

import { api, type InstallWarning } from "../api";
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

/* ---------- 离线依赖安装告警（Linux postinst 写 install-warning.json） ---------- */

let installWarning: InstallWarning | null = null;

export async function refreshInstallWarning(): Promise<void> {
  try {
    installWarning = await api.installWarning();
  } catch {
    // 后端读不到就当作没有告警：诊断页本身必须始终可用。
    installWarning = null;
  }
  renderInstallWarning();
}

export function renderInstallWarning(): void {
  const present = installWarning?.present === true;
  el.installWarning.hidden = !present;
  el.installWarningRetry.disabled = !present;
  if (!present) {
    el.installWarningDetail.textContent = "";
    el.installWarningError.hidden = true;
    el.installWarningError.textContent = "";
    return;
  }
  const parts: string[] = [];
  if (installWarning?.package) {
    parts.push(t("settings.installWarningPackage", { package: installWarning.package }));
  }
  if (installWarning?.reason) {
    parts.push(installWarning.reason);
  }
  if (installWarning?.written_at) {
    parts.push(t("settings.installWarningTime", { time: installWarning.written_at }));
  }
  if (installWarning?.log_path) {
    parts.push(t("settings.installWarningLogPath", { path: installWarning.log_path }));
  }
  el.installWarningDetail.textContent = parts.join(" · ");
}

/** 「查看日志」：/var/log 下的文件普通用户不一定能直接打开，先复制路径。 */
export async function copyInstallWarningLog(): Promise<void> {
  const path = installWarning?.log_path ?? "";
  if (!path) {
    return;
  }
  const ok = await copyText(path);
  toast(
    ok
      ? t("settings.installWarningLogCopied", { path })
      : t("settings.installWarningLogCopyFail", { path }),
  );
}

/** 「重试」：pkexec 重跑 postinst 生成的 wrapper（幂等，成功后 wrapper 自己删告警）。 */
export async function retryInstallDeps(): Promise<void> {
  el.installWarningRetry.disabled = true;
  el.installWarningError.hidden = true;
  el.installWarningError.textContent = "";
  try {
    toast(await api.retryInstallDeps());
  } catch (error) {
    el.installWarningError.hidden = false;
    el.installWarningError.textContent = errorText(error);
  } finally {
    el.installWarningRetry.disabled = false;
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

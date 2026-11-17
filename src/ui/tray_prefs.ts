import { api } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText } from "./format";
import { toast } from "./toast";
import { localLog } from "./logs";

// 设置页「关闭窗口时隐藏到托盘」。
//
// 后端把偏好存在 platform.json（与 config.json 同目录），关窗行为由后端的
// on_window_event 读同一份偏好决定；这里只负责开关、状态展示与失败提示。
//
// 托盘没建起来时后端会放行关闭（否则会变成既没窗口也没托盘的僵尸进程），
// 所以这里的开关只影响「托盘可用时」的行为。

let enabled = true;
let pending = false;
let failure: string | null = null;

export function renderCloseToTray(): void {
  el.closeToTray.disabled = pending;
  el.closeToTray.checked = enabled;
  const error = failure ?? "";
  el.closeToTrayError.hidden = error.length === 0;
  el.closeToTrayError.textContent = error;
}

export async function loadCloseToTray(): Promise<void> {
  try {
    enabled = await api.getCloseToTray();
    failure = null;
  } catch (error) {
    // 读不到就按后端默认值（true）展示，并把这个错误说清楚。
    enabled = true;
    failure = t("settings.closeToTrayReadFail", { error: errorText(error) });
    localLog(failure);
  }
  renderCloseToTray();
}

export async function toggleCloseToTray(next: boolean): Promise<void> {
  if (pending) {
    return;
  }
  pending = true;
  failure = null;
  renderCloseToTray();
  try {
    enabled = await api.setCloseToTray(next);
    const message = enabled
      ? t("settings.closeToTrayOnDone")
      : t("settings.closeToTrayOffDone");
    toast(message);
    localLog(message);
  } catch (error) {
    failure = t("settings.closeToTrayFail", { error: errorText(error) });
    toast(failure);
    localLog(failure);
    // 开关失败时以后端真实状态为准（可能本来就写不进去）。
    try {
      enabled = await api.getCloseToTray();
    } catch {
      // 读不回来就保持当前显示值，不猜。
    }
  } finally {
    pending = false;
    renderCloseToTray();
  }
}

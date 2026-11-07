import { listen } from "@tauri-apps/api/event";
import { api, type ImeBootstrapStatus } from "../api";
import { t, type MessageKey } from "../i18n";
import { el } from "./dom";
import { errorText } from "./format";
import { localLog } from "./logs";
import { toast } from "./toast";

// 输入法首启自举的红条（B 方案）。
//
// 安装器只交付程序本体：输入法在程序首次启动时由 ensure(Mode::Install) 自举。
// 装小狼毫需要管理员权限，普通用户启动时最常见的结局就是 needs_elevation —— 必须
// 明确告诉用户「点修复 / 以管理员重启」，而不是把失败留在日志里。

/// 失败原因码 → 本地化文案。
///
/// 后端的 detail 是中文技术描述（"未找到预编译产物"…），英文界面下直接当正文
/// 会中英混排。现在结论按 reason 码本地化，detail 降级成末尾的"技术细节"。
const REASON_KEYS: Record<string, MessageKey> = {
  ok: "imeBanner.reasonOk",
  noop: "imeBanner.reasonNoop",
  partial: "imeBanner.reasonPartial",
  "install-disabled": "imeBanner.reasonInstallDisabled",
  "install-failed": "imeBanner.reasonInstallFailed",
  timeout: "imeBanner.reasonTimeout",
  crashed: "imeBanner.reasonCrashed",
  failed: "imeBanner.reasonFailed",
  running: "imeBanner.reasonRunning",
};

/// 后端新增了原因码而前端还没跟上时，用 result 兜底 —— 结论行不能空着。
const RESULT_KEYS: Record<string, MessageKey> = {
  ok: "imeBanner.reasonOk",
  noop: "imeBanner.reasonNoop",
  partial: "imeBanner.reasonPartial",
  failed: "imeBanner.reasonFailed",
};

function reasonText(reason: string, result?: string): string {
  const key = REASON_KEYS[reason] ?? (result ? RESULT_KEYS[result] : undefined);
  return key ? t(key) : "";
}

export function renderImeBanner(status: ImeBootstrapStatus | null): void {
  // 自举还在跑 = 不是失败（红条别弹）；真正的结论出来后再显示。
  const failed = status !== null && !status.ok && !status.running;
  el.imeWarning.hidden = !failed;
  if (!failed || status === null) {
    el.imeWarningDetail.textContent = "";
    return;
  }
  const parts: string[] = [];
  // 结论优先（本地化），后端的中文 detail 降级成末尾的技术细节。
  const headline = reasonText(status.reason, status.result);
  if (headline) {
    parts.push(headline);
  }
  if (status.ime_install) {
    parts.push(t("imeBanner.state", { install: status.ime_install }));
  }
  if (status.needs_elevation) {
    parts.push(t("imeBanner.needsElevation"));
  }
  if (status.updated_at) {
    parts.push(t("imeBanner.at", { time: status.updated_at }));
  }
  if (status.detail) {
    // 过长时截断，避免把红条撑成一屏。
    const detail = status.detail.length > 400 ? status.detail.slice(0, 400) + "…" : status.detail;
    parts.push(t("imeBanner.detail", { text: detail }));
  }
  el.imeWarningDetail.textContent = parts.join(" · ");
}

export async function refreshImeStatus(): Promise<void> {
  try {
    renderImeBanner(await api.imeStatus());
  } catch {
    // 读不到状态就当作没有告警：设置页本身必须始终可用。
    renderImeBanner(null);
  }
}

/// 后台自举完成时（可能晚于界面加载）刷新红条并提示一次。
export async function subscribeImeBootstrap(): Promise<void> {
  await listen<ImeBootstrapStatus>("ime://bootstrap", (event) => {
    renderImeBanner(event.payload);
    if (!event.payload.ok && !event.payload.running) {
      // 提示语本地化（后端 detail 是中文技术描述，留在日志里）。
      // 回退用 result（机器码，语言中立），中文的 ime_error 只进日志 ——
      // 正文里混一段中文在英文界面下很难看。
      const text =
        reasonText(event.payload.reason, event.payload.result) ||
        t("imeBanner.failed", { error: event.payload.result });
      toast(text);
      localLog(
        `${text} · ${event.payload.detail} · ${event.payload.ime_error ?? ""}`,
      );
    }
  });
}

/// 「以管理员重启」：Windows 上走 UAC 重新拉起自己；成功后当前实例会退出。
export async function elevateForIme(): Promise<void> {
  el.imeElevate.disabled = true;
  try {
    await api.restartElevated();
  } catch (error) {
    toast(t("imeBanner.elevateFailed", { error: errorText(error) }));
  } finally {
    el.imeElevate.disabled = false;
  }
}

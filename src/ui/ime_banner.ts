import { listen } from "@tauri-apps/api/event";
import { api, type ExtraSupportView, type ImeBootstrapStatus } from "../api";
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
  // 后端只在「结论是 partial 且唯一原因是小键盘」时给这个码。
  "numpad-pending": "imeBanner.reasonNumpadPending",
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
  // 增强功能（数字小键盘 / 中文标点直出 / v 转数字）是另一件事：它不靠自举，
  // 靠本机装的引擎是不是我们的自建版，所以单独查一次。
  void refreshExtraSupport();
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

// ── 输入法增强功能（数字小键盘 / 中文标点直出 / v 转数字）────────────────────
//
// 增强组件是 rime-sync 自研、编进输入法引擎本体的，发行版 Rime 里没有：
// 方案一旦引用它们就直接报错，整个输入法用不了。
// 判据全在后端（core 的 ime_state::assess_extra_support），这里只负责两件事：
// 1. 把结论翻译成用户看得懂的大白话；
// 2. Linux 上给出「一键换装」按钮（Windows 不给按钮：点一次「修复输入法」
//    程序会自动换 Weasel）。
//
// 面板在 index.html 里静态声明（#extra-ime-panel）：固定文案走 data-i18n，
// 结论/建议这类由后端判据生成的文字走 t(key) —— 切语言时 app.ts 会再刷一次。

let extraState: ExtraSupportView | null = null;

/// 按后端结论渲染增强功能区块（文案全走 i18n，后端只给判据与人话建议）。
function renderExtraSupport(): void {
  const state = extraState;
  if (!state) {
    el.extraPanel.hidden = true;
    return;
  }
  el.extraPanel.hidden = false;
  const supported = state.support === "supported";
  el.extraSummary.textContent = supported
    ? t("extraIme.supported")
    : state.support === "unsupported"
      ? t("extraIme.unsupported")
      : t("extraIme.unknown");
  el.extraAdvice.textContent = t("extraIme.advice", { text: state.advice });
  const linux = state.platform === "linux";
  // 换装入口只给 Linux（且换装脚本在位）；Windows 由「修复输入法」自动换。
  el.extraAction.hidden = supported || !linux || !state.can_reinstall;
  if (supported) {
    el.extraHint.hidden = true;
    el.extraHint.textContent = "";
    el.extraStatus.textContent = "";
    return;
  }
  el.extraHint.hidden = false;
  if (!linux) {
    el.extraHint.textContent = t("extraIme.windowsHint");
  } else if (!state.can_reinstall) {
    // AppImage 之类没有换装脚本的版本：只能让用户手动用安装包装。
    el.extraHint.textContent = t("extraIme.noScript");
  } else {
    el.extraHint.textContent = t("extraIme.linuxHint");
  }
}

/// 读取本机输入法的增强功能状态并刷新界面。
export async function refreshExtraSupport(): Promise<void> {
  try {
    extraState = await api.extraSupport();
  } catch (error) {
    // 读不到状态也要把原因说清楚，不能悄悄装没这回事。
    extraState = null;
    el.extraPanel.hidden = false;
    el.extraSummary.textContent = t("extraIme.detectFailed", { error: errorText(error) });
    el.extraAdvice.textContent = "";
    el.extraHint.textContent = "";
    el.extraHint.hidden = true;
    el.extraAction.hidden = true;
    el.extraStatus.textContent = "";
    return;
  }
  renderExtraSupport();
}

/// 让用户挑一个本地安装包（后端直接调系统文件选择器）。
export async function pickExtraPackage(): Promise<void> {
  try {
    const picked = await api.pickExtraPackage();
    if (picked) {
      el.extraPackage.value = picked;
      el.extraStatus.textContent = "";
    }
  } catch (error) {
    el.extraStatus.textContent = t("extraIme.failed", { error: errorText(error) });
  }
}

/// 「一键换装」：把用户选的安装包交给后端，后端用 pkexec 调换装脚本。
/// pkexec 会弹管理员授权窗口，换装要几分钟，所以界面不等它跑完。
export async function runExtraInstall(): Promise<void> {
  const path = el.extraPackage.value.trim();
  if (!path) {
    el.extraStatus.textContent = t("extraIme.needPackage");
    return;
  }
  el.extraInstall.disabled = true;
  el.extraBrowse.disabled = true;
  el.extraInstall.textContent = t("extraIme.working");
  try {
    const message = await api.installExtraIme(path);
    el.extraStatus.textContent = message;
    toast(message);
    localLog(message);
  } catch (error) {
    // 后端已经把错误翻成中文了，这里只包一层，不把原始 Err 抛给用户。
    const text = t("extraIme.failed", { error: errorText(error) });
    el.extraStatus.textContent = text;
    toast(text);
    localLog(text);
  } finally {
    el.extraInstall.disabled = false;
    el.extraBrowse.disabled = false;
    el.extraInstall.textContent = t("extraIme.install");
  }
}

import { api, type AutostartMechanism, type AutostartState } from "../api";
import { t, type MessageKey } from "../i18n";
import { el } from "./dom";
import { errorText } from "./format";
import { toast } from "./toast";
import { localLog } from "./logs";

// 设置页「开机自启动」：后端写 XDG .desktop（Linux）/ HKCU Run（Windows），
// 这里只负责开关、展示机制与目标路径，以及把可读错误显示出来。

const MECHANISM_KEYS: Record<AutostartMechanism, MessageKey> = {
  "xdg-desktop": "autostart.mechanism.xdg",
  registry: "autostart.mechanism.registry",
  "launch-agent": "autostart.mechanism.launchAgent",
};

let snapshot: AutostartState | null = null;
// 后端返回的 error 之外，还要记住「本次开关失败」的原因：重新读状态不该把它冲掉。
let failure: string | null = null;
let pending = false;

function mechanismText(mechanism: AutostartMechanism): string {
  const key = MECHANISM_KEYS[mechanism] as MessageKey | undefined;
  return t(key ?? "autostart.mechanism.unknown");
}

export function renderAutostart(): void {
  const current = snapshot;
  const supported = current?.supported ?? false;
  el.autostartToggle.disabled = pending || !supported;
  el.autostartToggle.checked = current?.enabled ?? false;
  el.autostartState.textContent = current
    ? supported
      ? current.enabled
        ? t("settings.autostartOn")
        : t("settings.autostartOff")
      : t("settings.autostartUnsupported")
    : t("settings.autostartLoading");
  el.autostartState.className = "pill" + (current?.enabled ? " ok" : "");

  el.autostartMechanism.textContent = supported
    ? t("settings.autostartMechanism", { mechanism: mechanismText(current!.mechanism) })
    : "";
  el.autostartTarget.hidden = !current?.target;
  el.autostartTarget.textContent = current?.target
    ? t("settings.autostartTarget", { path: current.target })
    : "";
  const error = failure ?? current?.error ?? "";
  el.autostartError.hidden = error.length === 0;
  el.autostartError.textContent = error;
}

export async function loadAutostart(): Promise<void> {
  try {
    snapshot = await api.autostartStatus();
  } catch (error) {
    snapshot = null;
    failure = t("settings.autostartReadFail", { error: errorText(error) });
  }
  renderAutostart();
}

export async function toggleAutostart(enabled: boolean): Promise<void> {
  if (pending) {
    return;
  }
  pending = true;
  failure = null;
  renderAutostart();
  try {
    snapshot = await api.setAutostart(enabled);
    const message = enabled
      ? t("settings.autostartOnDone")
      : t("settings.autostartOffDone");
    toast(message);
    localLog(message);
  } catch (error) {
    // 开关失败时以后端真实状态为准（可能本来就写不进去），并把错误留在卡片里。
    failure = t("settings.autostartFail", { error: errorText(error) });
    snapshot = null;
    toast(failure);
    localLog(failure);
    try {
      snapshot = await api.autostartStatus();
    } catch {
      snapshot = null;
    }
  } finally {
    pending = false;
    renderAutostart();
  }
}

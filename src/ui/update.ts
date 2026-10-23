import { api } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText } from "./format";
import { confirmDialog } from "./modal";

// 版本显示、检查更新与安装。

let pendingUpdate: string | null = null;
let appVersion = "0.2.0";

export function applyAppVersion(version: string): void {
  appVersion = version;
  renderUpdate();
}

export function renderUpdate(): void {
  el.updateVersion.textContent = t("settings.version", { version: appVersion });
}

export async function checkUpdate(): Promise<void> {
  el.updateCheck.disabled = true;
  el.updateStatus.textContent = t("settings.updateChecking");
  el.updatePanel.hidden = true;
  try {
    const info = await api.checkUpdate();
    if (info.available && info.version) {
      pendingUpdate = info.version;
      el.updateStatus.textContent = t("settings.updateAvailable", {
        version: info.version,
        current: info.current_version,
      });
      el.updateNotes.textContent = info.notes ?? "";
      el.updatePanel.hidden = false;
    } else {
      pendingUpdate = null;
      el.updateStatus.textContent = t("settings.updateNone", { version: info.current_version });
    }
  } catch (error) {
    pendingUpdate = null;
    el.updateStatus.textContent = t("settings.updateFail", { error: errorText(error) });
  } finally {
    el.updateCheck.disabled = false;
  }
}

export async function installUpdate(): Promise<void> {
  if (!pendingUpdate) {
    return;
  }
  const confirmed = await confirmDialog(
    t("settings.updateConfirmTitle"),
    t("settings.updateConfirmBody", { version: pendingUpdate }),
  );
  if (!confirmed) {
    return;
  }
  el.updateInstall.disabled = true;
  el.updateStatus.textContent = t("settings.updateInstalling");
  try {
    const result = await api.installUpdate();
    el.updateStatus.textContent = t("settings.updateDone", { message: result.message });
    el.updatePanel.hidden = true;
  } catch (error) {
    el.updateStatus.textContent = t("settings.updateFail", { error: errorText(error) });
  } finally {
    el.updateInstall.disabled = false;
  }
}

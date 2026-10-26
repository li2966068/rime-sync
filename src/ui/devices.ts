import { api, type DeviceInfo } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText, formatTimestamp, escapeHtml } from "./format";
import { toast } from "./toast";
import { confirmDialog } from "./modal";

// 设备页：设备表渲染与撤销。

let devices: DeviceInfo[] = [];

/* ---------- 设备 ---------- */

export function renderDevices(): void {
  el.deviceEmpty.hidden = devices.length > 0;
  el.deviceEmpty.textContent = t("devices.empty");
  el.deviceBody.innerHTML = devices
    .map((device) => {
      const state = device.reachable
        ? '<span class="dot ok"></span>' + escapeHtml(t("devices.reachable"))
        : '<span class="dot"></span>' + escapeHtml(t("devices.offline"));
      // 本机但远端还没有 sync/<id>/owner.json：用独立文案区分「未上传」占位行。
      // 目录名 != 本机 ID 的旧 device_id 目录用「历史目录」徽标区分。
      let badge = "";
      if (device.is_self) {
        badge =
          ' <span class="badge">' +
          escapeHtml(device.reachable ? t("devices.self") : t("devices.selfNotUploaded")) +
          "</span>";
      } else if (device.legacy) {
        badge =
          ' <span class="badge warn" title="' +
          escapeHtml(t("devices.legacyHint")) +
          '">' +
          escapeHtml(t("devices.legacy")) +
          "</span>";
      }
      // 本机行给「重置远端身份」，其它行才是普通注销。
      const action = device.is_self
        ? '<button type="button" class="ghost small danger-text" data-reset="' +
          escapeHtml(device.id) +
          '">' +
          escapeHtml(t("devices.reset")) +
          "</button>"
        : '<button type="button" class="ghost small danger-text" data-revoke="' +
          escapeHtml(device.id) +
          '">' +
          escapeHtml(t("devices.revoke")) +
          "</button>";
      return (
        "<tr" +
        (device.is_self ? ' class="is-self"' : "") +
        "><td><code>" +
        escapeHtml(device.id) +
        "</code>" +
        badge +
        "</td><td>" +
        escapeHtml(device.hostname) +
        "</td><td>" +
        escapeHtml(device.os) +
        "</td><td>" +
        escapeHtml(formatTimestamp(device.heartbeat_ts)) +
        "<br />" +
        state +
        "</td><td>" +
        action +
        "</td></tr>"
      );
    })
    .join("");
}

export async function loadDevices(): Promise<void> {
  try {
    devices = await api.listDevices();
    renderDevices();
  } catch (error) {
    el.deviceBody.innerHTML = "";
    el.deviceEmpty.hidden = false;
    el.deviceEmpty.textContent = t("devices.loadFail", { error: errorText(error) });
  }
}

export async function revokeDevice(id: string): Promise<void> {
  const confirmed = await confirmDialog(
    t("devices.revokeConfirmTitle"),
    t("devices.revokeConfirmBody", { id }),
  );
  if (!confirmed) {
    return;
  }
  try {
    await api.revokeDevice(id);
    toast(t("devices.revokeOk", { id }));
    await loadDevices();
  } catch (error) {
    toast(t("devices.revokeFail", { error: errorText(error) }));
  }
}

/// 重置远端身份：删除 sync/<installation_id>（含 owner.json 与该目录下的快照）。
/// 本机配置与本地词库不受影响；下次同步会按同一 ID 重建目录。
export async function resetLocalRemoteDir(id: string): Promise<void> {
  const confirmed = await confirmDialog(
    t("devices.resetConfirmTitle"),
    t("devices.resetConfirmBody", { id }),
  );
  if (!confirmed) {
    return;
  }
  try {
    const result = await api.resetLocalRemoteDir(id);
    toast(t("devices.resetOk", { removed: result.removed }));
    await loadDevices();
  } catch (error) {
    toast(t("devices.resetFail", { error: errorText(error) }));
  }
}

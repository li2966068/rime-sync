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
      const self = device.is_self
        ? ' <span class="badge">' +
          escapeHtml(device.reachable ? t("devices.self") : t("devices.selfNotUploaded")) +
          "</span>"
        : "";
      return (
        "<tr" +
        (device.is_self ? ' class="is-self"' : "") +
        "><td><code>" +
        escapeHtml(device.id) +
        "</code>" +
        self +
        "</td><td>" +
        escapeHtml(device.hostname) +
        "</td><td>" +
        escapeHtml(device.os) +
        "</td><td>" +
        escapeHtml(formatTimestamp(device.heartbeat_ts)) +
        "<br />" +
        state +
        '</td><td><button type="button" class="ghost small danger-text" data-revoke="' +
        escapeHtml(device.id) +
        '"' +
        (device.is_self ? " disabled" : "") +
        ">" +
        escapeHtml(t("devices.revoke")) +
        "</button></td></tr>"
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

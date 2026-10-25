import { api, type Config, type DeviceIdCheck } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText } from "./format";
import { toast } from "./toast";
import { localLog } from "./logs";
import { config, busy, setConfig, requireConfig } from "./state";
import { clampBackupKeep, renderBackupKeep } from "./backups";
import { refreshSecretStatus } from "./secret";
import { loadDevices } from "./devices";
import { renderDeviceKeys } from "./device_keys";

// 设置页：配置回填、草稿与保存、Rime 目录、设备名称（label）。

type DeviceChoice = "migrate" | "local" | "cancel";
let deviceChoiceResolve: ((choice: DeviceChoice) => void) | null = null;

export function renderConfig(next: Config): void {
  setConfig(next);
  el.url.value = next.webdav.url;
  el.user.value = next.webdav.username;
  // 后端序列化时跳过 password，这里可能是 undefined。
  el.pass.value = next.webdav.password ?? "";
  el.dir.value = next.webdav.remote_dir;
  el.rimeDir.value = next.rime_dir ?? "";
  el.interval.value = String(next.transport_interval_minutes);
  el.deviceId.value = next.device_id;
  el.kindUserdict.checked = next.sync_kinds.userdict;
  el.kindYaml.checked = next.sync_kinds.custom_yaml;
  el.kindDict.checked = next.sync_kinds.dict_sources;
  el.excludes.value = next.exclude_globs.join("\n");
  el.stopFcitx5.checked = next.stop_fcitx5_during_apply;
  renderBackupKeep(next.backup_keep);
  el.footerConfig.textContent = next.device_id ? t("footer.device", { id: next.device_id }) : "";
  renderDeviceKeys();
  renderFirstSyncBanner();
}

/* ---------- 首同步保护（SPEC-4 §A1） ---------- */

export function renderFirstSyncBanner(): void {
  // 字段缺失时按后端默认值 preview 处理（宁可按只读展示，也不静默放开上传）。
  const policy = config?.first_sync_policy ?? "preview";
  const visible = config !== null && policy !== "normal";
  el.firstSyncBanner.hidden = !visible;
  el.firstSyncSwitch.disabled = !visible || busy;
  if (!visible) {
    el.firstSyncError.hidden = true;
    el.firstSyncError.textContent = "";
  }
}

export async function switchToTwoWay(): Promise<void> {
  if (!config || busy) {
    return;
  }
  // 只改 first_sync_policy；其余字段（含 schema_version / password_ref）原样写回。
  const draft: Config = { ...config, first_sync_policy: "normal" };
  el.firstSyncSwitch.disabled = true;
  try {
    const saved = await api.setConfig(draft);
    renderConfig(saved);
    toast(t("firstSync.switched"));
    localLog(t("firstSync.switched"));
  } catch (error) {
    const text = errorText(error);
    el.firstSyncError.textContent = t("firstSync.switchFail", { error: text });
    el.firstSyncError.hidden = false;
    toast(t("firstSync.switchFail", { error: text }));
    localLog(t("firstSync.switchFail", { error: text }));
    el.firstSyncSwitch.disabled = false;
  }
}

export function settingsDraft(): Config {
  const base = requireConfig();
  const minutes = Number.parseInt(el.interval.value, 10);
  return {
    ...base,
    transport_interval_minutes: Number.isFinite(minutes)
      ? Math.min(1440, Math.max(0, minutes))
      : base.transport_interval_minutes,
    backup_keep: clampBackupKeep(Number.parseInt(el.backupKeep.value, 10)),
    sync_kinds: {
      userdict: el.kindUserdict.checked,
      custom_yaml: el.kindYaml.checked,
      dict_sources: el.kindDict.checked,
    },
    exclude_globs: el.excludes.value
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line.length > 0),
    stop_fcitx5_during_apply: el.stopFcitx5.checked,
  };
}

export async function persist(draft: Config, message: string): Promise<void> {
  const saved = await api.setConfig(draft);
  renderConfig(saved);
  void refreshSecretStatus();
  toast(message);
}

export async function saveRimeDir(): Promise<void> {
  try {
    const value = el.rimeDir.value.trim();
    const saved = await api.setConfig({
      ...requireConfig(),
      rime_dir: value.length > 0 ? value : null,
    });
    renderConfig(saved);
    el.rimeDirSource.textContent = value ? t("sync.manual") : t("sync.notDetected");
  } catch (error) {
    toast(t("conn.saveFail", { error: errorText(error) }));
  }
}

/// 原生目录选择器：设置页保存，向导只填输入框（点「下一步」时统一保存）。
export async function pickRimeDir(
  target: HTMLInputElement,
  status: HTMLParagraphElement,
  save: boolean,
): Promise<void> {
  try {
    const picked = await api.pickFolder(t("sync.rimeDir"));
    if (!picked) {
      return;
    }
    target.value = picked;
    status.textContent = t("sync.manual");
    if (save) {
      const saved = await api.setConfig({ ...requireConfig(), rime_dir: picked });
      renderConfig(saved);
      toast(t("sync.saved"));
    }
  } catch (error) {
    toast(t("sync.detectFail") + "：" + errorText(error));
  }
}

export async function detectRimeDir(
  target = el.rimeDir,
  status = el.rimeDirSource,
  silent = false,
): Promise<void> {
  el.detect.disabled = true;
  status.textContent = t("sync.detecting");
  try {
    const detection = await api.detectRimeDir();
    target.value = detection.path;
    status.textContent = t("sync.source", { source: detection.source });
    const saved = await api.setConfig({ ...requireConfig(), rime_dir: detection.path });
    renderConfig(saved);
    if (!silent) {
      toast(t("sync.detected"));
    }
  } catch (error) {
    status.textContent = t("sync.detectFail");
    toast(t("sync.detectFail") + "：" + errorText(error));
  } finally {
    el.detect.disabled = busy;
  }
}

/* ---------- 设备名称（label）与去重 ---------- */

export const DEVICE_ID_MAX = 64;

export function deviceIdError(id: string): string | null {
  if (!id) {
    return t("settings.deviceInvalidEmpty");
  }
  if (id.length > DEVICE_ID_MAX) {
    return t("settings.deviceInvalidLong");
  }
  if (id === "." || id === "..") {
    return t("settings.deviceInvalidReserved");
  }
  if (!/^[a-z0-9-]+$/.test(id)) {
    return t("settings.deviceInvalidChars");
  }
  return null;
}

export function randomDeviceId(): string {
  const alphabet = "abcdefghijklmnopqrstuvwxyz0123456789";
  const bytes = new Uint8Array(6);
  crypto.getRandomValues(bytes);
  let suffix = "";
  for (const byte of bytes) {
    suffix += alphabet[byte % alphabet.length];
  }
  return "device-" + suffix;
}

export function deviceChoiceDialog(body: string): Promise<DeviceChoice> {
  el.deviceModalBody.textContent = body;
  el.deviceModal.hidden = false;
  el.deviceMigrate.focus();
  return new Promise((resolve) => {
    deviceChoiceResolve = resolve;
  });
}

export function settleDeviceChoice(choice: DeviceChoice): void {
  el.deviceModal.hidden = true;
  const resolve = deviceChoiceResolve;
  deviceChoiceResolve = null;
  resolve?.(choice);
}

/// 设备名称变更后：先把本机身份登记到远端（目录名 = installation_id，幂等，只写 owner.json），
/// 再刷新设备页。远端登记失败不影响本地已保存的名称：同步时会再写一次 owner.json。
async function refreshAfterIdentityChange(): Promise<void> {
  try {
    await api.publishIdentity();
    toast(t("settings.devicePublishOk"));
  } catch (error) {
    toast(t("settings.devicePublishFail", { error: errorText(error) }));
  }
  await loadDevices();
}

export async function saveSettings(): Promise<void> {
  if (!config || busy) {
    return;
  }
  const nextId = el.deviceId.value.trim();
  const invalid = deviceIdError(nextId);
  if (invalid) {
    el.settingsHint.textContent = invalid;
    toast(invalid);
    return;
  }
  const previousId = config.device_id;
  if (nextId === previousId) {
    el.settingsHint.textContent = "";
    await persist(settingsDraft(), t("settings.saved"));
    return;
  }

  el.settingsHint.textContent = t("settings.deviceChecking");
  let check: DeviceIdCheck;
  try {
    check = await api.checkDeviceId(nextId);
  } catch (error) {
    const text = t("settings.deviceCheckFail", { error: errorText(error) });
    el.settingsHint.textContent = text;
    toast(text);
    return;
  }
  if (!check.ok) {
    const text = check.error ?? t("settings.deviceInvalidChars");
    el.settingsHint.textContent = text;
    toast(text);
    return;
  }

  const body = check.remote_exists
    ? t("settings.deviceChangeBodyExists", { old: previousId, new: nextId })
    : t("settings.deviceChangeBody", { old: previousId, new: nextId });
  const choice = await deviceChoiceDialog(body);
  if (choice === "cancel") {
    el.settingsHint.textContent = "";
    return;
  }
  if (choice === "migrate") {
    // 后端把 installation.yaml、本地与远端 sync 目录一起改名，并保存 device_id 标签；
    // 失败时后端已回滚，这里只需保留旧配置。
    el.settingsHint.textContent = t("settings.deviceChecking");
    try {
      const result = await api.migrateDevice(previousId, nextId);
      // 后端按结构化结果说明到底做了什么：无需迁移 / 只改本地目录 / 复制了远端目录。
      const message = result.skipped
        ? t("settings.deviceMigrateSkipped")
        : result.remote_renamed
          ? t("settings.deviceMigrateOk", { files: result.copied_files })
          : t("settings.deviceMigrateLocalRenamed");
      toast(message);
      if (result.warnings.length > 0) {
        localLog(result.warnings.join("；"));
      }
    } catch (error) {
      const text = t("settings.deviceMigrateFail", { error: errorText(error) });
      el.settingsHint.textContent = text;
      toast(text);
      return;
    }
  }
  try {
    await persist(
      { ...settingsDraft(), device_id: nextId },
      choice === "migrate" ? t("settings.deviceMigrateDone") : t("settings.deviceLocalDone"),
    );
    el.settingsHint.textContent = "";
  } catch (error) {
    const text = t("conn.saveFail", { error: errorText(error) });
    el.settingsHint.textContent = text;
    toast(text);
    return;
  }
  // 迁移 / 仅本地修改都要登记新身份并刷新设备页，否则设备页会停留在旧 ID。
  await refreshAfterIdentityChange();
}

import { api } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText, escapeHtml } from "./format";
import { toast } from "./toast";
import { config, busy, requireConfig, setConfig } from "./state";
import { bareKey, labelForKey } from "./config_keys";

// 设置页「设备本地键」卡片：这些 patch 键只保留在本机，同步时不上传，
// 也不会被其它设备的修改覆盖。默认只含 schema_list（各设备保留自己的默认方案）。

/// 与核心 default_device_local_keys 保持一致；后端字段缺失时按此展示。
export const DEFAULT_DEVICE_LOCAL_KEYS = ["schema_list"];
/// 与核心 DEVICE_LOCAL_KEY_MAX_LEN 保持一致。
export const DEVICE_LOCAL_KEY_MAX = 128;

export function deviceLocalKeys(): string[] {
  const keys = config?.device_local_keys;
  return Array.isArray(keys) ? keys : [...DEFAULT_DEVICE_LOCAL_KEYS];
}

/// 键名形如 a/b 或单段：每段只允许字母、数字、下划线与连字符。
export function localKeyError(key: string): string | null {
  if (key.length === 0) {
    return t("deviceLocal.emptyInput");
  }
  if (key.length > DEVICE_LOCAL_KEY_MAX) {
    return t("deviceLocal.tooLong");
  }
  if (!/^[A-Za-z0-9_-]+(\/[A-Za-z0-9_-]+)*$/.test(key)) {
    return t("deviceLocal.invalid");
  }
  return null;
}

function setStatus(text: string): void {
  el.deviceLocalStatus.textContent = text;
}

export function renderDeviceKeys(): void {
  const keys = deviceLocalKeys();
  el.deviceLocalEmpty.hidden = keys.length > 0;
  el.deviceLocalAdd.disabled = busy;
  el.deviceLocalInput.disabled = busy;
  el.deviceLocalList.innerHTML = keys
    .map((key) => {
      const raw = escapeHtml(key);
      const label = labelForKey(key);
      const known = label !== key;
      return (
        '<div class="key-row">' +
        '<span class="key-name"><code>' +
        raw +
        "</code>" +
        (known ? '<span class="key-label">' + escapeHtml(label) + "</span>" : "") +
        "</span>" +
        '<button type="button" class="ghost small danger-text" data-remove-key="' +
        raw +
        '" title="' +
        escapeHtml(t("deviceLocal.remove")) +
        '" aria-label="' +
        escapeHtml(t("deviceLocal.remove")) +
        '">' +
        escapeHtml(t("deviceLocal.remove")) +
        "</button></div>"
      );
    })
    .join("");
}

async function persistKeys(next: string[], message: string): Promise<boolean> {
  try {
    const saved = await api.setConfig({ ...requireConfig(), device_local_keys: next });
    // 老后端不认识该字段时回包会缺省：按刚提交的值兜住，避免刚加的键立刻从列表消失。
    // 只回写配置对象，不整体重绘表单：设置页可能还有未保存的其它草稿。
    setConfig(
      Array.isArray(saved.device_local_keys) ? saved : { ...saved, device_local_keys: next },
    );
    setStatus("");
    renderDeviceKeys();
    toast(message);
    return true;
  } catch (error) {
    const text = t("deviceLocal.saveFail", { error: errorText(error) });
    setStatus(text);
    toast(text);
    return false;
  }
}

export async function addDeviceLocalKey(raw: string): Promise<void> {
  if (busy) {
    return;
  }
  // 允许直接粘贴 patch.menu/page_size：裸键才是配置里真正写入的形式。
  const key = bareKey(raw);
  const invalid = localKeyError(key);
  if (invalid) {
    setStatus(invalid);
    return;
  }
  const current = deviceLocalKeys();
  if (current.includes(key)) {
    setStatus(t("deviceLocal.duplicate", { key }));
    return;
  }
  const ok = await persistKeys(
    [...current, key],
    t("deviceLocal.added", { key: labelForKey(key) }),
  );
  if (ok) {
    el.deviceLocalInput.value = "";
  }
}

export async function removeDeviceLocalKey(key: string): Promise<void> {
  if (busy) {
    return;
  }
  const current = deviceLocalKeys();
  if (!current.includes(key)) {
    return;
  }
  await persistKeys(
    current.filter((item) => item !== key),
    t("deviceLocal.removed", { key: labelForKey(key) }),
  );
}

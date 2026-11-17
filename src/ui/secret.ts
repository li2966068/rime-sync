import { api, type SecretStatus } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { setPasswordStored } from "./password";

// 密码保管状态（钥匙串 / 文件）。

let secretState: SecretStatus | null = null;

export function getSecretState(): SecretStatus | null {
  return secretState;
}

export function applySecretText(status: SecretStatus): void {
  el.secretStatus.textContent =
    (status.backend === "keyring" ? t("conn.secretKeyring") : t("conn.secretFile")) +
    " · " +
    (status.has_password ? t("conn.secretSaved") : t("conn.secretMissing"));
}

export async function refreshSecretStatus(): Promise<void> {
  try {
    const status = await api.secretStatus();
    secretState = status;
    // 密码在不在后端，只用「已设置/未设置」表达：前端不持有密码本身。
    setPasswordStored(status.has_password);
    el.secretStatus.textContent =
      (status.backend === "keyring" ? t("conn.secretKeyring") : t("conn.secretFile")) +
      " · " +
      (status.has_password ? t("conn.secretSaved") : t("conn.secretMissing"));
    el.secretStatus.className = "pill" + (status.has_password ? " ok" : "");
  } catch {
    el.secretStatus.textContent = "";
  }
}

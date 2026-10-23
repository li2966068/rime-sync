import { api, type SecretStatus } from "../api";
import { t } from "../i18n";
import { el } from "./dom";

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
    el.secretStatus.textContent =
      (status.backend === "keyring" ? t("conn.secretKeyring") : t("conn.secretFile")) +
      " · " +
      (status.has_password ? t("conn.secretSaved") : t("conn.secretMissing"));
    el.secretStatus.className = "pill" + (status.has_password ? " ok" : "");
  } catch {
    el.secretStatus.textContent = "";
  }
}

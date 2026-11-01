import type { ProxyConfig } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { config } from "./state";

// 代理小节：地址校验、草稿与应用。
// 校验规则必须与后端 crates/core/src/proxy.rs 的 ProxySpec::parse 保持一致。

const PROXY_SCHEMES = ["http", "https", "socks5", "socks5h"];

/** 返回错误文案；null = 合法。空地址不在这里报错（禁用时允许为空）。 */
export function proxyUrlError(raw: string): string | null {
  const url = raw.trim();
  if (!url) {
    return t("proxy.urlEmpty");
  }
  const match = /^([a-zA-Z][a-zA-Z0-9+.-]*):\/\/(.*)$/.exec(url);
  if (!match) {
    return t("proxy.urlScheme");
  }
  if (!PROXY_SCHEMES.includes(match[1].toLowerCase())) {
    return t("proxy.urlScheme");
  }
  const rest = match[2];
  if (rest.includes("@")) {
    return t("proxy.urlCredentials");
  }
  if (/[/?#\s]/.test(rest)) {
    return t("proxy.urlPath");
  }
  let port = "";
  if (rest.startsWith("[")) {
    const end = rest.indexOf("]");
    if (end <= 1) {
      return t("proxy.urlHost");
    }
    const tail = rest.slice(end + 1);
    if (tail) {
      if (!tail.startsWith(":")) {
        return t("proxy.urlHost");
      }
      port = tail.slice(1);
    }
  } else {
    const parts = rest.split(":");
    if (parts.length > 2 || !parts[0]) {
      return t("proxy.urlHost");
    }
    if (parts.length === 2) {
      port = parts[1];
      if (!port) {
        return t("proxy.urlPort");
      }
    }
  }
  if (port) {
    if (!/^\d{1,5}$/.test(port)) {
      return t("proxy.urlPort");
    }
    const value = Number.parseInt(port, 10);
    if (value < 1 || value > 65535) {
      return t("proxy.urlPort");
    }
  }
  return null;
}

/** 界面草稿：password 留空 = 沿用后端已保存的密码。 */
export function proxyDraft(): ProxyConfig {
  return {
    enabled: el.proxyEnabled.checked,
    url: el.proxyUrl.value.trim(),
    username: el.proxyUser.value.trim(),
    password: el.proxyPass.value,
    password_ref: config?.proxy.password_ref ?? "none",
  };
}

/** 启用时才校验；返回可直接展示的错误文案。 */
export function proxyValidationError(draft: ProxyConfig): string | null {
  if (!draft.enabled) {
    return null;
  }
  const invalid = proxyUrlError(draft.url);
  if (invalid) {
    return invalid;
  }
  if (!draft.username.trim() && (draft.password ?? "").length > 0) {
    return t("proxy.userRequired");
  }
  return null;
}

export function renderProxy(next: ProxyConfig): void {
  el.proxyEnabled.checked = next.enabled;
  el.proxyUrl.value = next.url;
  el.proxyUser.value = next.username;
  // 后端序列化时跳过 password：这里永远是空串。
  el.proxyPass.value = next.password ?? "";
  syncProxyDisabled();
}

/** 未启用时代理地址/账号不可编辑，避免填了却不生效。 */
export function syncProxyDisabled(): void {
  const disabled = !el.proxyEnabled.checked;
  el.proxyUrl.disabled = disabled;
  el.proxyUser.disabled = disabled;
  el.proxyPass.disabled = disabled;
}

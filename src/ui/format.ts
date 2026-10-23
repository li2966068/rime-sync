import { getLang, t } from "../i18n";

// 文本格式化：错误串、时间戳、字节数与 HTML 转义。

export function errorText(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === "string") {
    return error;
  }
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}

export function timeStamp(date = new Date()): string {
  return [date.getHours(), date.getMinutes(), date.getSeconds()]
    .map((part) => String(part).padStart(2, "0"))
    .join(":");
}

export function locale(): string {
  return getLang() === "zh" ? "zh-CN" : "en-US";
}

export function formatTimestamp(seconds: number | null | undefined): string {
  if (!seconds) {
    return t("common.unknown");
  }
  return new Date(seconds * 1000).toLocaleString(locale());
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return bytes + " B";
  }
  if (bytes < 1024 * 1024) {
    return (bytes / 1024).toFixed(1) + " KB";
  }
  return (bytes / (1024 * 1024)).toFixed(1) + " MB";
}

export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

import { el } from "./dom";
import { errorText, timeStamp } from "./format";
import { api } from "../api";

// 日志面板：本地缓存、追加与从后端拉取。

let logCache: string[] = [];

export function clearLog(): void {
  logCache = [];
  el.log.textContent = "";
}

export function appendLog(line: string): void {
  logCache.push(line);
  if (logCache.length > 400) {
    logCache = logCache.slice(-400);
  }
  el.log.textContent = logCache.join("\n");
  el.log.scrollTop = el.log.scrollHeight;
}

export function localLog(message: string): void {
  appendLog("[" + timeStamp() + "] " + message);
}

export async function loadLog(): Promise<void> {
  try {
    const lines = await api.getLog(200);
    logCache = lines.slice(-400);
    el.log.textContent = logCache.join("\n");
    el.log.scrollTop = el.log.scrollHeight;
  } catch (error) {
    localLog("读取日志失败：" + errorText(error));
  }
}

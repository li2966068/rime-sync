import { type Config, type Report } from "../api";

// 跨模块共享的可变状态：只在声明模块内赋值，其余模块只读或走 setter。

export let config: Config | null = null;
export let busy = false;
export let lastRun = Date.now();
/// 最近一次预览的动作条数：「按此执行」在没有改动时保持禁用。
export let plannedActions = 0;
export let lastReport: Report | null = null;

export function setConfig(next: Config | null): void {
  config = next;
}

export function setBusyFlag(value: boolean): void {
  busy = value;
}

export function touchLastRun(): void {
  lastRun = Date.now();
}

export function setPlannedActions(value: number): void {
  plannedActions = value;
}

export function setLastReport(report: Report | null): void {
  lastReport = report;
}

export function requireConfig(): Config {
  if (!config) {
    throw new Error("配置尚未加载");
  }
  return config;
}

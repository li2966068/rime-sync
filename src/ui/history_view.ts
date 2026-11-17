// 同步历史的「纯逻辑」部分：把一条条记录整理成人能看懂的一行行文字。
//
// 单独成模块、不掺 DOM 也不掺 i18n：这样「一行里到底该显示什么」能被纯逻辑测试
// 钉住（src/ui/history.test.ts，node --test 跑）。翻译与时间格式化由调用方注入
// （与 ui/prune_view.ts 同一套路）。

/** 后端返回的一条历史记录（字段名与 Rust 侧 serde 一致）。 */
export interface HistoryEntryInput {
  /** 同步发生的时间（Unix 秒）。 */
  ts: number;
  /** transport（只搬运）/ apply（合并生效）。 */
  mode: string;
  ok: boolean;
  pulled: number;
  pushed: number;
  merged: number;
  conflicts: number;
  duration_ms: number;
  backup_id: string | null;
  error: string | null;
}

/** 翻译函数（注入进来，便于测试不依赖 DOM 与 localStorage）。 */
export type Translate = (key: string, params?: Record<string, string | number>) => string;

/** 时间格式化函数（注入进来：统一用 ui/format.ts 里那一个，不另写一套）。 */
export type FormatTime = (seconds: number) => string;

/** 表格里的一行。 */
export interface HistoryRow {
  /** 什么时候做的。 */
  time: string;
  /** 怎么做的：搬运 / 合并生效。 */
  mode: string;
  /** 成没成。 */
  result: string;
  /** 拉取 / 推送 / 合并 各多少。 */
  counts: string;
  /** 有没有冲突。 */
  conflicts: string;
  /** 用了多久。 */
  duration: string;
  /** 失败时的原因（成功时是「—」）。 */
  detail: string;
}

/** 方式名：认得的翻成中文，不认得的原样显示（别把用户看晕）。 */
export function historyModeText(mode: string, translate: Translate): string {
  if (mode === "transport") {
    return translate("history.mode.transport");
  }
  if (mode === "apply") {
    return translate("history.mode.apply");
  }
  return mode;
}

/** 结果：成功 / 失败。 */
export function historyResultText(ok: boolean, translate: Translate): string {
  return ok ? translate("sync.verdictOk") : translate("sync.verdictFail");
}

/** 拉取 / 推送 / 合并 各多少。 */
export function historyCountsText(entry: HistoryEntryInput, translate: Translate): string {
  return translate("history.countsValue", {
    pulled: entry.pulled,
    pushed: entry.pushed,
    merged: entry.merged,
  });
}

/** 冲突个数。 */
export function historyConflictsText(count: number, translate: Translate): string {
  return translate("history.conflictValue", { count });
}

/** 耗时：不到一秒说毫秒，超过一秒说秒。 */
export function historyDurationText(ms: number, translate: Translate): string {
  if (!Number.isFinite(ms) || ms < 0) {
    return translate("common.unknown");
  }
  if (ms < 1000) {
    return translate("history.durationMs", { ms: Math.round(ms) });
  }
  return translate("history.durationSec", { seconds: (ms / 1000).toFixed(1) });
}

/** 详情：失败时写清楚失败原因，成功时是「—」。 */
export function historyDetailText(entry: HistoryEntryInput, translate: Translate): string {
  if (!entry.ok && entry.error) {
    return entry.error;
  }
  return translate("sync.none");
}

/** 把一条记录变成一行文字。 */
export function buildHistoryRow(
  entry: HistoryEntryInput,
  translate: Translate,
  formatTime: FormatTime,
): HistoryRow {
  return {
    time: formatTime(entry.ts),
    mode: historyModeText(entry.mode, translate),
    result: historyResultText(entry.ok, translate),
    counts: historyCountsText(entry, translate),
    conflicts: historyConflictsText(entry.conflicts, translate),
    duration: historyDurationText(entry.duration_ms, translate),
    detail: historyDetailText(entry, translate),
  };
}

/** 一行记录的纯文本。 */
export function historyRowText(row: HistoryRow): string {
  return [
    row.time,
    row.mode,
    row.result,
    row.counts,
    row.conflicts,
    row.duration,
    row.detail,
  ].join(" · ");
}

/** 没有任何记录？（空态提示用，别留一片空白） */
export function isHistoryEmpty(entries: HistoryEntryInput[]): boolean {
  return entries.length === 0;
}

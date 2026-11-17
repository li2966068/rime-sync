// 同步历史卡片「纯逻辑」的测试：一行记录到底该显示什么。
//
// 跑法：node --experimental-strip-types --test src/ui/history.test.ts
// （Node 22 自带 node:test，无需任何测试框架依赖。）
//
// 纯逻辑在 src/ui/history_view.ts（与 ui/prune_view.ts 同一套路：不掺 DOM、
// 不掺 i18n，翻译与时间格式化都由调用方注入），所以这里不依赖浏览器环境。

import assert from "node:assert/strict";
import test from "node:test";

import {
  buildHistoryRow,
  historyCountsText,
  historyDetailText,
  historyDurationText,
  historyModeText,
  historyResultText,
  historyRowText,
  isHistoryEmpty,
  type HistoryEntryInput,
} from "./history_view.ts";

/** 假翻译：把 key 与参数打出来，便于断言「用了哪些文案」。 */
const translate = (key: string, params?: Record<string, string | number>): string =>
  params ? `${key}:${JSON.stringify(params)}` : key;

/** 假时间格式化：记录收到的秒数即可（真实那个在 ui/format.ts 里）。 */
const formatTime = (seconds: number): string => `T${seconds}`;

/** 造一条历史记录。 */
function entry(overrides: Partial<HistoryEntryInput> = {}): HistoryEntryInput {
  return {
    ts: 1_700_000_000,
    mode: "transport",
    ok: true,
    pulled: 3,
    pushed: 1,
    merged: 2,
    conflicts: 0,
    duration_ms: 1200,
    backup_id: null,
    error: null,
    ...overrides,
  };
}

test("一行记录里该有的都有：时间、方式、结果、数量、冲突、耗时、说明", () => {
  const row = buildHistoryRow(entry(), translate, formatTime);
  assert.equal(row.time, "T1700000000");
  assert.equal(row.mode, "history.mode.transport");
  assert.equal(row.result, "sync.verdictOk");
  assert.equal(row.counts, 'history.countsValue:{"pulled":3,"pushed":1,"merged":2}');
  assert.equal(row.conflicts, 'history.conflictValue:{"count":0}');
  assert.equal(row.duration, 'history.durationSec:{"seconds":"1.2"}');
  assert.equal(row.detail, "sync.none");
});

test("方式名：搬运 / 合并生效认得出来，不认得的原样显示", () => {
  assert.equal(historyModeText("transport", translate), "history.mode.transport");
  assert.equal(historyModeText("apply", translate), "history.mode.apply");
  assert.equal(historyModeText("preview", translate), "preview");
});

test("结果分成功与失败两说", () => {
  assert.equal(historyResultText(true, translate), "sync.verdictOk");
  assert.equal(historyResultText(false, translate), "sync.verdictFail");
});

test("数量：拉取 / 推送 / 合并 三个数都在", () => {
  const text = historyCountsText(entry({ pulled: 5, pushed: 0, merged: 2 }), translate);
  assert.ok(text.includes('"pulled":5'));
  assert.ok(text.includes('"pushed":0'));
  assert.ok(text.includes('"merged":2'));
});

test("耗时：不到一秒说毫秒，超过一秒说秒（保留一位小数）", () => {
  assert.equal(historyDurationText(350, translate), 'history.durationMs:{"ms":350}');
  assert.equal(historyDurationText(999, translate), 'history.durationMs:{"ms":999}');
  assert.equal(historyDurationText(1000, translate), 'history.durationSec:{"seconds":"1.0"}');
  assert.equal(historyDurationText(8600, translate), 'history.durationSec:{"seconds":"8.6"}');
});

test("耗时不认识（负数 / NaN）时别硬编一个时间出来", () => {
  assert.equal(historyDurationText(-1, translate), "common.unknown");
  assert.equal(historyDurationText(Number.NaN, translate), "common.unknown");
});

test("失败时把失败原因写出来，成功时是「—」", () => {
  const failed = entry({ ok: false, error: "远端连接失败" });
  assert.equal(historyDetailText(failed, translate), "远端连接失败");

  // 失败但后端没给原因：也别显示成空白。
  assert.equal(historyDetailText(entry({ ok: false }), translate), "sync.none");
  assert.equal(historyDetailText(entry(), translate), "sync.none");
});

test("一行的纯文本：各段用「·」隔开", () => {
  const row = buildHistoryRow(entry(), translate, formatTime);
  const text = historyRowText(row);
  assert.equal(
    text,
    "T1700000000 · history.mode.transport · sync.verdictOk · " +
      'history.countsValue:{"pulled":3,"pushed":1,"merged":2} · ' +
      'history.conflictValue:{"count":0} · history.durationSec:{"seconds":"1.2"} · sync.none',
  );
});

test("空态：一条记录都没有时要能判出来（界面据此显示提示，不留空白）", () => {
  assert.equal(isHistoryEmpty([]), true);
  assert.equal(isHistoryEmpty([entry()]), false);
});

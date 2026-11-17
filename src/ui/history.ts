import { api, type HistoryEntry } from "../api";
import { t, type MessageKey } from "../i18n";
import { errorText, escapeHtml, formatTimestamp } from "./format";
import {
  buildHistoryRow,
  isHistoryEmpty,
  type FormatTime,
  type HistoryRow,
  type Translate,
} from "./history_view";

// 同步历史卡片：把最近几次同步的记录摊开给用户看。
//
// 这块界面是**只读**的：数据来自 core 写下的 <状态目录>/history.jsonl
// （每次搬运 / 合并生效追加一条），这里只读不写，也不提供「删除历史」
// 「重新同步」这类按钮 —— 同步本身还是从「同步」页那三个按钮发起。
//
// 卡片放在同步日志上方：日志是「现在正在发生什么」，历史是「过去发生了什么」。
//
// 纯逻辑（怎么把一条记录变成一行字）在 ui/history_view.ts 里，
// 这样它能被纯逻辑测试直接钉住（src/ui/history.test.ts）。

/** 翻译：把 i18n 的 t 包一层（纯逻辑模块只认 string key）。 */
const translate: Translate = (key, params) => t(key as MessageKey, params);

/** 时间格式化：统一用 ui/format.ts 里那一个。 */
const formatTime: FormatTime = (seconds) => formatTimestamp(seconds);

function node<T extends HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

/** 一行记录的 HTML（表格里的一格一格）。 */
function historyRowHtml(entry: HistoryEntry, row: HistoryRow): string {
  const resultClass = entry.ok ? "history-ok" : "history-fail";
  const detailClass = entry.ok ? "muted" : "history-error";
  return (
    "<tr>" +
    "<td>" +
    escapeHtml(row.time) +
    "</td><td>" +
    escapeHtml(row.mode) +
    '</td><td class="' +
    resultClass +
    '">' +
    escapeHtml(row.result) +
    "</td><td>" +
    escapeHtml(row.counts) +
    "</td><td>" +
    escapeHtml(row.conflicts) +
    "</td><td>" +
    escapeHtml(row.duration) +
    '</td><td class="' +
    detailClass +
    '">' +
    escapeHtml(row.detail) +
    "</td></tr>"
  );
}

/** 渲染历史列表（表格 + 空态提示）。 */
export function renderHistory(entries: HistoryEntry[]): void {
  const body = node<HTMLTableSectionElement>("history-body");
  const empty = node<HTMLParagraphElement>("history-empty");
  const table = node<HTMLTableElement>("history-table");

  const blank = isHistoryEmpty(entries);
  if (empty) {
    empty.hidden = !blank;
    empty.textContent = blank ? t("history.empty") : "";
  }
  if (table) {
    table.hidden = blank;
  }
  if (!body) {
    return;
  }
  body.innerHTML = entries
    .map((entry) => historyRowHtml(entry, buildHistoryRow(entry, translate, formatTime)))
    .join("");
}

/** 从后端读一次历史并渲染。 */
export async function loadHistory(): Promise<void> {
  const empty = node<HTMLParagraphElement>("history-empty");
  try {
    const entries = await api.listHistory();
    renderHistory(entries);
  } catch (error) {
    const body = node<HTMLTableSectionElement>("history-body");
    if (body) {
      body.innerHTML = "";
    }
    const table = node<HTMLTableElement>("history-table");
    if (table) {
      table.hidden = true;
    }
    if (empty) {
      empty.hidden = false;
      empty.textContent = t("history.loadFail", { error: errorText(error) });
    }
  }
}

import { api, type PrunePreview } from "../api";
import { t, type MessageKey } from "../i18n";
import { errorText } from "./format";
import { localLog } from "./logs";
import { toast } from "./toast";
import { buildPruneView, type PruneView } from "./prune_view";

// 方案裁剪（prune）的界面：先预演、后确认。
//
// 为什么要有这一步：程序以前在首启自举时直接删掉不在保留名单里的
// <id>.custom.yaml 并重写 schema_list，用户既没确认过、也没法预演，
// 「我正在用的方案凭空消失」就是这么来的。现在改成两次动作：
// 1. 点「预演」：后端只读地算出清单，一个字节都不动；
// 2. 看过清单点「确认删除」：后端才真的删；点「取消」：作废这次预演，
//    什么都不删。
//
// 卡片整块是运行时挂上去的（不走 index.html 的静态骨架）：这块界面只在
// 「用户主动想清理方案」时才用得上，静态骨架里放一堆隐藏节点不划算。

let current: PruneView | null = null;
let mounted = false;

/** 挂载「清理输入方案」卡片（幂等）。 */
export function mountPruneCard(): void {
  if (mounted) {
    return;
  }
  const panel = document.getElementById("panel-settings");
  if (!panel) {
    return;
  }
  mounted = true;

  const card = document.createElement("div");
  card.className = "card";
  card.id = "prune-card";

  const head = document.createElement("div");
  head.className = "card-head";
  const title = document.createElement("h3");
  title.setAttribute("data-i18n", "prune.title");
  const actions = document.createElement("div");
  actions.className = "card-actions";

  const cancelButton = document.createElement("button");
  cancelButton.className = "ghost small";
  cancelButton.id = "btn-prune-cancel";
  cancelButton.type = "button";
  cancelButton.setAttribute("data-i18n", "prune.cancel");
  cancelButton.hidden = true;

  const confirmButton = document.createElement("button");
  confirmButton.className = "small";
  confirmButton.id = "btn-prune-confirm";
  confirmButton.type = "button";
  confirmButton.setAttribute("data-i18n", "prune.confirm");
  confirmButton.hidden = true;

  const previewButton = document.createElement("button");
  previewButton.className = "ghost small";
  previewButton.id = "btn-prune-preview";
  previewButton.type = "button";
  previewButton.setAttribute("data-i18n", "prune.preview");

  actions.append(cancelButton, confirmButton, previewButton);
  head.append(title, actions);

  const desc = document.createElement("p");
  desc.className = "hint";
  desc.setAttribute("data-i18n", "prune.desc");

  const status = document.createElement("p");
  status.className = "hint";
  status.id = "prune-status";
  status.setAttribute("role", "status");

  const report = document.createElement("div");
  report.id = "prune-report";
  report.hidden = true;

  card.append(head, desc, status, report);
  // 放在「输入方案」卡片之前：两件事都是方案相关的，放一起好找。
  const schemaCard = document.getElementById("schema-card");
  if (schemaCard?.parentNode) {
    schemaCard.parentNode.insertBefore(card, schemaCard);
  } else {
    panel.append(card);
  }

  previewButton.addEventListener("click", () => {
    void runPrunePreview();
  });
  confirmButton.addEventListener("click", () => {
    void confirmPrune();
  });
  cancelButton.addEventListener("click", () => {
    void cancelPrune();
  });
}

/** 预演：只读地拿到清单，一个字节都不动。 */
export async function runPrunePreview(): Promise<void> {
  const button = document.getElementById("btn-prune-preview") as HTMLButtonElement | null;
  const status = document.getElementById("prune-status");
  const report = document.getElementById("prune-report");
  if (button) {
    button.disabled = true;
  }
  if (status) {
    status.textContent = t("prune.previewing");
  }
  try {
    const preview: PrunePreview = await api.prunePreview();
    current = buildPruneView(preview, (key, params) => t(key as MessageKey, params));
    renderPruneReport(report, current);
    if (status) {
      status.textContent = current.headline;
    }
    localLog("预演方案清理：" + current.headline);
  } catch (error) {
    current = null;
    if (status) {
      status.textContent = t("prune.previewFailed", { error: errorText(error) });
    }
    if (report) {
      report.hidden = true;
    }
    localLog("预演方案清理失败：" + errorText(error));
  } finally {
    if (button) {
      button.disabled = false;
    }
    setButtons();
  }
}

/** 确认执行：后端凭预演令牌才真的删。 */
export async function confirmPrune(): Promise<void> {
  if (!current || !current.canConfirm) {
    return;
  }
  const confirmButton = document.getElementById("btn-prune-confirm") as HTMLButtonElement | null;
  const status = document.getElementById("prune-status");
  if (confirmButton) {
    confirmButton.disabled = true;
  }
  try {
    const report = await api.pruneConfirm(current.token);
    current = null;
    if (status) {
      status.textContent = t("prune.done", { count: report.removed.length });
    }
    const reportBox = document.getElementById("prune-report");
    if (reportBox) {
      reportBox.hidden = true;
    }
    const message = t("prune.done", { count: report.removed.length });
    toast(message);
    localLog(message);
  } catch (error) {
    if (status) {
      status.textContent = t("prune.failed", { error: errorText(error) });
    }
    localLog("确认方案清理失败：" + errorText(error));
  } finally {
    if (confirmButton) {
      confirmButton.disabled = false;
    }
    setButtons();
  }
}

/** 取消这次预演：作废令牌，不产生任何副作用。 */
export async function cancelPrune(): Promise<void> {
  const token = current?.token;
  current = null;
  const status = document.getElementById("prune-status");
  const report = document.getElementById("prune-report");
  try {
    if (token) {
      await api.pruneCancel(token);
    }
  } catch (error) {
    // 取消失败只记日志：取消本来就不删东西，失败也不影响用户。
    localLog("取消预演失败：" + errorText(error));
  }
  if (status) {
    status.textContent = t("prune.cancelled");
  }
  if (report) {
    report.hidden = true;
  }
  setButtons();
}

/** 按当前状态显示/隐藏「确认」「取消」两个按钮。 */
function setButtons(): void {
  const confirmButton = document.getElementById("btn-prune-confirm") as HTMLButtonElement | null;
  const cancelButton = document.getElementById("btn-prune-cancel") as HTMLButtonElement | null;
  const visible = current !== null;
  if (confirmButton) {
    confirmButton.hidden = !visible || !current?.canConfirm;
  }
  if (cancelButton) {
    cancelButton.hidden = !visible;
  }
}

/** 渲染预演清单：删哪些文件、schema_list 变成什么样、哪些方案会消失。 */
function renderPruneReport(box: HTMLElement | null, view: PruneView): void {
  if (!box) {
    return;
  }
  box.innerHTML = "";
  box.hidden = false;

  // 1) 会被删的文件
  box.append(section(t("prune.filesTitle"), view.fileLines, "mono"));
  // 2) 删除后的 schema_list
  const schemaLines: string[] = [];
  for (const block of view.schemaListBlocks) {
    schemaLines.push(block.path);
    schemaLines.push(block.after);
  }
  box.append(section(t("prune.schemaListTitle"), schemaLines, "mono"));
  // 3) 会消失的方案（人话）
  box.append(section(t("prune.affectedTitle"), view.affected));
  if (view.warnings.length > 0) {
    box.append(section(t("prune.warningTitle"), view.warnings));
  }
}

/** 一个带标题的小节（标题 + 内容行）。 */
function section(title: string, lines: string[], extraClass = ""): HTMLElement {
  const wrap = document.createElement("div");
  wrap.className = "prune-section";
  const head = document.createElement("strong");
  head.textContent = title;
  wrap.append(head);
  if (lines.length === 0) {
    const empty = document.createElement("p");
    empty.className = "hint";
    empty.textContent = t("prune.empty");
    wrap.append(empty);
    return wrap;
  }
  const list = document.createElement("ul");
  list.className = extraClass ? "hint " + extraClass : "hint";
  for (const line of lines) {
    const item = document.createElement("li");
    item.textContent = line;
    list.append(item);
  }
  wrap.append(list);
  return wrap;
}

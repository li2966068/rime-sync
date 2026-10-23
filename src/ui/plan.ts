import { api, type PlanKind, type PlanReport } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText } from "./format";
import { toast } from "./toast";
import { localLog } from "./logs";
import { setBusy } from "./sync";
import { refreshNetwork } from "./connection";
import { busy, setPlannedActions } from "./state";

// 预览计划页：分组渲染与「按此执行」。

/// 预览分组顺序：写动作在前，跳过在最后（与真实执行顺序一致）。
export const PLAN_ORDER: PlanKind[] = [
  "pull",
  "push",
  "conflict",
  "delete_local",
  "delete_remote",
  "skip",
];

/// 单个分组最多渲染多少行，避免一次预览撑爆页面。
export const PLAN_MAX_ROWS = 200;

export function planGroupLabel(kind: PlanKind): string {
  switch (kind) {
    case "pull":
      return t("plan.kindPull");
    case "push":
      return t("plan.kindPush");
    case "conflict":
      return t("plan.kindConflict");
    case "delete_local":
      return t("plan.kindDeleteLocal");
    case "delete_remote":
      return t("plan.kindDeleteRemote");
    case "skip":
      return t("plan.kindSkip");
  }
}

export function renderPlan(plan: PlanReport): void {
  // 失败或没有动作时「按此执行」保持禁用。
  setPlannedActions(plan.ok ? plan.actions.length : 0);
  el.planCard.hidden = false;
  el.planSummary.textContent = plan.ok
    ? t("plan.summary", {
        total: plan.total,
        pull: plan.counts.pull,
        push: plan.counts.push,
        conflict: plan.counts.conflict,
        remove: plan.counts.delete_local + plan.counts.delete_remote,
        skip: plan.counts.skip,
      })
    : t("plan.fail", { error: plan.error ?? "" });
  // 只读策略下「推送/删除」不会真的发生，必须明说，否则预览会误导用户。
  el.planNote.hidden = plan.mode === "normal";
  el.planNote.textContent =
    plan.mode === "normal" ? "" : t("plan.readOnlyMode", { mode: plan.mode });

  el.planGroups.textContent = "";
  let shown = 0;
  for (const kind of PLAN_ORDER) {
    if (shown >= PLAN_MAX_ROWS) {
      break;
    }
    const actions = plan.actions.filter((action) => action.kind === kind);
    if (actions.length === 0) {
      continue;
    }
    const group = document.createElement("section");
    group.className = "plan-group";
    const head = document.createElement("h3");
    head.textContent = planGroupLabel(kind) + " · " + actions.length;
    const list = document.createElement("ul");
    for (const action of actions) {
      if (shown >= PLAN_MAX_ROWS) {
        break;
      }
      shown += 1;
      const item = document.createElement("li");
      const path = document.createElement("code");
      path.textContent = action.path;
      const detail = document.createElement("span");
      detail.className = "plan-detail";
      detail.textContent = action.detail;
      item.append(path, detail);
      list.append(item);
    }
    group.append(head, list);
    el.planGroups.append(group);
  }

  if (plan.total === 0) {
    const empty = document.createElement("p");
    empty.className = "muted";
    empty.textContent = plan.ok ? t("plan.empty") : t("plan.fail", { error: plan.error ?? "" });
    el.planGroups.append(empty);
  } else if (plan.total > shown) {
    const more = document.createElement("p");
    more.className = "muted";
    more.textContent = t("plan.more", { count: plan.total - shown });
    el.planGroups.append(more);
  }
  el.planRun.disabled = plan.actions.length === 0 || !plan.ok;
}

export async function previewSync(): Promise<void> {
  if (busy) {
    return;
  }
  setBusy(true, t("busy.plan"));
  el.progress.classList.add("is-running");
  el.progressLabel.textContent = t("sync.startedPlan");
  el.progressCount.textContent = "";
  el.progressFill.style.width = "8%";
  try {
    const plan = await api.planSync();
    renderPlan(plan);
    el.progressFill.style.width = "100%";
    el.progressLabel.textContent = plan.ok ? t("plan.done") : t("sync.progressFailed");
    localLog(
      t("plan.log", {
        total: plan.total,
        pull: plan.counts.pull,
        push: plan.counts.push,
        conflict: plan.counts.conflict,
      }),
    );
    if (!plan.ok && plan.error) {
      localLog(t("sync.reportError", { error: plan.error }));
    }
  } catch (error) {
    el.progressLabel.textContent = t("sync.progressFailed");
    toast(t("plan.fail", { error: errorText(error) }));
  } finally {
    setBusy(false);
    el.progress.classList.remove("is-running");
    void refreshNetwork();
  }
}

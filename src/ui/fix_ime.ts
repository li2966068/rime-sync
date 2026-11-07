import { api, type FixImeReport } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText } from "./format";
import { toast } from "./toast";
import { localLog } from "./logs";
import { refreshImeStatus } from "./ime_banner";

// 输入法产物自举：与启动自举、CLI fix-ime 走同一套后端逻辑（core::prebuilt）。

export async function runFixIme(force = false): Promise<void> {
  // 两个按钮同时禁用：并发跑两次会互相覆盖同一个 build/，没有任何好处。
  const buttons = [el.fixIme, el.fixImeForce];
  const labels = buttons.map((button) => button.textContent);
  for (const button of buttons) {
    button.disabled = true;
    // 0.6.23：后端改成 async + spawn_blocking，界面不再冻结；把「处理中…」写到
    // 按钮上，用户才知道点上了（安装输入法最多要等几分钟）。
    button.textContent = t("fixIme.working");
  }
  el.fixImeStatus.textContent = t("fixIme.running");
  try {
    const report = await api.fixIme(force);
    el.fixImeStatus.textContent = describe(report);
    const buildMessage = outcomeLine(report);
    // 输入法本体装回来 / 装不上都要单独提示：这才是用户最直接的感受。
    const weaselMessage =
      report.weasel_install === "installed" || report.weasel_install === "failed"
        ? weaselLine(report)
        : "";
    // B-2：config 模板被跳过 = 该输入法**没有 schema_list**（用户没有方案可选）。
    // 这条必须单独说，不能埋在明细里。
    const configMessage = configSkippedLine(report);
    const message = [buildMessage, weaselMessage, configMessage]
      .filter(Boolean)
      .join("；");
    toast(message);
    localLog(message);
  } catch (error) {
    const raw = errorText(error);
    // 0.6.23：后端同一时刻只允许一个修复任务；并发时返回 fix-ime-busy，
    // 用界面语言提示（其它错误照旧显示原文，方便排障）。
    const text = raw.includes("fix-ime-busy")
      ? t("fixIme.busy")
      : t("fixIme.fail", { error: raw });
    el.fixImeStatus.textContent = text;
    toast(text);
    localLog(text);
  } finally {
    buttons.forEach((button, index) => {
      button.disabled = false;
      button.textContent = labels[index];
    });
    // 修复结果同时反映到红条：装上了就消失，仍需管理员权限就保留。
    void refreshImeStatus();
  }
}

/// 按后端状态机（ok / noop / partial / failed）给结论。
///
/// 0.6.24：以前这里只看 build_ok，于是「输入法安装失败但 build/ 还在」这种
/// failed 也被报成「产物已就绪」。判据必须跟着后端走。
function outcomeLine(report: FixImeReport): string {
  const okText = t("fixIme.ok", {
    copied: report.copied,
    kept: report.kept,
    source: report.payload_source || report.source,
  });
  const detail = report.detail || "";
  switch (report.result) {
    case "ok":
    case "noop":
      return okText;
    case "partial":
      // detail 是后端的中文技术描述：标成"技术细节"，避免英文界面中英混排。
      return detail
        ? t("fixIme.partialDetail", { text: detail })
        : t("fixIme.partial", { detail: okText });
    case "failed":
    default:
      return t("fixIme.fail", {
        error:
          detail ||
          t("fixIme.bad", {
            copied: report.copied + report.overwritten,
            probe: report.probe_after,
          }),
      });
  }
}

function describe(report: FixImeReport): string {
  const parts = [
    t("fixIme.detail", {
      copied: report.copied,
      kept: report.kept,
      overwritten: report.overwritten,
      config: report.config_copied,
    }),
    t("fixIme.sourceLine", { source: report.payload_source || report.source }),
    t("fixIme.buildSchemas", { list: schemaLine(report) }),
    t("fixIme.buildSource", { source: report.build_source }),
    t("fixIme.deploy", {
      method: report.deploy_method,
      result: report.deploy_result,
      ms: report.deploy_ms,
    }),
  ];
  if (report.escalated) {
    parts.push(t("fixIme.escalated"));
  }
  const configLine = configSkippedLine(report);
  if (configLine) {
    parts.push(configLine);
  }
  parts.push(weaselLine(report));
  if (report.result === "partial" || report.result === "failed") {
    parts.push(t("fixIme.resultLine", { result: report.result, detail: report.detail }));
  }
  if (report.needs_elevation || report.weasel_needs_elevation) {
    parts.push(t("fixIme.needsElevation"));
  }
  if (report.log_path) {
    parts.push(t("fixIme.log", { path: report.log_path }));
  }
  return parts.join(" · ");
}

/// B-2：config 模板被 data 门禁挡下时的提示行。
///
/// 空数组 = 没有跳过（正常）。非空 = 用户拿到的输入法**没有 schema_list**：
/// default.custom.yaml 没能写进去，所以方案列表是空的。
function configSkippedLine(report: FixImeReport): string {
  const skipped = report.config_skipped ?? [];
  if (skipped.length === 0) {
    return "";
  }
  return t("fixIme.configSkipped", { list: skipped.join(", ") });
}

/// 每个方案的状态：id:ok / id:repaired / id:missing。
function schemaLine(report: FixImeReport): string {
  const schemas = report.build_schemas ?? [];
  if (schemas.length === 0) {
    return "-";
  }
  return schemas.map((schema) => schema.id + ":" + schema.state).join(" · ");
}

/// 输入法本体的状态行（与后端 ImeReport.install 一一对应）。
function weaselLine(report: FixImeReport): string {
  const version = report.weasel_version || "?";
  switch (report.weasel_install) {
    case "installed":
      return t("fixIme.weaselInstalled", { version });
    case "already-present":
      return t("fixIme.weaselPresent", { version });
    case "skipped(platform)":
      return t("fixIme.weaselSkipped");
    case "disabled":
      return t("fixIme.weaselDisabled");
    default:
      return t("fixIme.weaselFailed", {
        error: report.weasel_error || report.weasel_install,
      });
  }
}

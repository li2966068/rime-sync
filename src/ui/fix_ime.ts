import { api, type FixImeReport } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText } from "./format";
import { toast } from "./toast";
import { localLog } from "./logs";

// 输入法产物自举：与启动自举、CLI fix-ime 走同一套后端逻辑（core::prebuilt）。

export async function runFixIme(force = false): Promise<void> {
  // 两个按钮同时禁用：并发跑两次会互相覆盖同一个 build/，没有任何好处。
  const buttons = [el.fixIme, el.fixImeForce];
  for (const button of buttons) {
    button.disabled = true;
  }
  el.fixImeStatus.textContent = t("fixIme.running");
  try {
    const report = await api.fixIme(force);
    el.fixImeStatus.textContent = describe(report);
    const buildMessage = report.build_ok
      ? t("fixIme.ok", {
          copied: report.copied,
          kept: report.kept,
          source: report.payload_source || report.source,
        })
      : t("fixIme.bad", {
          copied: report.copied + report.overwritten,
          probe: report.probe_after,
        });
    // 输入法本体装回来 / 装不上都要单独提示：这才是用户最直接的感受。
    const weaselMessage =
      report.weasel_install === "installed" || report.weasel_install === "failed"
        ? weaselLine(report)
        : "";
    const message = [buildMessage, weaselMessage].filter(Boolean).join("；");
    toast(message);
    localLog(message);
  } catch (error) {
    const text = t("fixIme.fail", { error: errorText(error) });
    el.fixImeStatus.textContent = text;
    toast(text);
    localLog(text);
  } finally {
    for (const button of buttons) {
      button.disabled = false;
    }
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
  ];
  if (report.escalated) {
    parts.push(t("fixIme.escalated"));
  }
  parts.push(weaselLine(report));
  if (report.needs_elevation || report.weasel_needs_elevation) {
    parts.push(t("fixIme.needsElevation"));
  }
  if (report.log_path) {
    parts.push(t("fixIme.log", { path: report.log_path }));
  }
  return parts.join(" · ");
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

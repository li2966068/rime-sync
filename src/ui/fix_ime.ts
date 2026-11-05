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
    const message = report.build_ok
      ? t("fixIme.ok", {
          copied: report.copied,
          kept: report.kept,
          source: report.payload_source || report.source,
        })
      : t("fixIme.bad", {
          copied: report.copied + report.overwritten,
          probe: report.probe_after,
        });
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
  if (report.log_path) {
    parts.push(t("fixIme.log", { path: report.log_path }));
  }
  return parts.join(" · ");
}

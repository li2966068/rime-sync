import { api, type Config, type ProbeResult } from "../api";
import { t, presetHint } from "../i18n";
import { el, setStatus } from "./dom";
import { errorText } from "./format";
import { toast } from "./toast";
import { localLog } from "./logs";
import { config, requireConfig } from "./state";
import { renderConfig, renderFirstSyncBanner } from "./settings";
import { refreshSecretStatus } from "./secret";
import { loadWatchStatus } from "./watch";
import { loadBackups } from "./backups";
import { loadConflicts } from "./conflicts";
import { guessPreset } from "./presets";

// 同步设置向导（首启 / --setup 共用）。

export interface WizardState {
  step: number;
  policy: "preview" | "normal";
  mode: "first-run" | "settings";
  finished: boolean;
}

export let wizard: WizardState = { step: 1, policy: "preview", mode: "first-run", finished: false };

/* ---------- 同步设置向导（首启 / --setup 共用） ---------- */

export function wizardTitleText(): string {
  return t(wizard.mode === "settings" ? "wizard.titleSetup" : "wizard.titleFirstRun");
}

export function setWizardPolicy(policy: "preview" | "normal"): void {
  wizard.policy = policy;
  for (const input of el.wizardStrategy.querySelectorAll<HTMLInputElement>(
    'input[name="wizard-strategy"]',
  )) {
    input.checked = input.value === policy;
  }
  el.wizardPolicyHint.textContent = t(policy === "normal" ? "wizard.normalHint" : "wizard.previewHint");
}

export function setWizardStep(step: number): void {
  wizard.step = step;
  for (const section of el.wizardSteps) {
    section.classList.toggle("is-active", section.dataset.wizardStep === String(step));
  }
  el.wizardStepLabel.textContent = step >= 5 ? "" : t("wizard.step", { current: step, total: 4 });
  el.wizardBar.style.width = Math.min(100, (Math.min(step, 4) / 4) * 100) + "%";
  el.wizardBack.disabled = step <= 1 || step >= 5;
  el.wizardSkip.hidden = step !== 3;
  el.wizardNext.disabled = false;
  el.wizardNext.textContent =
    step >= 5 ? t("wizard.enter") : step === 4 ? t("wizard.finish") : t("common.next");
}

export function openWizard(mode: "first-run" | "settings" = "first-run"): void {
  wizard = { step: 1, policy: "preview", mode, finished: false };
  el.wizardTitle.textContent = wizardTitleText();
  const base = config;
  if (base) {
    el.wizardRime.value = base.rime_dir ?? "";
    el.wizardDevice.value = base.device_id;
    el.wizardUrl.value = base.webdav.url;
    el.wizardUser.value = base.webdav.username;
    el.wizardPass.value = "";
    el.wizardDir.value = base.webdav.remote_dir;
    const selected = guessPreset(base.webdav.url);
    el.wizardPreset.value = selected;
    el.wizardPresetHint.textContent = presetHint(selected);
    setWizardPolicy(base.first_sync_policy === "normal" ? "normal" : "preview");
  } else {
    el.wizardDevice.value = "";
    setWizardPolicy("preview");
  }
  el.wizardResult.hidden = true;
  el.wizardSummary.hidden = true;
  el.wizardDetails.hidden = true;
  el.wizardStatus.hidden = true;
  el.wizard.hidden = false;
  setWizardStep(1);
  void autoDetectWizard();
}

export async function autoDetectWizard(): Promise<void> {
  el.wizardRimeStatus.textContent = t("sync.detecting");
  try {
    const detection = await api.detectRimeDir();
    if (!el.wizardRime.value) {
      el.wizardRime.value = detection.path;
    }
    el.wizardRimeStatus.textContent = t("sync.source", { source: detection.source });
  } catch {
    el.wizardRimeStatus.textContent = t("sync.notDetected");
  }
}

export function wizardDraft(): Config {
  return {
    ...requireConfig(),
    rime_dir: el.wizardRime.value.trim() || null,
    device_id: el.wizardDevice.value.trim(),
    first_sync_policy: wizard.policy,
    webdav: {
      url: el.wizardUrl.value.trim(),
      username: el.wizardUser.value.trim(),
      password: el.wizardPass.value,
      remote_dir: el.wizardDir.value.trim(),
    },
  };
}

export async function saveWizardDraft(): Promise<boolean> {
  try {
    const saved = await api.setConfig(wizardDraft());
    renderConfig(saved);
    // 留空设备名时后端会自动生成，回填给界面避免下次保存又生成新的。
    el.wizardDevice.value = saved.device_id;
    void refreshSecretStatus();
    return true;
  } catch (error) {
    toast(t("conn.saveFail", { error: errorText(error) }));
    return false;
  }
}

export function wizardDetailsText(result: ProbeResult): string[] {
  const lines = [
    t("wizard.testLatency", { ms: result.latency_ms }),
    t("wizard.testServer", { server: result.server_type || result.server || "?" }),
    t("wizard.testRemoteDir", { dir: result.remote_dir || "/" }),
  ];
  if (result.remote_dir_missing) {
    lines.push(t("wizard.testDirMissing"));
  }
  return lines;
}

export async function wizardTest(): Promise<void> {
  if (!el.wizardUrl.value.trim() || !el.wizardUser.value.trim()) {
    setStatus(el.wizardStatus, "err", t("wizard.required"));
    return;
  }
  if (!(await saveWizardDraft())) {
    return;
  }
  el.wizardTest.disabled = true;
  el.wizardDetails.hidden = true;
  setStatus(el.wizardStatus, "idle", t("wizard.testing"));
  try {
    const result = await api.testConnection();
    const lines = wizardDetailsText(result);
    if (!result.ok) {
      setStatus(el.wizardStatus, "err", t("wizard.testFail", { error: result.message || t("conn.testFail") }));
    } else {
      // 写权限只在 normal 下必需；preview 下写失败只提示，不阻塞流程。
      const probe = await api.probeWrite();
      if (probe.ok) {
        setStatus(el.wizardStatus, "ok", t("wizard.testOk"));
      } else {
        lines.push(
          t("wizard.testWriteFail", {
            step: probe.step,
            status: probe.http_status,
            hint: probe.hint ? " · " + probe.hint : "",
          }),
        );
        setStatus(el.wizardStatus, "err", t("wizard.testWriteOnlyFail"));
      }
    }
    el.wizardDetails.hidden = false;
    el.wizardDetails.textContent = lines.join("\n");
  } catch (error) {
    setStatus(el.wizardStatus, "err", t("wizard.testFail", { error: errorText(error) }));
  } finally {
    el.wizardTest.disabled = false;
  }
}

export function closeWizard(): void {
  el.wizard.hidden = true;
  el.wizardPass.value = "";
  wizard.finished = true;
  localLog(t("wizard.done"));
  renderFirstSyncBanner();
  void loadWatchStatus();
  void loadBackups();
  void loadConflicts();
}

// 「完成」：保存完整配置（含 first_sync_policy），按需执行一次首次同步并展示 pulled/pushed/mode。
export async function wizardFinish(): Promise<void> {
  if (!(await saveWizardDraft())) {
    return;
  }
  el.wizardNext.disabled = true;
  el.wizardBack.disabled = true;
  el.wizardResult.hidden = true;
  let summary: string | null = null;
  let failed = false;
  if (el.wizardRunFirst.checked) {
    el.wizardResult.hidden = false;
    el.wizardResult.textContent = t("wizard.running");
    try {
      // preview 走只拉分支；normal 是用户显式选择的双向。
      const result = await api.firstTransport(wizard.policy === "normal" ? "bidirectional" : "pull");
      const report = result.report;
      const lines = [
        report.ok
          ? t("wizard.firstDone")
          : t("wizard.firstFail", { error: report.error ?? t("sync.verdictFail") }),
        t("wizard.firstStats", {
          pulled: report.pulled,
          pushed: report.pushed,
          mode: report.mode ?? result.strategy,
        }),
      ];
      if (report.backup_id) {
        lines.push(t("sync.lastBackup", { id: report.backup_id }));
      }
      if (result.note) {
        lines.push(result.note);
      }
      el.wizardResult.textContent = lines.join("\n");
      summary = lines.slice(0, 2).join("\n");
      failed = !report.ok;
    } catch (error) {
      failed = true;
      el.wizardResult.textContent = t("wizard.firstFail", { error: errorText(error) });
    }
  } else {
    summary = t("wizard.firstSkipped", {
      mode: t(wizard.policy === "normal" ? "wizard.normal" : "wizard.preview"),
    });
  }
  el.wizardNext.disabled = false;
  if (failed) {
    el.wizardBack.disabled = false;
    return;
  }
  wizard.finished = true;
  const notes: string[] = [];
  if (summary) {
    notes.push(summary);
  }
  if (wizard.policy !== "normal") {
    notes.push(t("wizard.switchLater"));
  }
  el.wizardSummary.hidden = false;
  el.wizardSummary.textContent = notes.join("\n\n");
  setWizardStep(5);
}

export async function wizardNext(): Promise<void> {
  switch (wizard.step) {
    case 1: {
      const value = el.wizardRime.value.trim();
      if (!value) {
        el.wizardRimeStatus.textContent = t("wizard.pathMissing");
        return;
      }
      setWizardStep(2);
      return;
    }
    case 2: {
      if (!el.wizardUrl.value.trim() || !el.wizardUser.value.trim()) {
        toast(t("wizard.required"));
        return;
      }
      if (!(await saveWizardDraft())) {
        return;
      }
      setWizardStep(3);
      return;
    }
    case 3:
      setWizardStep(4);
      return;
    case 4:
      await wizardFinish();
      return;
    default:
      closeWizard();
      return;
  }
}

export function wizardBack(): void {
  if (wizard.step > 1 && wizard.step < 5) {
    setWizardStep(wizard.step - 1);
  }
}

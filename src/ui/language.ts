import { getLang, t, presetHint } from "../i18n";
import { el, setFooter } from "./dom";
import { config, lastReport } from "./state";
import { conflicts, activeConflict, renderConflictList, renderDiff } from "./conflicts";
import { renderDevices } from "./devices";
import { renderBackups } from "./backups";
import { renderSchemas } from "./schemas";
import { renderDedupe } from "./dedupe";
import { renderUpdate } from "./update";
import { renderWatch } from "./watch";
import { renderFirstSyncBanner } from "./settings";
import { wizard, wizardTitleText } from "./wizard";
import { renderReport } from "./sync";
import { applySecretText, getSecretState } from "./secret";

// 语言切换后重绘所有动态文案。

export function refreshLanguageDependent(): void {
  document.title = t("app.name");
  el.togglePass.textContent = el.pass.type === "text" ? t("conn.hide") : t("conn.show");
  el.diagCopy.textContent = t("settings.diagCopy");
  el.language.value = getLang();
  el.presetHint.textContent = presetHint(el.preset.value);
  el.wizardPresetHint.textContent = presetHint(el.wizardPreset.value);
  el.rimeDirSource.textContent = config?.rime_dir ? t("sync.saved") : t("sync.notDetected");
  el.progressLabel.textContent = t("sync.progressWaiting");
  el.conflictsCount.textContent = t("conflicts.count", { count: conflicts.length });
  renderFirstSyncBanner();
  renderWatch();
  renderConflictList();
  if (activeConflict) {
    const entry = conflicts.find((item) => item.path === activeConflict);
    if (entry) {
      renderDiff(entry);
    }
  }
  renderDevices();
  renderBackups();
  renderSchemas();
  renderDedupe();
  renderUpdate();
  if (lastReport) {
    renderReport(lastReport);
  }
  const secretStatus = getSecretState();
  if (secretStatus) {
    applySecretText(secretStatus);
  }
  if (!el.wizard.hidden) {
    el.wizardTitle.textContent = wizardTitleText();
    el.wizardPolicyHint.textContent = t(
      wizard.policy === "normal" ? "wizard.normalHint" : "wizard.previewHint",
    );
    el.wizardNext.textContent =
      wizard.step >= 5
        ? t("wizard.enter")
        : wizard.step === 4
          ? t("wizard.finish")
          : t("common.next");
    el.wizardStepLabel.textContent =
      wizard.step >= 5 ? "" : t("wizard.step", { current: wizard.step, total: 4 });
  }
  if (config) {
    el.footerConfig.textContent = config.device_id ? t("footer.device", { id: config.device_id }) : "";
  }
  if (!lastReport) {
    setFooter(t("footer.ready"));
  }
}

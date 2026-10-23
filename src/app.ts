import { el, setFooter, setStatus } from "./ui/dom";
import { config, busy, lastRun, touchLastRun, setLastReport } from "./ui/state";
import { errorText } from "./ui/format";
import { toast } from "./ui/toast";
import { clearLog, localLog, loadLog } from "./ui/logs";
import { settleModal } from "./ui/modal";
import { refreshSecretStatus } from "./ui/secret";
import { beginSaveBackupKeep, loadBackups, createBackup, deleteBackup, pruneBackups, restoreBackup } from "./ui/backups";
import { renderConfig, switchToTwoWay, saveRimeDir, pickRimeDir, detectRimeDir, randomDeviceId, settleDeviceChoice, saveSettings } from "./ui/settings";
import { saveConnection, testConnection, probeWrite, listRemote, refreshNetwork } from "./ui/connection";
import { renderProgress, renderReport, run } from "./ui/sync";
import { previewSync } from "./ui/plan";
import { conflicts, activeConflict, renderDiff, loadConflicts, resolveOne, confirmResolve, resolveFromList, resolveAll } from "./ui/conflicts";
import { loadDevices, revokeDevice } from "./ui/devices";
import { updateDedupeApply, loadDedupe, applyDedupe } from "./ui/dedupe";
import { copyDiagPath, exportDiagnostics } from "./ui/diagnostics";
import { applyAppVersion, renderUpdate, checkUpdate, installUpdate } from "./ui/update";
import { applyWatchEvent, renderWatch, toggleWatch, loadWatchStatus } from "./ui/watch";
import { loadPresets, applyPresetTo } from "./ui/presets";
import { setWizardPolicy, setWizardStep, openWizard, autoDetectWizard, wizardTest, wizardNext, wizardBack } from "./ui/wizard";
import { refreshLanguageDependent } from "./ui/language";
import { api, type Report, type Progress, type ConflictChoice, type WatchEvent } from "./api";
import { getLang, setLang, t, applyI18n } from "./i18n";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";

// 应用装配：标签切换、事件绑定、订阅与启动。

export function switchPanel(name: string): void {
  for (const tab of el.tabs) {
    const active = tab.dataset.panel === name;
    tab.classList.toggle("is-active", active);
    tab.setAttribute("aria-selected", String(active));
  }
  for (const panel of el.panels) {
    panel.classList.toggle("is-active", panel.id === "panel-" + name);
  }
  if (name === "conflicts") {
    void loadConflicts();
  }
  if (name === "devices") {
    void loadDevices();
  }
  if (name === "settings") {
    void loadBackups();
  }
}

/* ---------- 事件绑定 ---------- */

export function bindEvents(): void {
  for (const tab of el.tabs) {
    tab.addEventListener("click", () => {
      const name = tab.dataset.panel;
      if (name) {
        switchPanel(name);
      }
    });
  }

  el.togglePass.addEventListener("click", () => {
    const showing = el.pass.type === "text";
    el.pass.type = showing ? "password" : "text";
    el.togglePass.textContent = showing ? t("conn.show") : t("conn.hide");
    el.togglePass.setAttribute("aria-pressed", String(!showing));
  });

  el.saveConn.addEventListener("click", () => {
    void saveConnection().catch(() => undefined);
  });
  el.test.addEventListener("click", () => {
    void testConnection();
  });
  el.probe.addEventListener("click", () => {
    void probeWrite();
  });
  el.listRemote.addEventListener("click", () => {
    void listRemote();
  });
  el.preset.addEventListener("change", () => {
    void applyPresetTo(el.preset.value, el.url, el.presetHint, el.preset);
  });

  el.detect.addEventListener("click", () => {
    void detectRimeDir();
  });
  el.browseRime.addEventListener("click", () => {
    void pickRimeDir(el.rimeDir, el.rimeDirSource, true);
  });
  el.wizardBrowse.addEventListener("click", () => {
    void pickRimeDir(el.wizardRime, el.wizardRimeStatus, false);
  });
  el.rimeDir.addEventListener("change", () => {
    void saveRimeDir();
  });
  el.watchToggle.addEventListener("change", () => {
    void toggleWatch(el.watchToggle.checked);
  });
  el.firstSyncSwitch.addEventListener("click", () => {
    void switchToTwoWay();
  });
  el.plan.addEventListener("click", () => {
    void previewSync();
  });
  el.planRun.addEventListener("click", () => {
    el.planCard.hidden = true;
    void run("transport");
  });
  el.planClose.addEventListener("click", () => {
    el.planCard.hidden = true;
  });
  el.transport.addEventListener("click", () => {
    void run("transport");
  });
  el.apply.addEventListener("click", () => {
    void run("apply");
  });
  el.cancel.addEventListener("click", () => {
    void api.cancel().then(
      () => {
        localLog(t("sync.cancelRequested"));
        setFooter(t("sync.cancelled"));
      },
      (error: unknown) => {
        toast(t("sync.cancelFail", { error: errorText(error) }));
      },
    );
  });

  el.refreshLog.addEventListener("click", () => {
    void loadLog();
  });
  el.clearLog.addEventListener("click", () => {
    clearLog();
  });

  el.conflictsRefresh.addEventListener("click", () => {
    void loadConflicts();
  });
  el.conflictsKeepAll.addEventListener("click", () => {
    void resolveAll("local");
  });
  el.conflictsTakeAll.addEventListener("click", () => {
    void resolveAll("conflict");
  });
  el.conflictsTakeRemoteAll.addEventListener("click", () => {
    void resolveAll("remote");
  });
  el.conflictList.addEventListener("click", (event) => {
    const node = event.target as HTMLElement;
    const action = node.closest<HTMLElement>("[data-choice]");
    const actionPath = action?.dataset.path;
    const actionChoice = action?.dataset.choice;
    if (actionPath && actionChoice) {
      void resolveFromList(actionPath, actionChoice as ConflictChoice);
      return;
    }
    const target = node.closest<HTMLElement>("[data-path]");
    const path = target?.dataset.path;
    if (!path) {
      return;
    }
    const entry = conflicts.find((item) => item.path === path);
    if (entry) {
      renderDiff(entry);
    }
  });
  el.conflictList.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" && event.key !== " ") {
      return;
    }
    if ((event.target as HTMLElement).closest("button")) {
      return;
    }
    const item = (event.target as HTMLElement).closest<HTMLElement>(".conflict-item");
    const path = item?.dataset.path;
    if (!path) {
      return;
    }
    event.preventDefault();
    const entry = conflicts.find((itemEntry) => itemEntry.path === path);
    if (entry) {
      renderDiff(entry);
    }
  });
  el.conflictLocal.addEventListener("click", () => {
    const path = activeConflict;
    if (path) {
      void resolveOne(path, "local").then(loadConflicts);
    }
  });
  el.conflictCopy.addEventListener("click", () => {
    const path = activeConflict;
    if (!path) {
      return;
    }
    const entry = conflicts.find((item) => item.path === path);
    if (!entry) {
      return;
    }
    void confirmResolve(entry, "conflict").then((confirmed) => {
      if (confirmed) {
        void resolveOne(path, "conflict").then(loadConflicts);
      }
    });
  });
  el.conflictRemote.addEventListener("click", () => {
    const path = activeConflict;
    if (!path) {
      return;
    }
    const entry = conflicts.find((item) => item.path === path);
    if (!entry) {
      return;
    }
    void confirmResolve(entry, "remote").then((confirmed) => {
      if (confirmed) {
        void resolveOne(path, "remote").then(loadConflicts);
      }
    });
  });

  let syncingScroll = false;
  const syncScroll = (source: HTMLDivElement, follower: HTMLDivElement) => {
    source.addEventListener("scroll", () => {
      if (syncingScroll) {
        return;
      }
      syncingScroll = true;
      follower.scrollTop = source.scrollTop;
      syncingScroll = false;
    });
  };
  syncScroll(el.diffLeft, el.diffRight);
  syncScroll(el.diffRight, el.diffLeft);

  el.devicesRefresh.addEventListener("click", () => {
    void loadDevices();
  });
  el.deviceBody.addEventListener("click", (event) => {
    const target = (event.target as HTMLElement).closest<HTMLElement>("[data-revoke]");
    const id = target?.dataset.revoke;
    if (id) {
      void revokeDevice(id);
    }
  });

  el.saveSettings.addEventListener("click", () => {
    void saveSettings().catch((error: unknown) => {
      toast(t("conn.saveFail", { error: errorText(error) }));
    });
  });
  // 「重新生成」只填一个新 ID，保存时仍走「迁移 / 仅本地 / 取消」三选一。
  el.regenDevice.addEventListener("click", () => {
    el.deviceId.value = randomDeviceId();
    toast(t("settings.regenFilled"));
  });

  el.dedupeScan.addEventListener("click", () => {
    void loadDedupe();
  });
  el.dedupeApply.addEventListener("click", () => {
    void applyDedupe();
  });
  el.dedupeSections.addEventListener("change", (event) => {
    if ((event.target as HTMLElement).matches("input[data-dedupe-kind]")) {
      updateDedupeApply();
    }
  });
  el.deviceMigrate.addEventListener("click", () => {
    settleDeviceChoice("migrate");
  });
  el.deviceLocal.addEventListener("click", () => {
    settleDeviceChoice("local");
  });
  el.deviceCancel.addEventListener("click", () => {
    settleDeviceChoice("cancel");
  });
  el.deviceModal.addEventListener("click", (event) => {
    if (event.target === el.deviceModal) {
      settleDeviceChoice("cancel");
    }
  });

  el.backupCreate.addEventListener("click", () => {
    void createBackup();
  });
  el.backupPrune.addEventListener("click", () => {
    void pruneBackups();
  });
  el.backupKeep.addEventListener("change", () => {
    beginSaveBackupKeep();
  });
  el.backupList.addEventListener("click", (event) => {
    const restore = (event.target as HTMLElement).closest<HTMLElement>("[data-restore]");
    if (restore?.dataset.restore) {
      void restoreBackup(restore.dataset.restore);
      return;
    }
    const remove = (event.target as HTMLElement).closest<HTMLElement>("[data-delete]");
    if (remove?.dataset.delete) {
      void deleteBackup(remove.dataset.delete);
    }
  });

  el.diag.addEventListener("click", () => {
    void exportDiagnostics();
  });
  el.diagCopy.addEventListener("click", () => {
    void copyDiagPath();
  });

  el.language.addEventListener("change", () => {
    setLang(el.language.value === "en" ? "en" : "zh");
    applyI18n();
    refreshLanguageDependent();
  });

  el.updateCheck.addEventListener("click", () => {
    void checkUpdate();
  });
  el.updateInstall.addEventListener("click", () => {
    void installUpdate();
  });

  el.modalCancel.addEventListener("click", () => settleModal(false));
  el.modalConfirm.addEventListener("click", () => settleModal(true));
  el.modal.addEventListener("click", (event) => {
    if (event.target === el.modal) {
      settleModal(false);
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") {
      return;
    }
    if (!el.deviceModal.hidden) {
      settleDeviceChoice("cancel");
    }
    if (!el.modal.hidden) {
      settleModal(false);
    }
  });

  el.wizardDetect.addEventListener("click", () => {
    void autoDetectWizard();
  });
  el.wizardPreset.addEventListener("change", () => {
    void applyPresetTo(el.wizardPreset.value, el.wizardUrl, el.wizardPresetHint, el.wizardPreset);
  });
  el.wizardTest.addEventListener("click", () => {
    void wizardTest();
  });
  el.wizardBack.addEventListener("click", wizardBack);
  el.wizardNext.addEventListener("click", () => {
    void wizardNext();
  });
  el.wizardSkip.addEventListener("click", () => {
    setWizardStep(4);
  });
  el.wizardStrategy.addEventListener("change", () => {
    const checked = el.wizardStrategy.querySelector<HTMLInputElement>(
      'input[name="wizard-strategy"]:checked',
    );
    setWizardPolicy(checked?.value === "normal" ? "normal" : "preview");
  });

  window.setInterval(() => {
    if (!config || busy) {
      return;
    }
    const minutes = config.transport_interval_minutes;
    if (minutes <= 0) {
      return;
    }
    if (Date.now() - lastRun < minutes * 60_000) {
      return;
    }
    void run("transport", true);
  }, 20_000);
}

export async function subscribe(): Promise<void> {
  await listen<Progress>("sync://progress", (event) => {
    renderProgress(event.payload);
  });
  await listen<Report>("sync://done", (event) => {
    setLastReport(event.payload);
    renderReport(event.payload);
  });
  await listen<WatchEvent>("sync://watch", (event) => {
    applyWatchEvent(event.payload);
  });
}

export async function init(): Promise<void> {
  bindEvents();
  el.language.value = getLang();
  applyI18n();
  document.title = t("app.name");
  el.progressLabel.textContent = t("sync.progressWaiting");
  el.rimeDirSource.textContent = t("sync.notDetected");
  renderWatch();
  el.diagCopy.textContent = t("settings.diagCopy");
  el.togglePass.textContent = t("conn.show");
  renderUpdate();

  void getVersion().then((version) => {
    applyAppVersion(version);
  });

  try {
    // --setup：安装完成后由安装器带参数启动，无论是否首次运行都强制进设置向导。
    const forcedSetup = await api.takeSetupRequest();
    const needsSetup = forcedSetup || (await api.needsSetup());
    const loaded = await api.getConfig();
    renderConfig(loaded);
    el.rimeDirSource.textContent = loaded.rime_dir ? t("sync.saved") : t("sync.notDetected");
    // 启动时校验：配置里的目录被删掉后（后端已重置为 null），自动重新探测一次。
    if (!loaded.rime_dir) {
      void detectRimeDir(el.rimeDir, el.rimeDirSource, true);
    }
    await loadPresets();
    await subscribe();
    await loadLog();
    await refreshSecretStatus();
    await refreshNetwork();
    await loadWatchStatus();
    await loadBackups();
    localLog("界面已就绪");
    setFooter(t("footer.ready"));
    if (needsSetup) {
      openWizard(forcedSetup ? "settings" : "first-run");
    }
  } catch (error) {
    const text = errorText(error);
    setStatus(el.connStatus, "err", t("footer.backendDown") + ": " + text);
    localLog("初始化失败：" + text);
    setFooter(t("footer.backendDown"));
  }
  touchLastRun();
}

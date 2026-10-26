

// DOM 基础：元素缓存、状态条与页脚写入。

export function byId<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) {
    throw new Error("缺少界面元素 #" + id);
  }
  return node as T;
}

export const el = {
  tabs: Array.from(document.querySelectorAll<HTMLButtonElement>(".tab")),
  panels: Array.from(document.querySelectorAll<HTMLElement>(".panel")),
  busy: byId<HTMLDivElement>("busy"),
  busyText: byId<HTMLSpanElement>("busy-text"),

  preset: byId<HTMLSelectElement>("preset"),
  presetHint: byId<HTMLParagraphElement>("preset-hint"),
  url: byId<HTMLInputElement>("dav-url"),
  user: byId<HTMLInputElement>("dav-user"),
  pass: byId<HTMLInputElement>("dav-pass"),
  dir: byId<HTMLInputElement>("dav-dir"),
  togglePass: byId<HTMLButtonElement>("toggle-pass"),
  saveConn: byId<HTMLButtonElement>("btn-save-conn"),
  test: byId<HTMLButtonElement>("btn-test"),
  probe: byId<HTMLButtonElement>("btn-probe"),
  listRemote: byId<HTMLButtonElement>("btn-list-remote"),
  connStatus: byId<HTMLDivElement>("conn-status"),
  networkStatus: byId<HTMLParagraphElement>("network-status"),
  remoteList: byId<HTMLDivElement>("remote-list"),
  secretStatus: byId<HTMLSpanElement>("secret-status"),

  rimeDir: byId<HTMLInputElement>("rime-dir"),
  rimeDirSource: byId<HTMLParagraphElement>("rime-dir-source"),
  detect: byId<HTMLButtonElement>("btn-detect"),
  browseRime: byId<HTMLButtonElement>("btn-browse-rime"),
  wizardBrowse: byId<HTMLButtonElement>("wizard-browse"),
  watchToggle: byId<HTMLInputElement>("watch-toggle"),
  watchStatus: byId<HTMLSpanElement>("watch-status"),
  watchLast: byId<HTMLSpanElement>("watch-last"),
  transport: byId<HTMLButtonElement>("btn-transport"),
  apply: byId<HTMLButtonElement>("btn-apply"),
  cancel: byId<HTMLButtonElement>("btn-cancel"),
  plan: byId<HTMLButtonElement>("btn-plan"),
  planCard: byId<HTMLDivElement>("plan-card"),
  planSummary: byId<HTMLParagraphElement>("plan-summary"),
  planNote: byId<HTMLParagraphElement>("plan-note"),
  planGroups: byId<HTMLDivElement>("plan-groups"),
  planRun: byId<HTMLButtonElement>("btn-plan-run"),
  planClose: byId<HTMLButtonElement>("btn-plan-close"),

  progress: byId<HTMLDivElement>("progress"),
  progressFill: byId<HTMLDivElement>("progress-fill"),
  progressLabel: byId<HTMLSpanElement>("progress-label"),
  progressCount: byId<HTMLSpanElement>("progress-count"),

  reportPulled: byId<HTMLSpanElement>("report-pulled"),
  reportPushed: byId<HTMLSpanElement>("report-pushed"),
  reportMerged: byId<HTMLSpanElement>("report-merged"),
  reportConflicts: byId<HTMLSpanElement>("report-conflicts"),
  reportDuration: byId<HTMLSpanElement>("report-duration"),
  reportBackup: byId<HTMLSpanElement>("report-backup"),
  reportMode: byId<HTMLDivElement>("report-mode"),
  reportNote: byId<HTMLDivElement>("report-note"),

  firstSyncBanner: byId<HTMLDivElement>("first-sync-banner"),
  firstSyncError: byId<HTMLParagraphElement>("first-sync-error"),
  firstSyncSwitch: byId<HTMLButtonElement>("btn-first-sync-normal"),

  log: byId<HTMLPreElement>("log"),
  refreshLog: byId<HTMLButtonElement>("btn-refresh-log"),
  clearLog: byId<HTMLButtonElement>("btn-clear-log"),

  conflictsCount: byId<HTMLSpanElement>("conflicts-count"),
  conflictList: byId<HTMLDivElement>("conflict-list"),
  conflictEmpty: byId<HTMLParagraphElement>("conflict-empty"),
  conflictBody: byId<HTMLDivElement>("conflict-body"),
  conflictMeta: byId<HTMLDivElement>("conflict-meta"),
  diffLeft: byId<HTMLDivElement>("diff-left"),
  diffRight: byId<HTMLDivElement>("diff-right"),
  conflictsRefresh: byId<HTMLButtonElement>("btn-conflicts-refresh"),
  conflictsKeepAll: byId<HTMLButtonElement>("btn-conflicts-keep-all"),
  conflictsTakeAll: byId<HTMLButtonElement>("btn-conflicts-take-all"),
  conflictsTakeRemoteAll: byId<HTMLButtonElement>("btn-conflicts-remote-all"),
  conflictLocal: byId<HTMLButtonElement>("btn-conflict-local"),
  conflictCopy: byId<HTMLButtonElement>("btn-conflict-copy"),
  conflictRemote: byId<HTMLButtonElement>("btn-conflict-remote"),

  devicesRefresh: byId<HTMLButtonElement>("btn-devices-refresh"),
  deviceBody: byId<HTMLTableSectionElement>("device-body"),
  deviceEmpty: byId<HTMLParagraphElement>("device-empty"),

  interval: byId<HTMLInputElement>("interval"),
  deviceId: byId<HTMLInputElement>("device-id"),
  deviceIdentityHint: byId<HTMLParagraphElement>("device-identity-hint"),
  regenDevice: byId<HTMLButtonElement>("btn-regen-device"),
  deviceModal: byId<HTMLDivElement>("device-modal"),
  deviceModalBody: byId<HTMLParagraphElement>("device-modal-body"),
  deviceMigrate: byId<HTMLButtonElement>("device-migrate"),
  deviceLocal: byId<HTMLButtonElement>("device-local"),
  deviceCancel: byId<HTMLButtonElement>("device-cancel"),
  migrateModal: byId<HTMLDivElement>("migrate-modal"),
  migrateModalBody: byId<HTMLParagraphElement>("migrate-modal-body"),
  migrateModalTarget: byId<HTMLParagraphElement>("migrate-modal-target"),
  migrateMerge: byId<HTMLButtonElement>("migrate-merge"),
  migrateReplace: byId<HTMLButtonElement>("migrate-replace"),
  migrateCancel: byId<HTMLButtonElement>("migrate-cancel"),
  kindUserdict: byId<HTMLInputElement>("kind-userdict"),
  kindYaml: byId<HTMLInputElement>("kind-yaml"),
  kindDict: byId<HTMLInputElement>("kind-dict"),
  excludes: byId<HTMLTextAreaElement>("excludes"),
  stopFcitx5: byId<HTMLInputElement>("stop-fcitx5"),
  schemaList: byId<HTMLDivElement>("schema-list"),
  schemaEmpty: byId<HTMLParagraphElement>("schema-empty"),
  schemaStatus: byId<HTMLParagraphElement>("schema-status"),
  schemaRefresh: byId<HTMLButtonElement>("btn-schema-refresh"),
  schemaSave: byId<HTMLButtonElement>("btn-schema-save"),
  deviceLocalList: byId<HTMLDivElement>("device-local-list"),
  deviceLocalEmpty: byId<HTMLParagraphElement>("device-local-empty"),
  deviceLocalInput: byId<HTMLInputElement>("device-local-input"),
  deviceLocalAdd: byId<HTMLButtonElement>("btn-device-local-add"),
  deviceLocalStatus: byId<HTMLParagraphElement>("device-local-status"),
  saveSettings: byId<HTMLButtonElement>("btn-save-settings"),
  settingsHint: byId<HTMLSpanElement>("settings-hint"),

  backupList: byId<HTMLDivElement>("backup-list"),
  backupCreate: byId<HTMLButtonElement>("btn-backup-create"),
  backupPrune: byId<HTMLButtonElement>("btn-backup-prune"),
  backupKeep: byId<HTMLSelectElement>("backup-keep"),

  dedupeSections: byId<HTMLDivElement>("dedupe-sections"),
  dedupeStatus: byId<HTMLParagraphElement>("dedupe-status"),
  dedupeScan: byId<HTMLButtonElement>("btn-dedupe-scan"),
  dedupeApply: byId<HTMLButtonElement>("btn-dedupe-apply"),

  diag: byId<HTMLButtonElement>("btn-diag"),
  diagResult: byId<HTMLDivElement>("diag-result"),
  diagPath: byId<HTMLElement>("diag-path"),
  diagCopy: byId<HTMLButtonElement>("btn-diag-copy"),

  installWarning: byId<HTMLDivElement>("install-warning"),
  installWarningDetail: byId<HTMLParagraphElement>("install-warning-detail"),
  installWarningError: byId<HTMLParagraphElement>("install-warning-error"),
  installWarningLog: byId<HTMLButtonElement>("btn-install-warning-log"),
  installWarningRetry: byId<HTMLButtonElement>("btn-install-warning-retry"),

  language: byId<HTMLSelectElement>("language"),
  updateStatus: byId<HTMLElement>("update-status"),
  updateVersion: byId<HTMLParagraphElement>("update-version"),
  updatePanel: byId<HTMLDivElement>("update-panel"),
  updateNotes: byId<HTMLParagraphElement>("update-notes"),
  updateCheck: byId<HTMLButtonElement>("btn-update-check"),
  updateInstall: byId<HTMLButtonElement>("btn-update-install"),

  footerStatus: byId<HTMLSpanElement>("footer-status"),
  footerConfig: byId<HTMLSpanElement>("footer-config"),
  toast: byId<HTMLDivElement>("toast"),

  modal: byId<HTMLDivElement>("modal"),
  modalTitle: byId<HTMLHeadingElement>("modal-title"),
  modalBody: byId<HTMLParagraphElement>("modal-body"),
  modalCancel: byId<HTMLButtonElement>("modal-cancel"),
  modalConfirm: byId<HTMLButtonElement>("modal-confirm"),

  wizard: byId<HTMLDivElement>("wizard"),
  wizardStepLabel: byId<HTMLSpanElement>("wizard-step-label"),
  wizardBar: byId<HTMLDivElement>("wizard-bar"),
  wizardSteps: Array.from(document.querySelectorAll<HTMLElement>(".wizard-step")),
  wizardBack: byId<HTMLButtonElement>("wizard-back"),
  wizardSkip: byId<HTMLButtonElement>("wizard-skip"),
  wizardNext: byId<HTMLButtonElement>("wizard-next"),
  wizardRime: byId<HTMLInputElement>("wizard-rime"),
  wizardDetect: byId<HTMLButtonElement>("wizard-detect"),
  wizardRimeStatus: byId<HTMLParagraphElement>("wizard-rime-status"),
  wizardPreset: byId<HTMLSelectElement>("wizard-preset"),
  wizardPresetHint: byId<HTMLParagraphElement>("wizard-preset-hint"),
  wizardTitle: byId<HTMLHeadingElement>("wizard-title"),
  wizardDevice: byId<HTMLInputElement>("wizard-device"),
  wizardDetails: byId<HTMLDivElement>("wizard-details"),
  wizardSummary: byId<HTMLDivElement>("wizard-summary"),
  wizardPolicyHint: byId<HTMLParagraphElement>("wizard-policy-hint"),
  wizardRunFirst: byId<HTMLInputElement>("wizard-run-first"),
  wizardUrl: byId<HTMLInputElement>("wizard-url"),
  wizardUser: byId<HTMLInputElement>("wizard-user"),
  wizardPass: byId<HTMLInputElement>("wizard-pass"),
  wizardDir: byId<HTMLInputElement>("wizard-dir"),
  wizardTest: byId<HTMLButtonElement>("wizard-test"),
  wizardStatus: byId<HTMLDivElement>("wizard-status"),
  wizardStrategy: byId<HTMLDivElement>("wizard-strategy"),
  wizardResult: byId<HTMLDivElement>("wizard-result"),
};

export function setFooter(text: string): void {
  el.footerStatus.textContent = text;
}

export function setStatus(node: HTMLDivElement, kind: "ok" | "err" | "idle", text: string): void {
  node.hidden = false;
  node.className = "status" + (kind === "idle" ? "" : " " + kind);
  node.textContent = text;
}

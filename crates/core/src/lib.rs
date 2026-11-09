//! rime-sync-core: Rime WebDAV 同步引擎（无 GUI 依赖）。

// 非测试代码一律不允许 unwrap/expect：集成测试是独立 crate，不受此约束；
// src 内的单测模块只在 cfg(test) 下编译，同样不受影响。
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod backup;
pub mod cancel;
pub mod config;
pub mod data_patch;
pub mod dedupe;
pub mod deploy;
pub mod engine;
pub mod history;
pub mod ime;
pub mod ime_state;
pub mod index;
pub mod librime;
pub mod linux_deploy;
pub mod lock;
pub mod log;
pub mod managed_custom;
pub mod merge3;
pub mod network;
pub mod plan;
pub mod platform;
pub mod prebuilt;
pub mod presets;
pub mod process;
pub mod profile;
pub mod proxy;
pub mod prune;
pub mod retry;
pub mod rime_dir;
pub mod secret;
pub mod userdb;
pub mod watch;
pub mod webdav;
pub mod yaml_text;

pub use backup::{BackupInfo, BackupManager, BackupManifest, RestoreReport};
pub use cancel::CancelToken;
pub use config::{Config, FirstSyncPolicy, MigrationReport, PasswordRef, CONFIG_SCHEMA_VERSION};
pub use data_patch::{DataDirCandidate, DataPatchReport, PatchOutcome};
pub use engine::{
    ConflictDetail, Engine, PlanAction, PlanCounts, PlanKind, PlanReport, Progress, ProgressSink,
    Report,
};
pub use history::{HistoryEntry, HISTORY_LIMIT};
pub use ime::{ImeReport, WeaselHealth};
pub use ime_state::{
    assess, assess_numpad, ensure, Action, Assessment, KeepSet, Mode, NumpadStatus,
    Options as EnsureOptions, Outcome, OutcomeResult, Reason, SchemaState,
    State as SchemaFileState,
};
pub use lock::{acquire as acquire_lock, inspect as inspect_lock, LockGuard, LockInfo, LockStatus};
pub use log::{LogWriter, RotatingLog, DEFAULT_KEEP, DEFAULT_MAX_BYTES};
pub use network::{NetworkPolicy, NetworkProbe, NetworkState};
pub use platform::RimeTrigger;
pub use prebuilt::{
    install_prebuilt, install_prebuilt_with, probe_build, resolve_payload, BuildProbe,
    InstallOptions, PayloadLocation, PrebuiltReport, ENV_MODE, ENV_PAYLOAD_DIR, MARKERS,
    PROBE_FILE,
};
pub use presets::Preset;
pub use proxy::{ProxyConfig, ProxyKind, ProxySpec, PROXY_PASSWORD_ACCOUNT};
pub use prune::{PruneOptions, PruneReport};
pub use retry::RetryState;
pub use secret::{detect as detect_secret_store, SecretBackend, SecretSelection, SecretStore};
pub use userdb::{diff_stats as userdb_diff_stats, UserdbStats};
pub use watch::{WatchEvent, WatchHandle};

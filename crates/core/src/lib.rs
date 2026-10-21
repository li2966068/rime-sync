//! rime-sync-core: Rime WebDAV 同步引擎（无 GUI 依赖）。

pub mod backup;
pub mod cancel;
pub mod config;
pub mod engine;
pub mod history;
pub mod index;
pub mod lock;
pub mod log;
pub mod merge3;
pub mod network;
pub mod plan;
pub mod platform;
pub mod presets;
pub mod process;
pub mod retry;
pub mod rime_dir;
pub mod secret;
pub mod userdb;
pub mod watch;
pub mod webdav;

pub use backup::{BackupInfo, BackupManager, BackupManifest, RestoreReport};
pub use cancel::CancelToken;
pub use config::{Config, FirstSyncPolicy, MigrationReport, PasswordRef, CONFIG_SCHEMA_VERSION};
pub use engine::{ConflictDetail, Engine, Progress, ProgressSink, Report};
pub use history::{HistoryEntry, HISTORY_LIMIT};
pub use lock::{acquire as acquire_lock, inspect as inspect_lock, LockGuard, LockInfo, LockStatus};
pub use log::{LogWriter, RotatingLog, DEFAULT_KEEP, DEFAULT_MAX_BYTES};
pub use network::{NetworkPolicy, NetworkProbe, NetworkState};
pub use platform::RimeTrigger;
pub use presets::Preset;
pub use retry::RetryState;
pub use secret::{detect as detect_secret_store, SecretBackend, SecretSelection, SecretStore};
pub use userdb::{diff_stats as userdb_diff_stats, UserdbStats};
pub use watch::{WatchEvent, WatchHandle};

//! rime-sync-core: Rime WebDAV 同步引擎（无 GUI 依赖）。

pub mod backup;
pub mod cancel;
pub mod config;
pub mod engine;
pub mod index;
pub mod lock;
pub mod merge3;
pub mod network;
pub mod plan;
pub mod platform;
pub mod presets;
pub mod retry;
pub mod rime_dir;
pub mod secret;
pub mod watch;
pub mod webdav;

pub use backup::{BackupInfo, BackupManager, BackupManifest, RestoreReport};
pub use cancel::CancelToken;
pub use config::{Config, PasswordRef};
pub use engine::{Engine, Progress, ProgressSink, Report};
pub use lock::{acquire as acquire_lock, inspect as inspect_lock, LockGuard, LockInfo, LockStatus};
pub use network::{NetworkPolicy, NetworkProbe, NetworkState};
pub use platform::RimeTrigger;
pub use presets::Preset;
pub use retry::RetryState;
pub use secret::{detect as detect_secret_store, SecretBackend, SecretSelection, SecretStore};
pub use watch::{WatchEvent, WatchHandle};

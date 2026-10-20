//! rime-sync-core: Rime WebDAV 同步引擎（无 GUI 依赖）。

pub mod config;
pub mod engine;
pub mod index;
pub mod merge3;
pub mod plan;
pub mod platform;
pub mod rime_dir;
pub mod webdav;

pub use engine::{Engine, Progress, ProgressSink, Report};
pub use platform::RimeTrigger;

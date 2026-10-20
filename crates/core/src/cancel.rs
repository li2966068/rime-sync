//! 取消信号：GUI 端可在长时间同步中途请求停止。
//!
//! 引擎在每个条目/目录边界检查一次，不做线程抢占：已开始的单个 HTTP 请求会走完。

use anyhow::{bail, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// 已请求取消时返回 Err，调用方直接用 `?` 中断当前流程。
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            bail!("cancelled by user");
        }
        Ok(())
    }
}

//! 把发布版本注入 core（0.6.22）。
//!
//! 数据补丁的注释行原来用 `env!("CARGO_PKG_VERSION")`，workspace 版本是 0.1.0，
//! 于是 CLI 写 `# patched by rime-sync 0.1.0`，GUI 写 tauri 的 0.6.x。
//! 这里统一来源，优先级从高到低：
//! 1. `RIMESYNC_VERSION`（构建期）；
//! 2. `src-tauri/tauri.conf.json` 的 version；
//! 3. `CHANGELOG.md` 顶部版本；
//! 4. `CARGO_PKG_VERSION`。
//!
//! 运行期还可用 `RIMESYNC_VERSION` 覆盖（见 `data_patch::resolve_version`）。

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=RIMESYNC_VERSION");
    let manifest = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let repo = manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf);

    let mut version = std::env::var("RIMESYNC_VERSION")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    if version.is_none() {
        if let Some(repo) = &repo {
            let conf = repo.join("src-tauri").join("tauri.conf.json");
            println!("cargo:rerun-if-changed={}", conf.display());
            version = tauri_version(&conf);
            if version.is_none() {
                let changelog = repo.join("CHANGELOG.md");
                println!("cargo:rerun-if-changed={}", changelog.display());
                version = std::fs::read_to_string(&changelog)
                    .ok()
                    .and_then(|text| changelog_version(&text));
            }
        }
    }

    let version = version.unwrap_or_else(|| {
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string())
    });
    println!("cargo:rustc-env=RIMESYNC_BUILD_VERSION={version}");
}

fn tauri_version(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("version")
        .and_then(|version| version.as_str())
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty())
}

/// CHANGELOG 顶部形如 `## v0.6.21 — 2026-11-08` 或 `## [0.6.21]`。
fn changelog_version(text: &str) -> Option<String> {
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("## ") else {
            continue;
        };
        let Some(token) = rest.split_whitespace().next() else {
            continue;
        };
        let token = token
            .trim_start_matches('v')
            .trim_matches(|ch| ch == '[' || ch == ']');
        if token.contains('.') && token.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
            return Some(token.to_string());
        }
    }
    None
}

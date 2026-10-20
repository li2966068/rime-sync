//! Rime 用户目录探测与 installation.yaml 解析。

use anyhow::{bail, Context, Result};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Detection {
    pub path: PathBuf,
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct Installation {
    pub installation_id: Option<String>,
    pub sync_dir: Option<PathBuf>,
}

/// explicit 来自 --rime-dir 或 config.rime_dir；其次环境变量；最后已知平台路径。
pub fn detect(explicit: Option<&Path>) -> Result<Detection> {
    if let Some(path) = explicit {
        if !path.is_dir() {
            bail!(
                "rime dir does not exist or is not a directory: {}",
                path.display()
            );
        }
        return Ok(Detection {
            path: path.to_path_buf(),
            source: "argument".to_string(),
        });
    }
    if let Some(raw) = env::var_os("RIME_USER_DATA_DIR") {
        let path = PathBuf::from(raw);
        if path.is_dir() {
            return Ok(Detection {
                path,
                source: "env:RIME_USER_DATA_DIR".to_string(),
            });
        }
    }
    for candidate in candidates() {
        if candidate.is_dir() {
            let source = format!("known:{}", candidate.display());
            return Ok(Detection {
                path: candidate,
                source,
            });
        }
    }
    bail!("no Rime user directory found; pass --rime-dir or set RIME_USER_DATA_DIR")
}

pub fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(home) = home_dir() else {
        return out;
    };
    #[cfg(target_os = "linux")]
    {
        out.push(home.join(".local/share/fcitx5/rime"));
        out.push(home.join(".config/fcitx/rime"));
        out.push(home.join(".config/ibus/rime"));
        out.push(home.join(".local/share/ibus/rime"));
    }
    #[cfg(target_os = "macos")]
    {
        out.push(home.join("Library/Rime"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = env::var_os("APPDATA") {
            out.push(PathBuf::from(appdata).join("Rime"));
        }
        out.push(home.join("AppData/Roaming/Rime"));
    }
    out
}

pub fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub fn installation(rime_dir: &Path) -> Installation {
    let Ok(text) = fs::read_to_string(rime_dir.join("installation.yaml")) else {
        return Installation::default();
    };
    parse_installation(&text)
}

pub fn parse_installation(text: &str) -> Installation {
    let mut out = Installation::default();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "installation_id" => out.installation_id = Some(value.to_string()),
            "sync_dir" => out.sync_dir = Some(PathBuf::from(value)),
            _ => {}
        }
    }
    out
}

pub fn installation_id(rime_dir: &Path) -> Option<String> {
    installation(rime_dir).installation_id
}

/// Rime 的 sync_dir 可以是绝对路径，也可以是相对 rime 目录的路径。
pub fn sync_dir(rime_dir: &Path) -> PathBuf {
    match installation(rime_dir).sync_dir {
        Some(path) if path.is_absolute() => path,
        Some(path) => rime_dir.join(path),
        None => rime_dir.join("sync"),
    }
}

pub fn ensure_sync_dir(rime_dir: &Path) -> Result<PathBuf> {
    let dir = sync_dir(rime_dir);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_installation_yaml() {
        let text = "# comment\ndistribution_code_name: fcitx-rime\ninstallation_id: devA\nsync_dir: \"my-sync\"\n";
        let parsed = parse_installation(text);
        assert_eq!(parsed.installation_id.as_deref(), Some("devA"));
        assert_eq!(parsed.sync_dir, Some(PathBuf::from("my-sync")));
    }

    #[test]
    fn explicit_detection_wins() {
        let dir = std::env::temp_dir().join(format!("rime-sync-det-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let det = detect(Some(&dir)).expect("detect");
        assert_eq!(det.source, "argument");
        assert_eq!(det.path, dir);
        fs::remove_dir_all(&det.path).ok();
    }

    #[test]
    fn missing_explicit_dir_is_an_error() {
        let dir = std::env::temp_dir().join("rime-sync-definitely-missing-dir");
        assert!(detect(Some(&dir)).is_err());
    }
}

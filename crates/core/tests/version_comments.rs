//! 源码注释的版本号不得跑到发布版本前面（「注释漂移」回归测试）。
//!
//! 起因：0.6.23 的开发期里，不少注释写成了下一个补丁版本——那一版还没发布，注释
//! 先在文档里「宣布」了一个不存在的版本，排障时按注释找版本对不上。
//!
//! 规则（0.6.23 起有约束力）：只比同一版本序列（0.6.x）——注释里出现的 0.6.x 版本
//! 必须 **≤ 当前发布版本**。当前发布版本取自 build.rs 注入的 RIMESYNC_BUILD_VERSION
//! （src-tauri/tauri.conf.json）。0.6.23 之前的旧规则是「上限 = 当前版本的补丁号 +1」，
//! 而全部注释本来就在当前版本上，于是那条断言恒真、等于没有约束；现在收紧到发布
//! 版本本身：写成下一个未发布的补丁版本就会红。
//!
//! 只扫注释，不扫字符串与文档正文（CHANGELOG 本来就是历史版本清单）。注释里提到
//! Weasel 0.17.4 这类别的产品的版本不算漂移——它们不在 0.6 序列里。
//!
//! 豁免：少数文件里的历史注释无法归类到 0.6 序列（例如 0.6.0 之前的旧记录），
//! 或者是有意保留的对照表。这类文件走下面的**文件级白名单**，每条都必须写清理由；
//! 新增豁免要在评审里说明，不允许为了让它变绿而豁免。

use std::path::{Path, PathBuf};

/// 只扫这些目录（相对仓库根）。
const ROOTS: [&str; 3] = ["crates", "src-tauri/src", "src"];
/// 这些后缀才可能是源码（docs/ 与 CHANGELOG 不参与）。
const EXTENSIONS: [&str; 2] = ["rs", "ts"];

type Version = (u64, u64, u64);

/// 文件级豁免：路径相对仓库根，理由必须写清（为什么这些注释不参与版本判定）。
struct Exemption {
    path: &'static str,
    reason: &'static str,
}

/// 当前没有需要豁免的文件：本仓库的注释全部在 0.6 序列内，且都不超过发布版本。
///
/// 留空是有意的——它为「确实无法归类」的历史注释保留了出口，同时任何新增条目都要
/// 在这里落一条理由，评审时一眼可见。空表由 exemption_list_points_at_real_files 兜底
/// （写错路径会立刻红）。
const EXEMPT_FILES: &[Exemption] = &[];

fn exemption_for(relative: &str) -> Option<&'static Exemption> {
    EXEMPT_FILES.iter().find(|entry| entry.path == relative)
}

fn parse_version(token: &str) -> Option<Version> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut values = [0u64; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        values[index] = part.parse().ok()?;
    }
    Some((values[0], values[1], values[2]))
}

/// 一段文本里所有形如 x.y.z 的版本号。
fn versions_in(text: &str) -> Vec<Version> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        if let Some(version) = parse_version(&text[start..index]) {
            out.push(version);
        }
    }
    out
}

/// 一行里的注释部分（Rust 与 TS 都用 //；块注释只认以 * 或 /* 开头的行）。
fn comment_of(line: &str) -> Option<&str> {
    if let Some(index) = line.find("//") {
        return Some(&line[index..]);
    }
    let trimmed = line.trim_start();
    if trimmed.starts_with("/*") || trimmed.starts_with('*') {
        return Some(trimmed);
    }
    None
}

/// 当前发布版本（同一条 x.y 版本序列）。
fn current_version() -> Version {
    parse_version(rime_sync_core::data_patch::BUILD_VERSION).unwrap_or_else(|| {
        panic!(
            "构建版本不是 x.y.z：{}",
            rime_sync_core::data_patch::BUILD_VERSION
        )
    })
}

/// 该版本号是否在同一序列（major.minor 相同）里跑到了发布版本前面。
///
/// 只比 0.6 序列：注释里提到 Weasel 0.17.4 不属于漂移。
fn runs_ahead(version: Version, current: Version) -> bool {
    version.0 == current.0 && version.1 == current.1 && version > current
}

/// 扫描一段源码文本，返回「超过发布版本」的注释行（file:line 形式）。
fn offenders_in(label: &str, text: &str, current: Version) -> Vec<String> {
    let mut offenders = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let Some(comment) = comment_of(line) else {
            continue;
        };
        for version in versions_in(comment) {
            if runs_ahead(version, current) {
                offenders.push(format!(
                    "{label}:{}: {}.{}.{}（允许到 {}.{}.{}）",
                    number + 1,
                    version.0,
                    version.1,
                    version.2,
                    current.0,
                    current.1,
                    current.2
                ));
            }
        }
    }
    offenders
}

fn source_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, out);
            continue;
        }
        let allowed = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| EXTENSIONS.contains(&ext))
            .unwrap_or(false);
        if allowed {
            out.push(path);
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/core 的上级两级就是仓库根")
        .to_path_buf()
}

/// 仓库相对的路径（用来匹配白名单）。
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn source_comments_do_not_run_ahead_of_the_release() {
    let root = repo_root();
    let current = current_version();
    let mut files = Vec::new();
    for dir in ROOTS {
        source_files(&root.join(dir), &mut files);
    }
    assert!(
        files.len() > 20,
        "只扫到 {} 个源文件，路径可能不对：{}",
        files.len(),
        root.display()
    );
    let mut offenders = Vec::new();
    let mut exempted = 0;
    for file in &files {
        let relative_path = relative(&root, file);
        if let Some(entry) = exemption_for(&relative_path) {
            // 豁免必须写理由：空理由等于没有豁免依据。
            assert!(
                entry.reason.trim().len() >= 8,
                "豁免 {} 没有写清理由",
                entry.path
            );
            exempted += 1;
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        offenders.extend(offenders_in(&relative_path, &text, current));
    }
    assert!(
        offenders.is_empty(),
        "源码注释里的版本号超过了当前发布版本 {}.{}.{}：\n{}\n（要么改成已发布的版本号，要么在 EXEMPT_FILES 里写清豁免理由）",
        current.0,
        current.1,
        current.2,
        offenders.join("\n")
    );
    assert_eq!(
        exempted,
        EXEMPT_FILES.len(),
        "有豁免条目没有命中任何被扫描的文件（路径写错或多扫了目录）"
    );
}

/// 上限就是发布版本本身：写成「下一个补丁版本」必须红，写成当前版本必须绿。
///
/// 版本号在测试里拼出来（不写死下一个版本），否则这段断言自己的注释会被扫成漂移。
#[test]
fn a_comment_one_patch_ahead_is_flagged_and_the_release_version_is_not() {
    let (major, minor, patch) = current_version();
    let ahead = format!("{major}.{minor}.{}", patch + 1);
    let at_release = format!("{major}.{minor}.{patch}");
    let current = current_version();

    let red = offenders_in("synthetic.rs", &format!("let x = 1; // {ahead}"), current);
    assert_eq!(red.len(), 1, "超出一个补丁版本必须红：{red:?}");
    assert!(red[0].starts_with("synthetic.rs:1:"), "{red:?}");

    let green = offenders_in(
        "synthetic.rs",
        &format!("let x = 1; // {at_release}"),
        current,
    );
    assert!(green.is_empty(), "当前发布版本必须绿：{green:?}");

    // 往前的历史版本、以及别的产品的版本号都不算漂移。
    let older = format!("{major}.{}.0", minor.saturating_sub(1));
    assert!(offenders_in("synthetic.rs", &format!("// {older} 的历史"), current).is_empty());
    assert!(offenders_in("synthetic.rs", "// Weasel 0.17.4 的注册表", current).is_empty());
}

#[test]
fn exemption_list_points_at_real_files() {
    let root = repo_root();
    for entry in EXEMPT_FILES {
        assert!(
            root.join(entry.path).is_file(),
            "豁免条目指向的文件不存在：{}",
            entry.path
        );
        assert!(
            !entry.reason.trim().is_empty(),
            "豁免条目缺少理由：{}",
            entry.path
        );
        assert_eq!(
            exemption_for(entry.path).map(|item| item.path),
            Some(entry.path)
        );
    }
}

#[test]
fn version_scanner_reads_the_forms_we_actually_write() {
    assert_eq!(versions_in("// 0.6.23：修好了"), vec![(0, 6, 23)]);
    assert_eq!(
        versions_in("/// 历史：0.6.2 与 0.6.22 都受影响"),
        vec![(0, 6, 2), (0, 6, 22)]
    );
    assert!(versions_in("0.6（只有两段）").is_empty());
    assert_eq!(comment_of("let x = 1; // 0.6.23"), Some("// 0.6.23"));
    assert_eq!(comment_of("let x = 1;"), None);
}

//! 合并词典里第三方词库的「碎音节」清洗。
//!
//! ## 背景（2026-11-18 在 227 上查实）
//!
//! librime 编译词库时，会把词条 code 里出现的**每一个音节**收进该词典的
//! 「音节表」（`third_party/librime/src/rime/dict/entry_collector.cc` 的
//! `EntryCollector::CreateEntry`）。方案里的 `speller/algebra` 又有缩写规则
//! `abbrev/^([a-z]).+$/$1/`：音节 `bo` 会派生出缩写拼写 `b`，用户敲
//! `bhzt` 命中「渤海钻探」（`bo hai zuan tan`）靠的就是这层映射。
//!
//! 搜狗一类的第三方词库里混着大量**英文词**，它们的 code 被逐字母拆开：
//! `web` → `w e b`、`docker` → `d o c k e r`、`linux` → `l i n u x`……
//! 这些**单音节**会：
//!
//! 1. 挤进音节表，把首字母缩写索引顶掉 —— 首字母候选被挤掉，
//!    `bhzt` 查不出「渤海钻探」；
//! 2. 让音节表从 424 涨到 1769，缩写候选的剪枝进一步变差。
//!
//! 实证（227 真机，全量重编译后对比）：
//!
//! | 词典 | `zgr` 候选 | `bhzt` 候选 |
//! |------|-----------|------------|
//! | 官方 `luna_pinyin` | 4318（中国人 第 1） | 2370（渤海钻探 在列） |
//! | 搜狗词库单独 | 0 | 1（只有 `bh`） |
//! | 合并 + 清洗碎音节 | 4318（中国人 第 1） | 2370（渤海钻探 在列） |
//!
//! ## 白名单怎么定的（不硬编码）
//!
//! 合法的单音节只有三个：`a`（啊）、`o`（哦）、`e`（鹅）—— 官方
//! `luna_pinyin.dict.yaml` 的 424 个音节里单音节正好是这三个。
//! 但方言词（`呣 m`、`嗯 n`）在官方 `pinyin_simp.dict.yaml` 里也以单音节出现，
//! 硬编码「非 a/o/e 一律删」会误伤它们。
//!
//! 所以白名单取**同一组词库里其它词库用过的单音节**：
//!
//! * 合并词典的 `import_tables` 是一组词库；
//! * 其中位于**系统 data 目录**的（官方词库）不作清洗，但它们的单音节集合
//!   是白名单的一部分；
//! * 用户目录里的词库按这个白名单清洗：code 里出现白名单外的单音节 ⇒
//!   整条词条剔除。
//!
//! 于是 `luna_pinyin` 在组里 ⇒ 白名单 = {a, o, e}；`pinyin_simp` 也在组里 ⇒
//! 白名单再加上 {m, n}，方言词不会被误删。
//!
//! ## 只动字节，不动语义
//!
//! * 行尾、BOM、注释、header 一律原样保留（按字节切行，命中就整行跳过）；
//! * 只剔除整条词条，其余行一个字节都不改（词库体积、权重、顺序不变）；
//! * 清洗**幂等**：跑第二遍不会再删任何东西。
//!
//! 清洗会改写词库文件 ⇒ 必须让 librime 重编译，否则 build 目录里的
//! `.table.bin` 还是老的（首字母依然失效）。调用方负责触发重部署。

use crate::atomic::write_atomic;
use crate::data_patch::backup_path;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// 汉语拼音里合法的三个单音节（兜底白名单）。
const BASE_SINGLE_SYLLABLES: [&str; 3] = ["a", "o", "e"];

/// 一份词库的位置信息。
#[derive(Debug, Clone)]
pub struct DictRef {
    /// 词库名（`import_tables` 里写的那一项）。
    pub name: String,
    /// 词库文件的实际路径（用户目录里的副本优先）。
    pub path: PathBuf,
    /// 是否是**第三方**词库（用户自己放进去的、系统里没有官方对应版本的）。
    ///
    /// 只有第三方词库需要清洗；官方词库（以及它的用户目录副本）不动，
    /// 但官方词库的单音节集合是清洗的白名单来源。
    pub user_owned: bool,
    /// 官方同名词库的位置（系统 data 目录里），没有则 None。
    ///
    /// 有它 ⇒ 这份词库是官方词库的副本 ⇒ 它的单音节可以进白名单
    /// （`pinyin_simp` 的 `m`/`n` 就是靠这条保住的）。
    pub base_path: Option<PathBuf>,
}

/// 一次清洗的结果。
#[derive(Debug, Clone)]
pub struct SanitizeOutcome {
    /// 被清洗的词库路径。
    pub path: PathBuf,
    /// 词库名。
    pub dict: String,
    /// 剔除的词条条数。
    pub removed: usize,
    /// 文件是否真的被改写过。
    pub changed: bool,
    /// 新建的 `.rimesync-orig` 备份路径（没建就是 None）。
    pub backup: Option<PathBuf>,
}

/// 解析 `import_tables` 引用的词库文件。
///
/// librime 的查找顺序是「用户目录 → 系统 data 目录」，这里照抄：
/// 用户目录里有同名文件就用用户的，没有才用 data 目录里的官方词库。
///
/// 白名单来源与「清洗对象」是**两件事**：
/// * 白名单 = 系统 data 目录里同名词库（官方版本）的单音节；
/// * 清洗对象 = 系统里没有官方对应版本、纯粹是用户塞进来的第三方词库。
///
/// 所以用户目录里放了一份官方 `pinyin_simp.dict.yaml` 副本时，它算**基准**
/// （白名单来源），不算第三方（不清洗）—— 否则官方词库里的合法单音节
/// `m`/`n`（呣、唔、嗯）会被当成垃圾删掉。
pub fn resolve_imported_dicts(
    rime_dir: &Path,
    data_dirs: &[PathBuf],
    tables: &[String],
) -> Vec<DictRef> {
    let mut out = Vec::new();
    for name in tables {
        let file_name = format!("{name}.dict.yaml");
        // 官方同名词库（系统 data 目录）：白名单来源。
        //
        // 注意必须排除用户目录本身：调用方常常把 rime_dir 也塞进 data_dirs
        // （那是为了让用户自建方案能作为查表来源），若不排除，用户目录里的
        // 第三方词库会被自己当成「官方词库」⇒ 既不清洗、白名单也全错。
        let base_path = data_dirs
            .iter()
            .filter(|dir| dir.as_path() != rime_dir)
            .map(|dir| dir.join(&file_name))
            .find(|path| path.is_file());
        let user_path = rime_dir.join(&file_name);
        if user_path.is_file() {
            out.push(DictRef {
                name: name.clone(),
                path: user_path,
                user_owned: base_path.is_none(),
                base_path,
            });
            continue;
        }
        if let Some(base_path) = base_path {
            out.push(DictRef {
                name: name.clone(),
                path: base_path.clone(),
                user_owned: false,
                base_path: Some(base_path),
            });
        }
    }
    out
}

/// 词库文件里出现过的全部音节（code 列按空格切开）。
pub fn collect_syllables(bytes: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in body_lines(bytes) {
        let Some(code) = code_column(line) else {
            continue;
        };
        for piece in code.split(|byte| *byte == b' ') {
            if piece.is_empty() {
                continue;
            }
            out.insert(String::from_utf8_lossy(piece).into_owned());
        }
    }
    out
}

/// 白名单：作为基准的词库（官方词库）用过的单音节 + 三个兜底单音节。
pub fn base_single_syllables(base_dicts: &[&Path]) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BASE_SINGLE_SYLLABLES
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    for path in base_dicts {
        let Ok(bytes) = fs::read(path) else {
            continue;
        };
        for syllable in collect_syllables(&bytes) {
            if syllable.chars().count() == 1 {
                out.insert(syllable);
            }
        }
    }
    out
}

/// 一行词条是不是「含非法单音节」。
///
/// 词库格式：制表符分隔的若干列，第 1 列是词，第 2 列是 code（空格分隔的音节），
/// 第 3 列是权重。code 里有白名单外的单音节 ⇒ 这条词条会往音节表里塞垃圾。
pub fn entry_has_broken_syllable(line: &[u8], allowed_single: &BTreeSet<String>) -> bool {
    let Some(code) = code_column(line) else {
        return false;
    };
    code.split(|byte| *byte == b' ')
        .filter(|piece| !piece.is_empty())
        .any(|syllable| {
            syllable.len() == 1
                && !allowed_single.contains(&String::from_utf8_lossy(syllable).into_owned())
        })
}

/// 算出清洗后的文件内容与被剔除的条数。
///
/// 返回 `None` 表示这个文件不是词典（没有 `...` 头结束标记），不该动。
pub fn sanitize_bytes(bytes: &[u8], allowed_single: &BTreeSet<String>) -> Option<(Vec<u8>, usize)> {
    // BOM 单独摘出来，原样放回。
    let (bom, body) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (&bytes[..3], &bytes[3..])
    } else {
        (&bytes[..0], bytes)
    };
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    out.extend_from_slice(bom);
    let mut removed = 0usize;
    let mut in_body = false;
    for line in body.split_inclusive(|byte| *byte == b'\n') {
        let trimmed = trim_line(line);
        if !in_body {
            if trimmed == b"..." {
                in_body = true;
            }
            out.extend_from_slice(line);
            continue;
        }
        if entry_has_broken_syllable(line, allowed_single) {
            removed += 1;
            continue;
        }
        out.extend_from_slice(line);
    }
    if !in_body {
        return None;
    }
    Some((out, removed))
}

/// 清洗一个词库文件（就地改写）。
///
/// * `backup` 钩子在**改写前**被调用一次（引擎会把它接到自己的备份闸门，
///   于是这次改写会出现在用户可见的备份列表里，可以回滚）；
/// * 另外按本仓库惯例留一份 `<file>.rimesync-orig`（只建一次，不覆盖）。
///
/// 没有脏词条时**一个字节都不写**（避免无谓的 mtime 变化触发重编译）。
pub fn sanitize_in_place(
    path: &Path,
    allowed_single: &BTreeSet<String>,
    backup: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<SanitizeOutcome> {
    let dict = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.trim_end_matches(".dict.yaml").to_string())
        .unwrap_or_default();
    let bytes = fs::read(path).with_context(|| format!("读取 {}", path.display()))?;
    let Some((cleaned, removed)) = sanitize_bytes(&bytes, allowed_single) else {
        return Ok(SanitizeOutcome {
            path: path.to_path_buf(),
            dict,
            removed: 0,
            changed: false,
            backup: None,
        });
    };
    if removed == 0 {
        return Ok(SanitizeOutcome {
            path: path.to_path_buf(),
            dict,
            removed: 0,
            changed: false,
            backup: None,
        });
    }
    // 备份闸门先行：备份失败就不改。
    backup(path)?;
    // 本仓库惯例的原件备份（只建一次）。
    let orig = backup_path(path);
    let mut created = None;
    if !orig.exists() {
        if let Some(parent) = orig.parent() {
            fs::create_dir_all(parent).with_context(|| format!("创建 {}", parent.display()))?;
        }
        fs::write(&orig, &bytes).with_context(|| format!("备份 {}", orig.display()))?;
        created = Some(orig.clone());
    }
    write_atomic(path, &cleaned).with_context(|| format!("写入 {}", path.display()))?;
    Ok(SanitizeOutcome {
        path: path.to_path_buf(),
        dict,
        removed,
        changed: true,
        backup: created,
    })
}

/// 清洗合并词典导入的、位于**用户目录**里的第三方词库。
///
/// * `data_dirs` = 系统 data 目录（官方词库所在），用于解析 `import_tables`
///   里的官方词库并取它们的单音节做白名单；
/// * 官方词库本身**不改**（用户可能故意改过）；
/// * 返回每个词库的清洗结果。
pub fn sanitize_imported_dicts(
    rime_dir: &Path,
    data_dirs: &[PathBuf],
    backup: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<Vec<SanitizeOutcome>> {
    let Some(merged) = crate::merged_dict::detect(rime_dir) else {
        return Ok(Vec::new());
    };
    let dicts = resolve_imported_dicts(rime_dir, data_dirs, &merged.import_tables);
    // 基准词库 = 官方词库（系统 data 目录里的那份，或它的用户目录副本所对应的官方版本）。
    // 它们的单音节集合就是白名单：官方 luna_pinyin ⇒ {a,o,e}；
    // 官方 pinyin_simp ⇒ 再加上 {m,n}（呣、唔、嗯 这类方言词不会被误删）。
    let base_paths: Vec<&Path> = dicts
        .iter()
        .filter_map(|dict| dict.base_path.as_deref())
        .collect();
    let allowed = base_single_syllables(&base_paths);
    let mut out = Vec::new();
    for dict in dicts.iter().filter(|dict| dict.user_owned) {
        let outcome = sanitize_in_place(&dict.path, &allowed, backup)?;
        if outcome.removed > 0 {
            tracing::info!(
                "sanitized dictionary {}: removed {} entries with broken syllables",
                outcome.dict,
                outcome.removed
            );
        }
        out.push(outcome);
    }
    Ok(out)
}

/// 词库正文（`...` 之后的行）。
fn body_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let body = match bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        true => &bytes[3..],
        false => bytes,
    };
    let mut out = Vec::new();
    let mut in_body = false;
    for line in body.split_inclusive(|byte| *byte == b'\n') {
        if !in_body {
            if trim_line(line) == b"..." {
                in_body = true;
            }
            continue;
        }
        out.push(line);
    }
    out
}

/// 取一行的 code 列（第 2 列，制表符分隔）。
fn code_column(line: &[u8]) -> Option<&[u8]> {
    let line = strip_eol(line);
    if line.is_empty() || line.starts_with(b"#") {
        return None;
    }
    let mut columns = line.split(|byte| *byte == b'\t');
    let _word = columns.next();
    columns.next()
}

/// 去掉一行的行尾。
fn strip_eol(line: &[u8]) -> &[u8] {
    let mut line = line;
    if let Some(rest) = line.strip_suffix(b"\n") {
        line = rest;
    }
    if let Some(rest) = line.strip_suffix(b"\r") {
        line = rest;
    }
    line
}

/// 去掉一行的行尾与首尾空白（只用来做判断，不写回）。
fn trim_line(line: &[u8]) -> &[u8] {
    let line = strip_eol(line);
    let start = line
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(line.len());
    let end = line
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|index| index + 1)
        .unwrap_or(start);
    &line[start..end]
}

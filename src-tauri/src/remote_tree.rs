//! 远端目录树的遍历、复制与递归删除（去重与设备迁移共用）。
//!
//! 顺序规划（copy_plan / deletion_order）是纯函数，单测覆盖复制与删除顺序；
//! 真正的 I/O 只是 PROPFIND / GET / PUT / MKCOL / DELETE。

use anyhow::{anyhow, bail, Context, Result};
use rime_sync_core::webdav::DavClient;

/// 递归上限：异常深的目录不拖垮 UI 与远端。
pub(crate) const MAX_DEPTH: usize = 8;
pub(crate) const MAX_ENTRIES: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WalkEntry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyOp {
    pub from: String,
    pub to: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct CopyReport {
    pub files: usize,
    pub dirs: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DeleteReport {
    pub removed: usize,
    pub freed_bytes: u64,
}

fn depth(path: &str) -> usize {
    path.matches('/').count()
}

/// 复制顺序：浅层在先，同层目录先于文件（父目录必须先 MKCOL）。
pub(crate) fn copy_plan(src: &str, dst: &str, entries: &[WalkEntry]) -> Vec<CopyOp> {
    let mut ops: Vec<CopyOp> = entries
        .iter()
        .filter(|entry| entry.path.starts_with(src) && entry.path != src)
        .map(|entry| CopyOp {
            to: format!("{dst}{}", entry.path.strip_prefix(src).unwrap_or("")),
            from: entry.path.clone(),
            is_dir: entry.is_dir,
        })
        .collect();
    ops.sort_by(|left, right| {
        depth(&left.from)
            .cmp(&depth(&right.from))
            .then_with(|| right.is_dir.cmp(&left.is_dir))
            .then_with(|| left.from.cmp(&right.from))
    });
    ops
}

/// 删除顺序：深层在先，同层文件先于目录（先删内容再删空目录）。
pub(crate) fn deletion_order(entries: &[WalkEntry]) -> Vec<String> {
    let mut ordered: Vec<&WalkEntry> = entries.iter().collect();
    ordered.sort_by(|left, right| {
        depth(&right.path)
            .cmp(&depth(&left.path))
            .then_with(|| left.is_dir.cmp(&right.is_dir))
            .then_with(|| right.path.cmp(&left.path))
    });
    ordered.iter().map(|entry| entry.path.clone()).collect()
}

/// 广度优先遍历 root 之下（不含 root 自身）的条目。
pub(crate) fn walk(
    client: &DavClient,
    root: &str,
    max_depth: usize,
    max_entries: usize,
) -> Result<Vec<WalkEntry>> {
    let mut out: Vec<WalkEntry> = Vec::new();
    let mut queue: Vec<(String, usize)> = vec![(root.to_string(), 0)];
    while let Some((dir, level)) = queue.pop() {
        if level >= max_depth {
            continue;
        }
        let entries = client
            .propfind(&dir, 1)
            .with_context(|| format!("PROPFIND {dir}"))?;
        let prefix = format!("{dir}/");
        for entry in entries {
            let Some(relative) = entry.path.strip_prefix(&prefix) else {
                continue;
            };
            if relative.is_empty() {
                continue;
            }
            if entry.is_dir {
                queue.push((entry.path.clone(), level + 1));
            }
            out.push(WalkEntry {
                path: entry.path,
                is_dir: entry.is_dir,
                size: entry.size.unwrap_or(0),
                last_modified: entry.last_modified,
            });
            if out.len() > max_entries {
                bail!("远端目录条目超过 {max_entries} 个，停止遍历：{root}");
            }
        }
    }
    out.sort_by(|left, right| left.path.cmp(&right.path));
    out.dedup_by(|left, right| left.path == right.path);
    Ok(out)
}

/// 逐目录 / 逐文件复制；调用方负责在失败后回滚（删除 dst）。
pub(crate) fn copy_tree(client: &DavClient, src: &str, dst: &str) -> Result<CopyReport> {
    let entries = walk(client, src, MAX_DEPTH, MAX_ENTRIES)?;
    client.mkcol(dst).with_context(|| format!("MKCOL {dst}"))?;
    let mut report = CopyReport::default();
    for op in copy_plan(src, dst, &entries) {
        if op.is_dir {
            client
                .mkcol(&op.to)
                .with_context(|| format!("MKCOL {}", op.to))?;
            report.dirs += 1;
            continue;
        }
        let bytes = client
            .get(&op.from)?
            .ok_or_else(|| anyhow!("远端文件读取失败（404）：{}", op.from))?;
        client
            .put(&op.to, &bytes, None)
            .with_context(|| format!("PUT {}", op.to))?;
        report.files += 1;
    }
    Ok(report)
}

/// 显式递归删除：不依赖服务端 DELETE 集合是否递归。
pub(crate) fn delete_tree(client: &DavClient, root: &str) -> Result<DeleteReport> {
    let entries = walk(client, root, MAX_DEPTH, MAX_ENTRIES)?;
    let freed_bytes: u64 = entries
        .iter()
        .filter(|entry| !entry.is_dir)
        .map(|entry| entry.size)
        .sum();
    let mut removed = 0usize;
    for path in deletion_order(&entries) {
        client
            .delete(&path)
            .with_context(|| format!("DELETE {path}"))?;
        removed += 1;
    }
    client
        .delete(root)
        .with_context(|| format!("DELETE {root}"))?;
    Ok(DeleteReport {
        removed: removed + 1,
        freed_bytes,
    })
}

pub(crate) fn tree_size(entries: &[WalkEntry]) -> u64 {
    entries
        .iter()
        .filter(|entry| !entry.is_dir)
        .map(|entry| entry.size)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> WalkEntry {
        WalkEntry {
            path: path.to_string(),
            is_dir: false,
            size,
            last_modified: None,
        }
    }

    fn dir(path: &str) -> WalkEntry {
        WalkEntry {
            path: path.to_string(),
            is_dir: true,
            size: 0,
            last_modified: None,
        }
    }

    #[test]
    fn copy_plan_creates_parent_directories_before_files() {
        let entries = vec![
            file("sync/old/main.userdb.txt", 10),
            dir("sync/old/nested"),
            file("sync/old/nested/deep.userdb.txt", 20),
            dir("sync/old/empty"),
        ];
        let ops = copy_plan("sync/old", "sync/new", &entries);
        let order: Vec<&str> = ops.iter().map(|op| op.to.as_str()).collect();
        assert_eq!(order.len(), 4);
        let nested = order
            .iter()
            .position(|path| *path == "sync/new/nested")
            .unwrap();
        let deep = order
            .iter()
            .position(|path| *path == "sync/new/nested/deep.userdb.txt")
            .unwrap();
        let top_file = order
            .iter()
            .position(|path| *path == "sync/new/main.userdb.txt")
            .unwrap();
        assert!(nested < deep, "directory must be created before its files");
        assert!(
            nested < top_file,
            "directories come before files at the same depth"
        );
        // 目标路径逐段替换，绝不落到源目录里。
        assert!(ops.iter().all(|op| op.to.starts_with("sync/new")));
        assert!(ops.iter().all(|op| !op.to.contains("/old")));
    }

    #[test]
    fn deletion_order_removes_children_and_files_first() {
        let entries = vec![
            dir("sync/x"),
            file("sync/x/a.userdb.txt", 1),
            dir("sync/x/nested"),
            file("sync/x/nested/b.userdb.txt", 2),
            dir("sync/x/nested/empty"),
        ];
        let order = deletion_order(&entries);
        let pos = |path: &str| order.iter().position(|item| item == path).unwrap();
        assert!(pos("sync/x/nested/b.userdb.txt") < pos("sync/x/nested"));
        assert!(pos("sync/x/nested") < pos("sync/x"));
        assert!(pos("sync/x/a.userdb.txt") < pos("sync/x"));
        // 同一层内文件先于目录。
        assert!(pos("sync/x/a.userdb.txt") < pos("sync/x/nested"));
        assert!(pos("sync/x/nested/b.userdb.txt") < pos("sync/x/nested/empty"));
        assert_eq!(order.len(), entries.len());
    }

    #[test]
    fn tree_size_sums_files_only() {
        let entries = vec![file("sync/x/a", 3), dir("sync/x/y"), file("sync/x/y/b", 4)];
        assert_eq!(tree_size(&entries), 7);
    }
}

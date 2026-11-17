#!/usr/bin/env bash
# 清理测试留下的临时目录。
#
# 为什么需要（2026-11-17 起因）：集成测试大量使用「临时目录 + 跑完就删」的
# 套路，但**删不干净**的几类情况很常见（测试中途 panic、进程被 kill、
# 测试故意造只读目录来验证降级路径）。结果 /tmp 里会攒下成百上千个
# `rime-sync-*` 目录 —— 实测一次就攒了 6000 多个、2.1G。
# 用户 2026-11-08 明确交代过：历史操作产生的空间要及时收回，别攒着把磁盘撑满
# （曾因 cargo 编译把盘撑满，导致所有工具 ENOSPC 全部瘫痪）。
#
# 这个脚本按名字前缀清理，只碰「本项目测试建的目录」，不碰别的：
#   /tmp/rime-sync-*            （绝大多数测试临时目录）
#   /tmp/rime-sync-tests/*      （统一根目录后新加的）
#
# 用法：
#   scripts/ci/clean-test-tmp.sh            # 清 /tmp
#   scripts/ci/clean-test-tmp.sh --dry-run  # 只列不删
#
# 退出码：0 = 正常（删了或没东西可删）。

set -euo pipefail

DRY_RUN=0
if [ "${1:-}" = "--dry-run" ]; then
  DRY_RUN=1
fi

# 临时目录根：尊重 TMPDIR，其余按平台惯例。
if [ -n "${TMPDIR:-}" ]; then
  ROOT=${TMPDIR%/}
elif [ -d /private/var/folders ] && [ "$(uname -s)" = "Darwin" ]; then
  ROOT=$(getconf DARWIN_USER_TEMP_DIR 2>/dev/null || echo /tmp)
else
  ROOT=/tmp
fi

# 只清这两个前缀下的东西，且必须是「名字以 rime-sync 开头的一级目录」。
# 用 -maxdepth 1 ⇒ 不会递归进用户自己的目录。
count=0
freed=0

for target in "$ROOT"/rime-sync-*; do
  [ -e "$target" ] || continue
  [ -d "$target" ] || continue
  name=$(basename "$target")
  case "$name" in
    rime-sync-* | rime-sync) ;;
    *) continue ;;
  esac
  size=$(du -sk "$target" 2>/dev/null | cut -f1)
  size=${size:-0}
  count=$((count + 1))
  freed=$((freed + size))
  if [ "$DRY_RUN" = "1" ]; then
    echo "would remove $target (${size}K)"
  else
    # 只读目录要先放开权限，否则删不掉（测试故意造的只读目录会卡住 rm -rf）。
    chmod -R u+rwX "$target" 2>/dev/null || true
    rm -rf "$target" 2>/dev/null || true
  fi
done

if [ "$DRY_RUN" = "1" ]; then
  echo "dry-run: $count 个目录，约 ${freed}K"
else
  echo "已清理 $count 个测试临时目录，释放约 $((freed / 1024))M（根目录 $ROOT）"
fi

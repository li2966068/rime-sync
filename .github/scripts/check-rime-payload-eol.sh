#!/usr/bin/env bash
# check-rime-payload-eol.sh —— 断言随包 Rime 产物在 checkout 之后**逐字节没变**。
#
# 用法：bash .github/scripts/check-rime-payload-eol.sh（在仓库任意位置都行）
#
# 背景（v0.6.24 真机故障 + v0.6.25 发布产物校验失败）：
# GitHub 的 windows runner 上 git 默认 core.autocrlf=true，checkout 会把仓库里的
# LF 文本转成 CRLF。随包产物的 MANIFEST.json 记的是 sha256，文本文件一旦被转成
# CRLF，清单与产物立刻对不上 —— 安装期每个对不上的文件算一次失败
# （真机实测 copy_failed=11：6 个 build/*.schema.yaml + 5 个 config/*.custom.yaml
# 被污染，13 个 .bin 不受影响）→ 输入法只铺了一半。
#
# 这个脚本在**出事地点**（Windows runner 的真实 checkout）抓 bug，两段断言：
#   1) 工作区里的每个文件逐字节等于索引里的 blob：checkout 动过任何字节都跑不掉；
#   2) 工作区里的随包产物逐字节等于 MANIFEST.json 承诺的字节：这正是安装器
#      在用户机器上做的同一件事，提前几秒钟做完，就不必等编完 Weasel、
#      打完 setup.exe 才发现清单对不上。
#
# 与 Linux 侧同口径：crates 的预编译产物清单校验
# （scripts/ime/gen-prebuilt-build.py --check）断言的是同一件事 ——
# 随包产物的字节必须是清单承诺的字节；那边另有一份「模拟 Windows checkout」的
# 护栏（用 git -c core.autocrlf=true checkout-index 在 Linux 上复现 Windows 检出）。
#
# 只依赖 git + coreutils（sha256sum / openssl 兜底）；不用 python，也不用
# git check-attr --cached（那个选项要 git ≥ 2.40，runner 上是 2.34）。
set -uo pipefail

REPO=$(git rev-parse --show-toplevel) || exit 1
cd "$REPO" || exit 1

SCOPE=packaging/rime
MANIFEST=packaging/rime/prebuilt/MANIFEST.json
fail=0

report() {
  printf '%s\n' "$1" >&2
  fail=1
}

# sha256：sha256sum（Linux coreutils / Git Bash 自带）→ openssl（兜底）。
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    printf ''
  fi
}

# 清单条目（path / bytes / sha256），解析 MANIFEST.json 的 files 数组。
manifest_entries() {
  awk '
    { sub(/\r$/, "") }
    /"files": *\[/ { infiles = 1; next }
    infiles && /^ *\]/ { infiles = 0 }
    infiles {
      if (match($0, /"path": *"[^"]+"/)) {
        p = substr($0, RSTART, RLENGTH); sub(/.*"path": *"/, "", p); sub(/"$/, "", p)
      }
      if (match($0, /"bytes": *[0-9]+/)) {
        b = substr($0, RSTART, RLENGTH); sub(/.*: */, "", b)
      }
      if (match($0, /"sha256": *"[0-9a-f]+"/)) {
        h = substr($0, RSTART, RLENGTH); sub(/.*"sha256": *"/, "", h); sub(/"$/, "", h)
      }
      if (p != "" && b != "" && h != "") { print p " " b " " h; p = ""; b = ""; h = "" }
    }
  ' "$MANIFEST"
}

if [ ! -f "$MANIFEST" ]; then
  echo "FAIL payload-eol  找不到清单：$MANIFEST" >&2
  exit 1
fi

# 1) 工作区逐字节等于索引 blob —— checkout 改了任何字节都跑不掉。
#    （不用 git ls-files --eol 的 w/ 列：它报的是属性推断出来的行尾，不是磁盘上的
#     真实字节，CRLF 被转换过也会报 w/lf，是假阴性。）
count=0
while IFS= read -r rel; do
  [ -n "$rel" ] || continue
  count=$((count + 1))
  idx=$(git rev-parse ":$rel" 2>/dev/null)
  if [ -z "$idx" ]; then
    report "FAIL payload-eol  索引里没有 $rel"
    continue
  fi
  if [ ! -f "$rel" ]; then
    report "FAIL payload-eol  工作区里缺 $rel（checkout 没落地？）"
    continue
  fi
  wt=$(git hash-object --no-filters -- "$rel" 2>/dev/null)
  if [ "$wt" != "$idx" ]; then
    report "FAIL payload-eol  $rel 与索引里的字节不同（index=$idx worktree=$wt）—— checkout 转换了行尾？"
  fi
done < <(git ls-files -- "$SCOPE")

# 2) 工作区里的随包产物逐字节等于 MANIFEST.json 承诺的字节。
entries=0
while read -r rel bytes want; do
  [ -n "$rel" ] || continue
  entries=$((entries + 1))
  f=packaging/rime/prebuilt/$rel
  if [ ! -f "$f" ]; then
    report "FAIL payload-eol  清单里的文件不存在：$f"
    continue
  fi
  got_bytes=$(wc -c < "$f" | tr -d ' ')
  if [ "$got_bytes" != "$bytes" ]; then
    report "FAIL payload-eol  $rel 大小 expected=$bytes actual=$got_bytes（行尾被转换了？）"
    continue
  fi
  got=$(sha256_of "$f")
  if [ -z "$got" ]; then
    report "FAIL payload-eol  没有 sha256 工具（sha256sum / openssl）"
    break
  fi
  if [ "$got" != "$want" ]; then
    report "FAIL payload-eol  $rel sha256 expected=$want actual=$got"
  fi
done < <(manifest_entries)

if [ "$entries" -eq 0 ]; then
  report "FAIL payload-eol  清单里没解析出条目：$MANIFEST"
fi

if [ "$fail" -ne 0 ]; then
  exit 1
fi

printf 'ok   payload-eol  packaging/rime 下 %s 个文件逐字节等于索引，清单 %s 条逐字节一致\n' "$count" "$entries"

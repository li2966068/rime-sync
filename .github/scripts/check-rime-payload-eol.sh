#!/usr/bin/env bash
# check-rime-payload-eol.sh —— 断言随包 Rime 产物在 checkout 之后**逐字节没变**。
#
# 用法：
#   bash .github/scripts/check-rime-payload-eol.sh            # 校验当前仓库
#   bash .github/scripts/check-rime-payload-eol.sh --selftest  # 自检：合成仓库证明断言能抓出「空过」
#
# 背景（v0.6.24 真机故障 + v0.6.25 发布产物校验失败）：
# GitHub 的 windows runner 上 git 默认 core.autocrlf=true，checkout 会把仓库里的
# LF 文本转成 CRLF。随包产物的 MANIFEST.json 记的是 sha256，文本文件一旦被转成
# CRLF，清单与产物立刻对不上 —— 安装期每个对不上的文件算一次失败
# （真机实测 copy_failed=11：6 个 build/*.schema.yaml + 5 个 config/*.custom.yaml
# 被污染，13 个 .bin 不受影响）→ 输入法只铺了一半。
#
# 这个脚本在**出事地点**（Windows runner 的真实 checkout）抓 bug，三段断言：
#   1) 工作区里的每个文件逐字节等于索引里的 blob：checkout 动过任何字节都跑不掉；
#   2) 工作区里的随包产物逐字节等于 MANIFEST.json 承诺的字节：这正是安装器
#      在用户机器上做的同一件事，提前几秒钟做完，就不必等编完 Weasel、
#      打完 setup.exe 才发现清单对不上；
#   3) **范围非空 + 集合相等**：范围内必须真的跟踪着文件，且被跟踪的随包产物
#      集合必须与清单条目集合**完全一致**（不多不少）。
#      没有第 3 段时前两段会「空过」：把 packaging/rime/prebuilt/* 从索引里
#      删掉（文件还在磁盘上），第 1 段数到 0 个文件、第 2 段只查清单里
#      还在磁盘上的那几条，于是脚本报「0 个文件逐字节等于索引」却 exit 0
#      —— 断言什么都没查却显示通过（vacuous pass）。
#
# 与 Linux 侧同口径：crates 的预编译产物清单校验
# （scripts/ime/gen-prebuilt-build.py --check）断言的是同一件事 ——
# 随包产物的字节必须是清单承诺的字节；那边另有一份「模拟 Windows checkout」的
# 护栏（用 git -c core.autocrlf=true checkout-index 在 Linux 上复现 Windows 检出）。
#
# 只依赖 git + coreutils（sha256sum / openssl 兜底）；不用 python，也不用
# git check-attr --cached（那个选项要 git ≥ 2.40，runner 上是 2.34）。
set -uo pipefail

PROG=$(basename "$0")
SCRIPT_DIR=$(cd "$(dirname "$0")" 2>/dev/null && pwd || echo ".")
SCRIPT_PATH="$SCRIPT_DIR/$PROG"

# 校验范围：随包 Rime 产物目录。
SCOPE=packaging/rime
# 随包产物本体（清单描述的就是这个目录下的文件，路径相对于它）。
PAYLOAD_DIR=packaging/rime/prebuilt
# 清单本身与 .gitignore 不是「产物」，不参与集合比较。
PAYLOAD_META="MANIFEST.json .gitignore"
MANIFEST=packaging/rime/prebuilt/MANIFEST.json

SELFTEST=0
for arg in "$@"; do
  case "$arg" in
    --selftest) SELFTEST=1 ;;
    -h|--help)
      sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "$PROG: 未知参数：$arg" >&2; exit 2 ;;
  esac
done

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
manifest_entries() { # $1=清单路径
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
  ' "$1"
}

# check_payload <仓库根>
# 在给定仓库里跑三段断言；失败时把 FAIL 行打到 stderr 并返回 1。
check_payload() { # $1=仓库根
  local repo=$1 fail=0 count=0 entries=0 rel bytes want f idx wt got_bytes got
  local manifest_path="$repo/$MANIFEST"
  local payload_prefix="$PAYLOAD_DIR/"

  report() {
    printf '%s\n' "$1" >&2
    fail=1
  }

  if [ ! -f "$manifest_path" ]; then
    echo "FAIL payload-eol  找不到清单：$MANIFEST" >&2
    return 1
  fi

  cd "$repo" || {
    echo "FAIL payload-eol  进不去仓库根：$repo" >&2
    return 1
  }

  # 1) 工作区逐字节等于索引 blob —— checkout 改了任何字节都跑不掉。
  #    （不用 git ls-files --eol 的 w/ 列：它报的是属性推断出来的行尾，不是磁盘上的
  #     真实字节，CRLF 被转换过也会报 w/lf，是假阴性。）
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

  # 1b) 范围非空：跟踪文件数必须 > 0，否则第 1 段什么都没查就「通过」。
  if [ "$count" -eq 0 ]; then
    report "FAIL payload-eol  $SCOPE 下没有任何被跟踪的文件（0 个）—— 断言空过，产物根本没进仓库"
  fi

  # 2) 工作区里的随包产物逐字节等于 MANIFEST.json 承诺的字节。
  while read -r rel bytes want; do
    [ -n "$rel" ] || continue
    entries=$((entries + 1))
    f=$payload_prefix$rel
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
  done < <(manifest_entries "$manifest_path")

  if [ "$entries" -eq 0 ]; then
    report "FAIL payload-eol  清单里没解析出条目：$MANIFEST"
  fi

  # 3) 集合相等：被跟踪的随包产物集合 == 清单条目集合。
  #    「清单里少了」= 产物漏进仓库；「清单里多了」= 清单指向没进仓库的文件。
  local tracked_list manifest_list
  tracked_list=$(mktemp)
  manifest_list=$(mktemp)
  # 跟踪的随包产物（去掉清单自身与 .gitignore 这两个非产物文件）。
  git ls-files -- "$PAYLOAD_DIR" 2>/dev/null |
    sed -e "s#^$payload_prefix##" |
    while IFS= read -r p; do
      [ -n "$p" ] || continue
      case " $PAYLOAD_META " in
        *" $p "*) continue ;;
      esac
      printf '%s\n' "$p"
    done | LC_ALL=C sort >"$tracked_list"
  manifest_entries "$manifest_path" | awk '{print $1}' | LC_ALL=C sort >"$manifest_list"

  local only_tracked only_manifest
  only_tracked=$(LC_ALL=C comm -23 "$tracked_list" "$manifest_list")
  only_manifest=$(LC_ALL=C comm -13 "$tracked_list" "$manifest_list")
  if [ -n "$only_tracked" ]; then
    report "FAIL payload-eol  被跟踪但清单里没有的随包产物：$(printf '%s' "$only_tracked" | tr '\n' ' ')"
  fi
  if [ -n "$only_manifest" ]; then
    report "FAIL payload-eol  清单里有但没被跟踪的随包产物：$(printf '%s' "$only_manifest" | tr '\n' ' ')"
  fi
  local tracked_n manifest_n
  tracked_n=$(wc -l <"$tracked_list" | tr -d ' ')
  manifest_n=$(wc -l <"$manifest_list" | tr -d ' ')
  if [ "$tracked_n" -eq 0 ]; then
    report "FAIL payload-eol  $PAYLOAD_DIR 下没有被跟踪的随包产物（0 个）—— 断言空过，清单与仓库脱节"
  fi
  if [ "$tracked_n" != "$manifest_n" ]; then
    report "FAIL payload-eol  随包产物数量不一致：跟踪 $tracked_n 个 vs 清单 $manifest_n 条"
  fi
  rm -f "$tracked_list" "$manifest_list"

  if [ "$fail" -ne 0 ]; then
    return 1
  fi

  printf 'ok   payload-eol  %s 下 %s 个文件逐字节等于索引，清单 %s 条逐字节一致（随包产物集合相等：%s 个）\n' \
    "$SCOPE" "$count" "$entries" "$tracked_n"
  return 0
}

# ---------------------------------------------------------------- 自检

# 造一个合成仓库：随包产物 + 与之一致的清单。
make_fixture_repo() { # $1=仓库根
  local repo=$1
  mkdir -p "$repo/$PAYLOAD_DIR/build" "$repo/$PAYLOAD_DIR/config"
  printf 'schema: mock\n' >"$repo/$PAYLOAD_DIR/build/mock.schema.yaml"
  printf 'schema_list: []\n' >"$repo/$PAYLOAD_DIR/config/mock.custom.yaml"
  printf 'payload: mock\n' >"$repo/$PAYLOAD_DIR/build/mock.bin"
  ( cd "$repo" && git init -q . && git add -A . ) || return 1
  # 清单按产物现算一份（sha256 + 字节数）。
  python3 - "$repo/$MANIFEST" "$PAYLOAD_DIR" <<'PY'
import hashlib, json, os, sys
manifest_path, payload_dir = sys.argv[1], sys.argv[2]
root = os.path.dirname(manifest_path)
entries = []
for sub in ("build", "config"):
    base = os.path.join(root, sub)
    if not os.path.isdir(base):
        continue
    for name in sorted(os.listdir(base)):
        path = os.path.join(base, name)
        if not os.path.isfile(path):
            continue
        data = open(path, "rb").read()
        entries.append({"path": sub + "/" + name, "bytes": len(data),
                        "sha256": hashlib.sha256(data).hexdigest()})
with open(manifest_path, "w") as handle:
    handle.write(json.dumps({"files": entries}, indent=2) + "\n")
PY
  ( cd "$repo" && git add -A . )
}

run_selftest() {
  command -v python3 >/dev/null 2>&1 || { echo "$PROG: 自检需要 python3" >&2; exit 2; }
  command -v git >/dev/null 2>&1 || { echo "$PROG: 自检需要 git" >&2; exit 2; }

  local failures=0
  SELFTEST_BASE=$(mktemp -d)
  [ -n "$SELFTEST_BASE" ] && trap 'rm -rf "$SELFTEST_BASE"' EXIT

  expect_ok() { # $1=label $2=仓库根
    if ( check_payload "$2" ) >"$SELFTEST_BASE/out.log" 2>&1; then
      printf '  ok   自检：%s\n' "$1"
    else
      printf '  FAIL 自检：%s 应当通过却失败\n' "$1"
      sed -n '1,40p' "$SELFTEST_BASE/out.log"
      failures=$((failures + 1))
    fi
  }
  expect_fail() { # $1=label $2=仓库根 $3=期望出现在报错里的片段
    if ( check_payload "$2" ) >"$SELFTEST_BASE/out.log" 2>&1; then
      printf '  FAIL 自检：%s 应当失败却通过（断言空过）\n' "$1"
      failures=$((failures + 1))
      return
    fi
    if grep -qF "$3" "$SELFTEST_BASE/out.log"; then
      printf '  ok   自检：%s（被断言抓住）\n' "$1"
    else
      printf '  FAIL 自检：%s 失败了，但报错里没有「%s」\n' "$1" "$3"
      sed -n '1,40p' "$SELFTEST_BASE/out.log"
      failures=$((failures + 1))
    fi
  }

  # ---- 正向样本：完整的仓库必须通过 ----
  local good="$SELFTEST_BASE/good"
  make_fixture_repo "$good" || { echo "$PROG: 自检造夹具失败" >&2; exit 2; }
  expect_ok "完整随包产物应通过" "$good"

  # ---- 空过场景：把随包产物从索引里删掉（文件还在磁盘上）----
  # 旧版脚本在这里会报「0 个文件逐字节等于索引」然后 exit 0 —— 什么都没查却显示通过。
  local vacuous="$SELFTEST_BASE/vacuous"
  make_fixture_repo "$vacuous" || exit 2
  ( cd "$vacuous" && git rm -r -q --cached "$PAYLOAD_DIR/build" "$PAYLOAD_DIR/config" )
  expect_fail "随包产物被移出索引（空过场景）" "$vacuous" "没有被跟踪的随包产物"

  # ---- 范围整体为空：连 packaging/rime 下都没东西被跟踪（清单在磁盘上但没进索引）----
  local empty="$SELFTEST_BASE/empty"
  mkdir -p "$empty/$PAYLOAD_DIR"
  printf '{\n  "files": []\n}\n' >"$empty/$MANIFEST"
  ( cd "$empty" && git init -q . )
  expect_fail "范围内一个跟踪文件都没有" "$empty" "没有任何被跟踪的文件"

  # ---- 集合不一致：清单少了一条（产物漏进清单）----
  local missing="$SELFTEST_BASE/missing"
  make_fixture_repo "$missing" || exit 2
  python3 - "$missing/$MANIFEST" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as handle:
    data = json.load(handle)
data["files"] = data["files"][:-1]
with open(path, "w") as handle:
    handle.write(json.dumps(data, indent=2) + "\n")
PY
  expect_fail "清单少了一条（产物漏进清单）" "$missing" "被跟踪但清单里没有的随包产物"

  # ---- 集合不一致：清单多了一条（清单指向没进仓库的文件）----
  local extra="$SELFTEST_BASE/extra"
  make_fixture_repo "$extra" || exit 2
  python3 - "$extra/$MANIFEST" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as handle:
    data = json.load(handle)
data["files"].append({"path": "build/ghost.bin", "bytes": 1, "sha256": "0" * 64})
with open(path, "w") as handle:
    handle.write(json.dumps(data, indent=2) + "\n")
PY
  expect_fail "清单多了一条（指向没进仓库的文件）" "$extra" "清单里有但没被跟踪的随包产物"

  # ---- 真实缺陷仍要被抓：字节被改（模拟 checkout 转了行尾）----
  local dirty="$SELFTEST_BASE/dirty"
  make_fixture_repo "$dirty" || exit 2
  printf 'schema: mock\r\nchanged\r\n' >"$dirty/$PAYLOAD_DIR/build/mock.schema.yaml"
  expect_fail "随包产物字节被改（行尾被转换）" "$dirty" "与索引里的字节不同"

  # ---- 只动清单、不动工作区：第 2 条断言（清单承诺的字节 vs 磁盘字节）----
  # 2026-11-17 复核发现：上面 6 个场景都同时触发第 1 条（工作区 vs 索引），
  # 所以第 2 条即使写坏了也轮不到它生效，自检照样全绿。
  # 这里把清单改坏**再重新 add**（让工作区 == 索引）⇒ 第 1 条通过，
  # 只有第 2 条能抓住。
  local manifest_sha="$SELFTEST_BASE/manifest-sha"
  make_fixture_repo "$manifest_sha" || exit 2
  python3 - "$manifest_sha/$MANIFEST" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as handle:
    data = json.load(handle)
for entry in data["files"]:
    if entry["path"].endswith(".bin"):
        entry["sha256"] = "0" * 64
        break
with open(path, "w") as handle:
    handle.write(json.dumps(data, indent=2) + "\n")
PY
  ( cd "$manifest_sha" && git add -A . )
  expect_fail "清单里的 sha256 与磁盘字节不符（第 2 条断言）" "$manifest_sha" "sha256 expected="

  local manifest_size="$SELFTEST_BASE/manifest-size"
  make_fixture_repo "$manifest_size" || exit 2
  python3 - "$manifest_size/$MANIFEST" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as handle:
    data = json.load(handle)
for entry in data["files"]:
    if entry["path"].endswith(".bin"):
        entry["bytes"] = entry["bytes"] + 7
        break
with open(path, "w") as handle:
    handle.write(json.dumps(data, indent=2) + "\n")
PY
  ( cd "$manifest_size" && git add -A . )
  expect_fail "清单里的字节数与磁盘不符（第 2 条断言）" "$manifest_size" "大小 expected="

  printf '\n自检失败 %d 项\n' "$failures"
  [ "$failures" -eq 0 ]
}

if [ "$SELFTEST" -eq 1 ]; then
  run_selftest
  exit $?
fi

REPO=$(git rev-parse --show-toplevel) || exit 1
check_payload "$REPO"
exit $?

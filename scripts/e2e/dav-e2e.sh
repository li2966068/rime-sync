#!/usr/bin/env bash
# dav-e2e.sh — 真实 WebDAV 端到端：rclone serve webdav + 两台设备
#
# 覆盖的链路（每条断言都计 PASS/FAIL/SKIP）：
#   1. test-conn      两台设备都能读到 WebDAV（207）
#   2. transport A#1  normal 策略下首次上传 shared/files 与 sync/<devA>/
#   3. transport B#1  拉到 A 的配置与快照（两侧字节一致）
#   4. transport A#2  回拉 B 新写的快照；配置改动 A → 远端
#   5. transport B#2  配置改动 远端 → B（两侧字节一致）
#   6. apply B        备份 + transport 阶段；deploy 阶段按本机 librime 情况断言或 SKIP
#   7. 冲突           A/B 改同一词表 → B 生成 <name>.conflict-<devB>-<ts>，远端不被覆盖
#   8. 红线           远端无 *.userdb、shared/ 下无 *.userdb.txt、无 *.bin
#   9. status         远端能看到两台设备
#
# 用法:
#   cargo build -p rime-sync-cli
#   bash scripts/e2e/dav-e2e.sh
#   RIME_SYNC_CLI=target/debug/rime-sync-cli DAV_PORT=18099 KEEP=1 bash scripts/e2e/dav-e2e.sh
#
# 环境变量:
#   RIME_SYNC_CLI        CLI 路径（默认 target/debug/rime-sync-cli）
#   DAV_PORT             固定端口（默认 0 = 每次挑一个空闲端口）
#   DAV_HOST             监听地址（默认 127.0.0.1）
#   DAV_USER/DAV_PASS    HTTP Basic 账号（默认 e2e/e2e）
#   KEEP=1               成功也保留临时目录（失败一律保留）
#   E2E_WORK_DIR         临时目录的父目录（CI 里指向 runner.temp 便于上传日志）
#   E2E_EXPECT_APPLY     apply 的预期：auto（默认）/ deploy-ok / deploy-missing / ime-running
#
# 依赖: bash、rclone、curl、python3、cmp/find（coreutils/findutils）
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CLI="${RIME_SYNC_CLI:-$REPO_ROOT/target/debug/rime-sync-cli}"
DAV_PORT="${DAV_PORT:-0}"
DAV_HOST="${DAV_HOST:-127.0.0.1}"
DAV_USER="${DAV_USER:-e2e}"
DAV_PASS="${DAV_PASS:-e2e}"
KEEP="${KEEP:-0}"
EXPECT_APPLY="${E2E_EXPECT_APPLY:-auto}"
DEVA="e2e-devA"
DEVB="e2e-devB"

PASS=0
FAIL=0
SKIP=0
FAILED=()
SUMMARY_DONE=0
WORK=""
REMOTE=""
LOG=""
RCLONE_PID=""

ok()   { PASS=$((PASS + 1)); printf 'PASS  %s\n' "$*"; }
bad()  { FAIL=$((FAIL + 1)); FAILED+=("$*"); printf 'FAIL  %s\n' "$*" >&2; }
skip() { SKIP=$((SKIP + 1)); printf 'SKIP  %s\n' "$*"; }
step() { printf '\n=== %s ===\n' "$*"; }

tail_logs() {
  if [ -n "$LOG" ] && [ -f "$LOG" ]; then
    printf -- '--- rclone 日志（末 20 行）---\n' >&2
    tail -n 20 "$LOG" >&2 || true
  fi
  local dev
  for dev in "$DEVA" "$DEVB"; do
    if [ -n "$WORK" ] && [ -f "$WORK/$dev/log.txt" ]; then
      printf -- '--- %s/log.txt（末 10 行）---\n' "$dev" >&2
      tail -n 10 "$WORK/$dev/log.txt" >&2 || true
    fi
  done
}

summary() {
  [ "$SUMMARY_DONE" = "1" ] && return 0
  SUMMARY_DONE=1
  printf '\n===== e2e 汇总: PASS=%d FAIL=%d SKIP=%d =====\n' "$PASS" "$FAIL" "$SKIP"
  if [ "$FAIL" -gt 0 ]; then
    printf '失败项:\n' >&2
    printf '  - %s\n' "${FAILED[@]}" >&2
    tail_logs
  fi
}

die() { bad "$*"; summary; exit 1; }

cleanup() {
  local rc=$?
  trap - EXIT INT TERM
  if [ -n "$RCLONE_PID" ] && kill -0 "$RCLONE_PID" 2>/dev/null; then
    kill "$RCLONE_PID" 2>/dev/null || true
    local i
    for i in $(seq 1 25); do
      kill -0 "$RCLONE_PID" 2>/dev/null || break
      sleep 0.2
    done
    kill -9 "$RCLONE_PID" 2>/dev/null || true
    wait "$RCLONE_PID" 2>/dev/null || true
  fi
  if [ "$rc" -ne 0 ] && [ "$SUMMARY_DONE" = "0" ]; then
    bad "脚本在断言序列之外异常退出（rc=$rc）"
  fi
  summary
  if [ -n "$WORK" ] && [ -d "$WORK" ]; then
    if [ "$rc" -eq 0 ] && [ "$FAIL" -eq 0 ] && [ "$KEEP" != "1" ]; then
      rm -rf "$WORK"
    else
      printf '工作目录保留：%s\n' "$WORK"
    fi
  fi
  exit "$rc"
}
trap cleanup EXIT INT TERM

# --- 断言辅助 -------------------------------------------------------------

# assert_json <json 文件> <对 r 求值的 python 表达式> <描述>
assert_json() {
  local file="$1" expr="$2" desc="$3"
  if python3 -c '
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        r = json.load(handle)
except Exception as exc:
    print("invalid json: %s" % exc, file=sys.stderr)
    raise SystemExit(1)
raise SystemExit(0 if eval(sys.argv[2]) else 1)
' "$file" "$expr"; then
    ok "$desc"
  else
    bad "$desc"
    printf '  实际: %s\n' "$(tr '\n' ' ' < "$file" | cut -c1-600)" >&2
  fi
}

assert_rc() { # <实际 rc> <描述>
  if [ "$1" -eq 0 ]; then ok "$2"; else bad "$2（rc=$1）"; fi
}

assert_same() { # <文件a> <文件b> <描述>
  if cmp -s "$1" "$2"; then ok "$3"; else bad "$3（字节不同）"; fi
}

assert_exists() { # <路径> <描述>
  if [ -e "$1" ]; then ok "$2"; else bad "$2（缺少 $1）"; fi
}

pick_port() {
  python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()'
}

# --- 前置检查 -------------------------------------------------------------

step "前置检查"
for bin in rclone curl python3 cmp find; do
  command -v "$bin" >/dev/null 2>&1 || die "缺少依赖：$bin"
done
[ -x "$CLI" ] || die "找不到 CLI 可执行文件：$CLI（先 cargo build -p rime-sync-cli，或用 RIME_SYNC_CLI 指定）"
"$CLI" --help >/dev/null 2>&1 || die "CLI 无法运行：$CLI"
ok "依赖就绪（rclone $(rclone version 2>/dev/null | head -1 | awk '{print $2}')，CLI $CLI）"

WORK_PARENT="${E2E_WORK_DIR:-${TMPDIR:-/tmp}}"
mkdir -p "$WORK_PARENT"
WORK="$(mktemp -d "$WORK_PARENT/rime-sync-dav-e2e.XXXXXX")"
REMOTE="$WORK/remote"
LOG="$WORK/rclone.log"
mkdir -p "$REMOTE"

# --- 启动 WebDAV ----------------------------------------------------------

step "启动 rclone WebDAV（随机端口 + 临时目录）"
FIXED_PORT=0
[ "$DAV_PORT" != "0" ] && FIXED_PORT=1
STARTED=0
for attempt in 1 2 3 4 5; do
  if [ "$FIXED_PORT" = "0" ]; then
    DAV_PORT="$(pick_port)"
  fi
  rclone serve webdav "$REMOTE" --addr "$DAV_HOST:$DAV_PORT" --user "$DAV_USER" --pass "$DAV_PASS" --log-level INFO --log-file "$LOG" >/dev/null 2>&1 &
  RCLONE_PID=$!
  ready=0
  for _ in $(seq 1 80); do
    kill -0 "$RCLONE_PID" 2>/dev/null || break
    if curl -fsS -o /dev/null -u "$DAV_USER:$DAV_PASS" -X PROPFIND "http://$DAV_HOST:$DAV_PORT/" 2>/dev/null; then
      ready=1
      break
    fi
    sleep 0.25
  done
  if [ "$ready" = "1" ]; then
    STARTED=1
    break
  fi
  printf '端口 %s 未就绪（第 %s 次），换端口重试\n' "$DAV_PORT" "$attempt" >&2
  kill "$RCLONE_PID" 2>/dev/null || true
  wait "$RCLONE_PID" 2>/dev/null || true
  RCLONE_PID=""
done
[ "$STARTED" = "1" ] || die "rclone WebDAV 启动失败（5 次重试，见 $LOG）"
DAV_URL="http://$DAV_HOST:$DAV_PORT/"
ok "rclone WebDAV 就绪：$DAV_URL（根 $REMOTE）"

export RIME_SYNC_DAV_URL="$DAV_URL"
export RIME_SYNC_DAV_USER="$DAV_USER"
export RIME_SYNC_DAV_PASS="$DAV_PASS"
# SPEC-4 §A1：默认 preview 只拉不推；e2e 要验证双向搬运，显式切 normal。
export RIME_SYNC_FIRST_SYNC_POLICY="normal"

# --- 构造两台设备 ---------------------------------------------------------

# seed_device <设备名> <快照正文>
seed_device() {
  local dev="$1" payload="$2"
  local dir="$WORK/$dev/rime"
  mkdir -p "$dir/sync/$dev" "$dir/luna_pinyin.userdb" "$dir/build" "$WORK/$dev"
  printf 'installation_id: %s\n' "$dev" > "$dir/installation.yaml"
  cat > "$dir/default.custom.yaml" <<'YAML'
patch:
  style/horizontal: true
  menu/page_size: 9
YAML
  printf '# 词表\n你好\n世界\n' > "$dir/custom_phrase.txt"
  # 红线样本：LevelDB 目录本体绝不能被搬运
  printf 'leveldb-current\n' > "$dir/luna_pinyin.userdb/CURRENT"
  printf 'leveldb-log\n' > "$dir/luna_pinyin.userdb/LOG"
  printf 'binary\n' > "$dir/build/luna_pinyin.table.bin"
  printf '%s' "$payload" > "$dir/sync/$dev/luna_pinyin.userdb.txt"
  # 明文密码不落盘：密码只走 RIME_SYNC_DAV_PASS。
  # stop_fcitx5_during_apply=false：CI 没有输入法；本地跑时也绝不杀开发机的 fcitx5。
  cat > "$WORK/$dev/config.json" <<JSON
{
  "schema_version": 2,
  "device_id": "$dev",
  "rime_dir": "$dir",
  "first_sync_policy": "normal",
  "stop_fcitx5_during_apply": false,
  "webdav": {
    "url": "$DAV_URL",
    "username": "$DAV_USER",
    "remote_dir": "rime-sync"
  }
}
JSON
}

run_cli() { # <设备名> <子命令...>
  local dev="$1"
  shift
  RIME_SYNC_RIME_DIR="$WORK/$dev/rime" "$CLI" --config "$WORK/$dev/config.json" --json "$@"
}

run_json() { # <设备名> <输出 json> <子命令...> → 打印退出码
  local dev="$1" out="$2"
  shift 2
  local rc=0
  run_cli "$dev" "$@" >"$out" 2>"${out%.json}.err" || rc=$?
  printf '%s' "$rc"
}

step "构造两台设备的本地 Rime 目录"
seed_device "$DEVA" "$(printf '# snapshot %s\n你好\tni hao\t100\n' "$DEVA")"
seed_device "$DEVB" "$(printf '# snapshot %s\n你好\tni hao\t100\n' "$DEVB")"
DEV_ROOT_A="$WORK/$DEVA/rime"
DEV_ROOT_B="$WORK/$DEVB/rime"
ok "已构造 $DEVA / $DEVB（default.custom.yaml、custom_phrase.txt、snapshot、LevelDB、build 产物）"

# --- 1. test-conn ---------------------------------------------------------

step "1) test-conn（两台设备）"
for dev in "$DEVA" "$DEVB"; do
  rc="$(run_json "$dev" "$WORK/conn-$dev.json" test-conn)"
  assert_rc "$rc" "$dev test-conn 退出码 0"
  # 首次同步前远端目录还不存在：rclone 返回 404 + remote_dir_missing=true，但连通性 ok。
  assert_json "$WORK/conn-$dev.json" 'r["ok"] is True and (200 <= r["status"] < 300 or r["remote_dir_missing"] is True)' "$dev test-conn ok（远端目录可待首次同步创建）"
  assert_json "$WORK/conn-$dev.json" 'r["dav_class"] != ""' "$dev 识别到 DAV 支持（dav_class 非空）"
done

# --- 2. devA 首轮 transport ----------------------------------------------

step "2) transport $DEVA 第 1 轮（normal 策略）"
rc="$(run_json "$DEVA" "$WORK/a1.json" transport)"
assert_rc "$rc" "$DEVA transport 退出码 0"
assert_json "$WORK/a1.json" 'r["ok"] is True and r["mode"] == "normal"' "$DEVA transport ok=true mode=normal"
assert_json "$WORK/a1.json" 'r["pushed"] >= 3' "$DEVA 首次 pushed>=3（配置 + 词表 + 快照）"
assert_json "$WORK/a1.json" 'not r["conflicts"]' "$DEVA 首次无冲突"
assert_exists "$REMOTE/rime-sync/sync/$DEVA/config/files/default.custom.yaml" "远端出现 sync/$DEVA/config/files/default.custom.yaml（每设备配置目录）"
assert_exists "$REMOTE/rime-sync/shared/files/custom_phrase.txt" "远端出现 shared/files/custom_phrase.txt"
assert_exists "$REMOTE/rime-sync/sync/$DEVA/luna_pinyin.userdb.txt" "远端出现 sync/$DEVA/luna_pinyin.userdb.txt"
rc="$(run_json "$DEVA" "$WORK/conn-after-$DEVA.json" test-conn)"
assert_rc "$rc" "$DEVA 同步后 test-conn 退出码 0"
assert_json "$WORK/conn-after-$DEVA.json" 'r["ok"] is True and 200 <= r["status"] < 300 and r["remote_dir_missing"] is False' "$DEVA 同步后远端目录已存在（2xx / remote_dir_missing=false）"

# --- 3. devB 首轮 transport ----------------------------------------------

step "3) transport $DEVB 第 1 轮（拉取 A 的配置与快照）"
rc="$(run_json "$DEVB" "$WORK/b1.json" transport)"
assert_rc "$rc" "$DEVB transport 退出码 0"
assert_json "$WORK/b1.json" 'r["ok"] is True and r["mode"] == "normal"' "$DEVB transport ok=true mode=normal"
# shared 两侧初始字节一致 → Unchanged 不计 pulled；B 只拉 A 的快照，同时推自己的快照
assert_json "$WORK/b1.json" 'r["pulled"] >= 1 and r["pushed"] >= 1' "$DEVB 首次拉到 A 的快照并上传自己的快照"
assert_json "$WORK/b1.json" 'not r["conflicts"]' "$DEVB 首次无冲突"
assert_same "$DEV_ROOT_A/default.custom.yaml" "$DEV_ROOT_B/default.custom.yaml" "配置双端一致（default.custom.yaml）"
assert_same "$DEV_ROOT_A/custom_phrase.txt" "$DEV_ROOT_B/custom_phrase.txt" "词表双端一致（custom_phrase.txt）"
assert_same "$DEV_ROOT_A/sync/$DEVA/luna_pinyin.userdb.txt" "$DEV_ROOT_B/sync/$DEVA/luna_pinyin.userdb.txt" "A 的快照已落到 B（内容一致）"

# --- 4. 双向增量 ----------------------------------------------------------

cp "$DEV_ROOT_B/default.custom.yaml" "$WORK/before-b-config.yaml"
BEFORE_B_CONFIG="$WORK/before-b-config.yaml"

step "4) 配置改动 A 只进 A 的 profile（B 不受影响）"
cat > "$DEV_ROOT_A/default.custom.yaml" <<'YAML'
patch:
  style/horizontal: true
  menu/page_size: 7
YAML
rc="$(run_json "$DEVA" "$WORK/a2.json" transport)"
assert_rc "$rc" "$DEVA transport 退出码 0"
assert_json "$WORK/a2.json" 'r["ok"] is True and r["pushed"] >= 1' "$DEVA 第 2 轮上传了配置改动"
assert_json "$WORK/a2.json" 'not r["conflicts"]' "$DEVA 第 2 轮无冲突"
assert_same "$DEV_ROOT_A/default.custom.yaml" "$REMOTE/rime-sync/sync/$DEVA/config/files/default.custom.yaml" "A 的配置改动已上传到自己的 profile"

# B 同时写一份新快照（模拟 librime 导出），下一轮由 B 推送
printf '# snapshot %s v2\n你好\tni hao\t100\n世界\tshi jie\t50\n' "$DEVB" > "$DEV_ROOT_B/sync/$DEVB/luna_pinyin.userdb.txt"
rc="$(run_json "$DEVB" "$WORK/b2.json" transport)"
assert_rc "$rc" "$DEVB transport 退出码 0"
assert_json "$WORK/b2.json" 'r["ok"] is True and r["pushed"] >= 1 and r["pulled"] == 0' "$DEVB 第 2 轮只推自己的快照（不再拉 A 的配置）"
assert_json "$WORK/b2.json" 'not r["conflicts"]' "$DEVB 第 2 轮无冲突"
assert_same "$BEFORE_B_CONFIG" "$DEV_ROOT_B/default.custom.yaml" "B 的配置未被 A 的改动覆盖（每设备独立）"
assert_same "$DEV_ROOT_B/sync/$DEVB/luna_pinyin.userdb.txt" "$REMOTE/rime-sync/sync/$DEVB/luna_pinyin.userdb.txt" "B 的新快照已上传远端"

step "5) 快照回拉 A（sync/<devB>/ 路径 + 内容一致）"
rc="$(run_json "$DEVA" "$WORK/a3.json" transport)"
assert_rc "$rc" "$DEVA transport 退出码 0"
assert_json "$WORK/a3.json" 'r["ok"] is True and r["pulled"] >= 1' "$DEVA 第 3 轮拉到 B 的新快照"
assert_json "$WORK/a3.json" 'not r["conflicts"]' "$DEVA 第 3 轮无冲突"
assert_same "$DEV_ROOT_B/sync/$DEVB/luna_pinyin.userdb.txt" "$DEV_ROOT_A/sync/$DEVB/luna_pinyin.userdb.txt" "B 的快照已回拉到 A（内容一致）"
assert_same "$DEV_ROOT_A/default.custom.yaml" "$REMOTE/rime-sync/sync/$DEVA/config/files/default.custom.yaml" "A 的配置在回拉后未被改动"

# --- 5. apply -------------------------------------------------------------

step "6) apply $DEVB（备份 + transport；deploy 依赖本机 librime）"
rc="$(run_json "$DEVB" "$WORK/b-apply.json" apply)"
assert_rc "$rc" "apply 退出码 0（报告里的 ok 单独断言）"
assert_json "$WORK/b-apply.json" 'r.get("backup_id") is not None' "apply 生成了 pre-apply 备份"

HAVE_LIBRIME=0
if command -v rime_dict_manager >/dev/null 2>&1 && command -v rime_deployer >/dev/null 2>&1; then
  HAVE_LIBRIME=1
fi
IME_RUNNING=0
if pgrep -x fcitx5 >/dev/null 2>&1; then
  IME_RUNNING=1
fi
case "$EXPECT_APPLY" in
  auto)
    if [ "$HAVE_LIBRIME" = "0" ]; then
      EXPECT_APPLY="deploy-missing"
    elif [ "$IME_RUNNING" = "1" ]; then
      EXPECT_APPLY="ime-running"
    else
      EXPECT_APPLY="deploy-ok"
    fi
    ;;
esac
printf 'apply 预期模式：%s（librime=%s，fcitx5 运行中=%s）\n' "$EXPECT_APPLY" "$HAVE_LIBRIME" "$IME_RUNNING"

case "$EXPECT_APPLY" in
  deploy-ok)
    assert_json "$WORK/b-apply.json" 'r["ok"] is True' "apply ok=true"
    assert_json "$WORK/b-apply.json" 'r.get("merged_entries") is not None' "apply 导出了合并后词条数（merged_entries）"
    ;;
  ime-running)
    # 本机 fcitx5 正占着 userdb：脚本刻意不杀输入法（stop_fcitx5_during_apply=false），
    # 所以 apply 的 deploy 阶段按预期失败，transport 阶段必须已经生效。
    skip "apply deploy：本机 fcitx5 正在运行且脚本不杀输入法（要真 deploy 请设 E2E_EXPECT_APPLY=deploy-ok 并在无 fcitx5 环境跑）"
    assert_json "$WORK/b-apply.json" 'r["ok"] is False and "fcitx5 is still running" in (r.get("error") or "")' "apply 如实报告 deploy 被 fcitx5 占用阻塞"
    ;;
  deploy-missing)
    skip "apply deploy：未安装 rime_dict_manager/rime_deployer（Ubuntu: apt install librime-bin）；本脚本只验证 transport 阶段"
    assert_json "$WORK/b-apply.json" 'r["ok"] is False and "not found in PATH" in (r.get("error") or "")' "apply 在缺失 librime 时如实报告 deploy 失败"
    ;;
  *)
    die "未知 E2E_EXPECT_APPLY=$EXPECT_APPLY（auto/deploy-ok/deploy-missing/ime-running）"
    ;;
esac

if [ "$EXPECT_APPLY" != "deploy-ok" ]; then
  rc="$(run_json "$DEVB" "$WORK/b-after-apply.json" transport)"
  assert_rc "$rc" "apply 之后再 transport 退出码 0"
  assert_json "$WORK/b-after-apply.json" '(r["pushed"], r["pulled"], r["merged"]) == (0, 0, 0)' "apply 的 transport 阶段已把 index 落盘（再跑幂等）"
  assert_json "$WORK/b-after-apply.json" 'not r["conflicts"]' "apply 之后无残留冲突"
fi

# --- 6. 冲突 --------------------------------------------------------------

step "7) 制造冲突：A/B 改同一词表"
printf '# 词表\n你好\n世界\n冲突来自 A\n' > "$DEV_ROOT_A/custom_phrase.txt"
rc="$(run_json "$DEVA" "$WORK/a4.json" transport)"
assert_rc "$rc" "$DEVA transport 退出码 0"
assert_json "$WORK/a4.json" 'r["ok"] is True and r["pushed"] >= 1 and not r["conflicts"]' "$DEVA 的改动顺利上传（无冲突）"

printf '# 词表\n你好\n世界\n冲突来自 B\n' > "$DEV_ROOT_B/custom_phrase.txt"
rc="$(run_json "$DEVB" "$WORK/b3.json" transport)"
assert_rc "$rc" "$DEVB transport 退出码 0"
assert_json "$WORK/b3.json" 'r["ok"] is True' "$DEVB 冲突轮 ok=true（冲突不是错误）"
assert_json "$WORK/b3.json" 'len(r["conflicts"]) >= 1' "$DEVB 报告了冲突"

CONFLICT_COPIES=()
while IFS= read -r line; do
  [ -n "$line" ] && CONFLICT_COPIES+=("$line")
done < <(find "$DEV_ROOT_B" -maxdepth 1 -type f -name 'custom_phrase.txt.conflict-*' | sort)
if [ "${#CONFLICT_COPIES[@]}" -eq 1 ]; then
  ok "B 生成且只生成 1 份冲突副本：$(basename "${CONFLICT_COPIES[0]}")"
else
  bad "B 的冲突副本数量应为 1，实际 ${#CONFLICT_COPIES[@]}"
fi
if [ "${#CONFLICT_COPIES[@]}" -ge 1 ]; then
  assert_same "$DEV_ROOT_B/custom_phrase.txt" "${CONFLICT_COPIES[0]}" "冲突副本保留了 B 的本地字节"
fi
assert_same "$DEV_ROOT_A/custom_phrase.txt" "$REMOTE/rime-sync/shared/files/custom_phrase.txt" "冲突没有覆盖远端（远端仍是 A 的版本）"

rc="$(run_json "$DEVB" "$WORK/b4.json" transport)"
assert_rc "$rc" "$DEVB 冲突后再次 transport 退出码 0"
assert_json "$WORK/b4.json" 'len(r["conflicts"]) >= 1' "$DEVB 未处理的冲突仍然被报告"
RECOUNT=0
while IFS= read -r _line; do
  [ -n "$_line" ] && RECOUNT=$((RECOUNT + 1))
done < <(find "$DEV_ROOT_B" -maxdepth 1 -type f -name 'custom_phrase.txt.conflict-*' | sort)
if [ "$RECOUNT" -eq 1 ]; then
  ok "重复 transport 不再生成新的冲突副本（仍是 1 份）"
else
  bad "重复 transport 后冲突副本数量应为 1，实际 $RECOUNT"
fi

# --- 7. 红线 --------------------------------------------------------------

step "8) 红线检查（远端）"
USERDB_HITS="$(find "$REMOTE" -name '*.userdb' -print)"
if [ -z "$USERDB_HITS" ]; then ok "远端不存在 *.userdb 目录/文件"; else bad "远端出现 *.userdb：$USERDB_HITS"; fi
SHARED_SNAPSHOT_HITS="$(find "$REMOTE/rime-sync/shared" -name '*.userdb.txt' -print 2>/dev/null || true)"
if [ -z "$SHARED_SNAPSHOT_HITS" ]; then ok "shared/ 下不存在 *.userdb.txt"; else bad "shared/ 下出现 *.userdb.txt：$SHARED_SNAPSHOT_HITS"; fi
BIN_HITS="$(find "$REMOTE" -name '*.bin' -print)"
if [ -z "$BIN_HITS" ]; then ok "远端不存在 build 产物 *.bin"; else bad "远端出现 *.bin：$BIN_HITS"; fi

# --- 8. status ------------------------------------------------------------

step "9) status $DEVB（远端两台设备可见）"
rc="$(run_json "$DEVB" "$WORK/b-status.json" status)"
assert_rc "$rc" "status 退出码 0"
assert_json "$WORK/b-status.json" 'len(r["remote_devices"]) == 2' "远端能看到 2 台设备"
assert_json "$WORK/b-status.json" 'r["remote_error"] is None' "status 读取远端无错误"
assert_json "$WORK/b-status.json" 'r["local_shared"] >= 2 and r["local_snapshots"] >= 1' "status 统计到本地 shared 文件与快照"

exit 0

#!/usr/bin/env bash
# e2e-rclone.sh — 用本地 rclone WebDAV 跑一遍 transport，验证：
#   1) 首轮能上传 shared/ 与 sync/<device>/（非空）；
#   2) 第二轮完全幂等（pushed/pulled/merged 均为 0，远端字节不变）；
#   3) 红线：远端任何位置都不存在 *.userdb 目录，shared/ 下不存在 *.userdb.txt。
#
# 用法：
#   cargo build -p rime-sync-cli
#   bash .github/scripts/e2e-rclone.sh
#   RIME_SYNC_CLI=target/debug/rime-sync-cli DAV_PORT=18099 KEEP=1 bash ...
#
# 依赖：bash、rclone、curl、python3、find/sha256sum。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CLI="${RIME_SYNC_CLI:-$REPO_ROOT/target/debug/rime-sync-cli}"
DAV_PORT="${DAV_PORT:-18099}"
DAV_USER="${DAV_USER:-e2e}"
DAV_PASS="${DAV_PASS:-e2e}"
DAV_ADDR="127.0.0.1:$DAV_PORT"
KEEP="${KEEP:-0}"
DEVICE="e2e-device"

fail() { echo "E2E-FAIL: $*" >&2; exit 1; }
log() { echo "=== $* ==="; }

for bin in rclone curl python3 sha256sum; do
  command -v "$bin" >/dev/null 2>&1 || fail "缺少依赖：$bin"
done
[ -x "$CLI" ] || fail "找不到 CLI 可执行文件：$CLI（先 cargo build -p rime-sync-cli）"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/rime-sync-e2e.XXXXXX")"
RIME="$WORK/rime"
REMOTE="$WORK/remote"
APP="$WORK/app"
RCLONE_PID=""
cleanup() {
  local rc=$?
  if [ -n "$RCLONE_PID" ] && kill -0 "$RCLONE_PID" 2>/dev/null; then
    kill "$RCLONE_PID" 2>/dev/null || true
    wait "$RCLONE_PID" 2>/dev/null || true
  fi
  if [ "$rc" -eq 0 ] && [ "$KEEP" != "1" ]; then
    rm -rf "$WORK"
  else
    echo "E2E 工作目录保留：$WORK"
  fi
  exit "$rc"
}
trap cleanup EXIT INT TERM

mkdir -p "$RIME/build" "$RIME/luna_pinyin.userdb" "$RIME/sync/$DEVICE" "$REMOTE" "$APP"
cat > "$RIME/installation.yaml" <<'YAML'
installation_id: e2e-device
YAML
cat > "$RIME/default.custom.yaml" <<'YAML'
patch:
  style/horizontal: true
  menu/page_size: 9
YAML
cat > "$RIME/luna_pinyin.dict.yaml" <<'YAML'
---
name: luna_pinyin
version: "1.0"
sort: by_weight
...
你好	ni hao	100
YAML
printf '# 词表\n你好\n世界\n' > "$RIME/custom_phrase.txt"
# 红线样本：LevelDB 目录本体绝不能被搬运
printf 'leveldb-current\n' > "$RIME/luna_pinyin.userdb/CURRENT"
printf 'leveldb-log\n' > "$RIME/luna_pinyin.userdb/LOG"
printf 'binary\n' > "$RIME/build/luna_pinyin.table.bin"
# 词库快照：应该只出现在远端 sync/<device>/ 下
printf 'userdb snapshot payload\n' > "$RIME/sync/$DEVICE/luna_pinyin.userdb.txt"

log "启动 rclone WebDAV（$DAV_ADDR，根 $REMOTE）"
rclone serve webdav "$REMOTE" \
  --addr "$DAV_ADDR" \
  --user "$DAV_USER" \
  --pass "$DAV_PASS" \
  --log-file "$WORK/rclone.log" \
  >/dev/null 2>&1 &
RCLONE_PID=$!

ready=0
for _ in $(seq 1 60); do
  if curl -fsS -o /dev/null -u "$DAV_USER:$DAV_PASS" -X PROPFIND "http://$DAV_ADDR/" 2>/dev/null; then
    ready=1
    break
  fi
  kill -0 "$RCLONE_PID" 2>/dev/null || fail "rclone 进程已退出，日志：$(tail -5 "$WORK/rclone.log" 2>/dev/null || true)"
  sleep 0.25
done
[ "$ready" = "1" ] || fail "rclone WebDAV 在 15s 内未就绪"

export RIME_SYNC_DAV_URL="http://$DAV_ADDR/"
export RIME_SYNC_DAV_USER="$DAV_USER"
export RIME_SYNC_DAV_PASS="$DAV_PASS"
export RIME_SYNC_RIME_DIR="$RIME"

remote_paths() {
  (cd "$REMOTE" && find . -type f | sort)
}

remote_manifest() {
  # 远端字节快照：路径 + sha256。
  # owner.json 是心跳文件（含 session/heartbeat_ts），每轮都会刷新，不属于幂等对象。
  (cd "$REMOTE" && find . -type f ! -name owner.json -print0 | sort -z | xargs -0 -r sha256sum | sort -k2)
}

log "transport 第 1 轮"
"$CLI" --config "$APP/config.json" --json transport > "$WORK/run1.json"
cat "$WORK/run1.json"
python3 - "$WORK/run1.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
assert r["ok"] is True, f"首轮 ok=false: {r}"
assert r["pushed"] >= 3, f"首轮 pushed 过少: {r}"
assert not r["conflicts"], f"首轮出现冲突: {r['conflicts']}"
print(f"首轮 OK: pushed={r['pushed']} merged={r['merged']} pulled={r['pulled']}")
PY

PATHS1="$(remote_paths)"
MANIFEST1="$(remote_manifest)"
[ -n "$MANIFEST1" ] || fail "首轮后远端为空"

log "断言远端结构"
grep -q "rime-sync/shared/files/default.custom.yaml" <<<"$MANIFEST1" \
  || fail "远端缺少 shared/files/default.custom.yaml"
grep -q "rime-sync/sync/$DEVICE/luna_pinyin.userdb.txt" <<<"$MANIFEST1" \
  || fail "远端缺少 sync/$DEVICE/luna_pinyin.userdb.txt"
grep -q "custom_phrase.txt" <<<"$MANIFEST1" || fail "远端缺少 custom_phrase.txt"

log "断言红线（*.userdb / shared 下的 *.userdb.txt）"
USERDB_HITS="$(find "$REMOTE" -name '*.userdb' -print)"
[ -z "$USERDB_HITS" ] || fail "远端出现 *.userdb 目录/文件：$USERDB_HITS"
SHARED_SNAPSHOT_HITS="$(find "$REMOTE/rime-sync/shared" -name '*.userdb.txt' -print 2>/dev/null || true)"
[ -z "$SHARED_SNAPSHOT_HITS" ] || fail "shared/ 下出现 *.userdb.txt（会让 librime 重复合并）：$SHARED_SNAPSHOT_HITS"
BIN_HITS="$(find "$REMOTE" -name '*.bin' -print)"
[ -z "$BIN_HITS" ] || fail "远端出现 build 产物 *.bin：$BIN_HITS"

log "transport 第 2 轮（幂等）"
"$CLI" --config "$APP/config.json" --json transport > "$WORK/run2.json"
cat "$WORK/run2.json"
python3 - "$WORK/run2.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
assert r["ok"] is True, f"第二轮 ok=false: {r}"
assert (r["pushed"], r["pulled"], r["merged"]) == (0, 0, 0), f"第二轮不幂等: {r}"
assert not r["conflicts"], f"第二轮出现冲突: {r['conflicts']}"
print("第二轮幂等 OK: pushed=pulled=merged=0")
PY

PATHS2="$(remote_paths)"
MANIFEST2="$(remote_manifest)"
if [ "$PATHS1" != "$PATHS2" ]; then
  diff <(echo "$PATHS1") <(echo "$PATHS2") || true
  fail "第二轮后远端文件集合发生变化（非幂等）"
fi
if [ "$MANIFEST1" != "$MANIFEST2" ]; then
  diff <(echo "$MANIFEST1") <(echo "$MANIFEST2") || true
  fail "第二轮后远端数据文件字节发生变化（非幂等）"
fi
echo "远端文件集合与字节在第二轮前后一致（$(wc -l <<<"$MANIFEST2") 个数据文件，owner.json 心跳除外）"

log "E2E 全部通过"

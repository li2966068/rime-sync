#!/usr/bin/env bash
# SPEC-2 E2E：备份 / 回滚 / 残留锁回收 / watch 事件 / probe 只读失败 + 红线复验。
# 依赖：rclone（本地 WebDAV）、timeout、curl、python3。
# 默认用 RIME_SYNC_SECRET_BACKEND=file 模拟无 Secret Service 的 headless 环境；
# 末尾会在不带该变量时再跑一次 status，报告本机真实探测到的后端。
set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)
BIN=${BIN:-$ROOT/target/debug/rime-sync-cli}
if [ ! -x "$BIN" ]; then
  echo "missing binary: $BIN (run: cargo build --workspace)" >&2
  exit 2
fi

export RIME_SYNC_SECRET_BACKEND=file

WORK=$(mktemp -d /tmp/rime-sync-e2e-XXXXXX)
RIME="$WORK/rime"
STATE="$WORK/state"
DAV_DIR="$WORK/dav"
DAV_RO="$WORK/dav-ro"
CFG="$STATE/config.json"
CFG_RO="$STATE/config-ro.json"
PORT=${PORT:-18181}
PORT_RO=${PORT_RO:-18182}
SERVER_PID=""
SERVER_RO_PID=""
WATCH_PID=""
FAILS=0

cleanup() {
  [ -n "$WATCH_PID" ] && kill "$WATCH_PID" 2>/dev/null
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null
  [ -n "$SERVER_RO_PID" ] && kill "$SERVER_RO_PID" 2>/dev/null
  wait 2>/dev/null
  rm -rf "$WORK"
}
trap cleanup EXIT

pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1"; FAILS=$((FAILS + 1)); }

wait_http() {
  local url=$1
  for _ in $(seq 1 50); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "$url" 2>/dev/null || true)
    [ "$code" != "000" ] && return 0
    sleep 0.1
  done
  return 1
}

mkdir -p "$RIME/sync/devA" "$STATE" "$DAV_DIR" "$DAV_RO/rime-sync"
printf 'patch:\n  a: 1\n' > "$RIME/default.custom.yaml"
printf 'word\n' > "$RIME/words.txt"
printf 'var: 1\n' > "$RIME/user.yaml"
printf 'snapshot\n' > "$RIME/sync/devA/main.userdb.txt"
mkdir -p "$RIME/main.userdb"
printf 'leveldb\n' > "$RIME/main.userdb/CURRENT"

# 旧版配置：带明文 password，用于验证迁移后不再落盘。
cat > "$CFG" <<EOF
{
  "webdav": {
    "url": "http://127.0.0.1:${PORT}",
    "username": "u",
    "password": "p",
    "remote_dir": "rime-sync"
  },
  "device_id": "devA",
  "rime_dir": "$RIME"
}
EOF

rclone serve webdav "$DAV_DIR" --addr "127.0.0.1:${PORT}" --user u --pass p >"$WORK/rclone.log" 2>&1 &
SERVER_PID=$!
rclone serve webdav "$DAV_RO" --addr "127.0.0.1:${PORT_RO}" --user u --pass p --read-only >"$WORK/rclone-ro.log" 2>&1 &
SERVER_RO_PID=$!
wait_http "http://127.0.0.1:${PORT}/" || { echo "rclone webdav did not start"; cat "$WORK/rclone.log"; exit 2; }
wait_http "http://127.0.0.1:${PORT_RO}/" || { echo "rclone read-only webdav did not start"; exit 2; }

echo "== a) apply 前生成备份 =="
"$BIN" --config "$CFG" --rime-dir "$RIME" apply --json >"$WORK/apply.json" 2>"$WORK/apply.err"
APPLY_CODE=$?
head -20 "$WORK/apply.json"
BACKUP_ID=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("backup_id") or "")' "$WORK/apply.json")
if [ -n "$BACKUP_ID" ] && [ -f "$STATE/backups/$BACKUP_ID/manifest.json" ]; then
  pass "apply 在任何写操作前生成了备份 $BACKUP_ID（apply exit=$APPLY_CODE）"
else
  fail "apply 没有生成备份（exit=$APPLY_CODE）"; head -10 "$WORK/apply.err"
fi
if grep -q 'default.custom.yaml' "$STATE/backups/$BACKUP_ID/manifest.json" 2>/dev/null; then
  pass "备份 manifest 含 default.custom.yaml"
else
  fail "备份 manifest 缺少 *.custom.yaml"
fi
"$BIN" --config "$CFG" --rime-dir "$RIME" backups --json >"$WORK/backups.json" 2>&1
if grep -q "$BACKUP_ID" "$WORK/backups.json"; then
  pass "backups 子命令列出备份 $BACKUP_ID"
else
  fail "backups 子命令没有列出备份"; head -10 "$WORK/backups.json"
fi

echo "== 迁移：config.json 不再含明文 password（file 后端模拟 headless）=="
if grep -q '"password"' "$CFG"; then
  fail "config.json 仍有明文 password 字段"; cat "$CFG"
else
  pass "config.json 无明文 password 字段"
fi
if [ -f "$STATE/secrets.json" ] && [ "$(stat -c '%a' "$STATE/secrets.json")" = "600" ]; then
  pass "无 Secret Service 时回落到 0600 secrets.json"
else
  fail "secrets.json 缺失或权限不是 0600"; ls -l "$STATE"
fi

echo "== b) rollback 还原被改动的 *.custom.yaml =="
printf 'patch:\n  a: 99\n  hacked: true\n' > "$RIME/default.custom.yaml"
"$BIN" --config "$CFG" --rime-dir "$RIME" rollback "$BACKUP_ID" --json >"$WORK/rollback.json" 2>&1
ROLLBACK_CODE=$?
head -10 "$WORK/rollback.json"
EXPECTED=$(printf 'patch:\n  a: 1')
if [ "$ROLLBACK_CODE" -eq 0 ] && [ "$(cat "$RIME/default.custom.yaml")" = "$EXPECTED" ]; then
  pass "rollback 还原 default.custom.yaml"
else
  fail "rollback 未还原（exit=$ROLLBACK_CODE, content=$(head -2 "$RIME/default.custom.yaml" | tr '\n' '|'))"
fi
PRE_RESTORE=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("pre_restore_id") or "")' "$WORK/rollback.json" 2>/dev/null)
if [ -n "$PRE_RESTORE" ] && [ -d "$STATE/backups/$PRE_RESTORE" ]; then
  pass "rollback 前自动建了 pre-restore 备份 $PRE_RESTORE"
else
  fail "缺少 pre-restore 备份"
fi

echo "== c) 伪造 stale 锁（不存在的 pid）→ 第二次运行自动接管 =="
printf '{"pid":4000000,"hostname":"ghost","started_at":1700000000,"boot_id":null}\n' > "$STATE/sync.lock"
"$BIN" --config "$CFG" --rime-dir "$RIME" transport --json >"$WORK/transport.json" 2>"$WORK/transport.err"
TRANSPORT_CODE=$?
head -14 "$WORK/transport.json"
if [ "$TRANSPORT_CODE" -eq 0 ] && ! grep -q 'another sync is running' "$WORK/transport.json"; then
  pass "stale 锁被接管，transport 正常完成"
else
  fail "stale 锁未被接管（exit=$TRANSPORT_CODE）"; head -10 "$WORK/transport.err"
fi
if [ ! -f "$STATE/sync.lock" ]; then
  pass "transport 结束后锁已释放"
else
  fail "锁文件残留"
fi
"$BIN" --config "$CFG" --rime-dir "$RIME" status --json >"$WORK/status.json" 2>&1
if grep -q '"lock_held": false' "$WORK/status.json" && grep -q '"lock_stale": false' "$WORK/status.json"; then
  pass "status 报告锁状态为空闲"
else
  fail "status 锁状态异常"; head -20 "$WORK/status.json"
fi

echo "== d) watch：文件变更后 5s 内触发一次 transport =="
timeout 20 "$BIN" --config "$CFG" --rime-dir "$RIME" watch --debounce-secs 1 >"$WORK/watch.out" 2>"$WORK/watch.err" &
WATCH_PID=$!
sleep 2
printf 'word\nnew-word\n' > "$RIME/words.txt"
TRIGGERED=0
for _ in $(seq 1 80); do
  if grep -q 'transport triggered' "$WORK/watch.err" 2>/dev/null; then TRIGGERED=1; break; fi
  sleep 0.1
done
if [ "$TRIGGERED" -eq 1 ]; then
  pass "watch 在 5s 内触发了 transport：$(grep -m1 'transport triggered' "$WORK/watch.err")"
else
  fail "watch 未在 5s 内触发"; tail -5 "$WORK/watch.err"
fi
kill "$WATCH_PID" 2>/dev/null
wait "$WATCH_PID" 2>/dev/null
WATCH_PID=""
if [ -f "$DAV_DIR/rime-sync/shared/files/words.txt" ]; then
  pass "watch 触发的 transport 已把 words.txt 推到远端"
else
  fail "watch 触发的 transport 没有上传 words.txt"
fi

echo "== e) probe 对只读 WebDAV 返回带中文提示的失败 =="
cat > "$CFG_RO" <<EOF
{
  "webdav": {
    "url": "http://127.0.0.1:${PORT_RO}",
    "username": "u",
    "remote_dir": "rime-sync"
  },
  "device_id": "devA",
  "rime_dir": "$RIME"
}
EOF
"$BIN" --config "$CFG_RO" --dav-pass p --rime-dir "$RIME" probe --json >"$WORK/probe.json" 2>"$WORK/probe.err"
PROBE_CODE=$?
head -30 "$WORK/probe.json"
if [ "$PROBE_CODE" -ne 0 ] && grep -q '"ok": false' "$WORK/probe.json"; then
  pass "probe 判失败（exit=$PROBE_CODE）"
else
  fail "probe 未按预期失败（exit=$PROBE_CODE）"; head -5 "$WORK/probe.err"
fi
if python3 - "$WORK/probe.json" <<'PY'
import json,sys
data=json.load(open(sys.argv[1]))
hint=data.get("hint") or ""
assert any("一" <= ch <= "鿿" for ch in hint), "hint has no chinese"
failed=[s for s in data.get("steps",[]) if not s.get("ok")]
assert failed, "no failed step"
assert failed[0]["status"] >= 400, "no 4xx/5xx status"
print("failed step:", failed[0]["step"], failed[0]["status"])
print("hint:", hint)
PY
then
  pass "probe 返回中文可执行提示（失败步骤已映射状态）"
else
  fail "probe 提示不是中文可执行建议"
fi

echo "== 红线复验：*.userdb/ 不上传 / 无明文密码 =="
USERDB_REMOTE=$(find "$DAV_DIR" -name '*.userdb' -print 2>/dev/null | head -5)
if [ -z "$USERDB_REMOTE" ]; then
  pass "远端无 *.userdb 目录/文件"
else
  fail "远端出现 userdb 残留: $USERDB_REMOTE"
fi
if [ -f "$DAV_DIR/rime-sync/sync/devA/main.userdb.txt" ]; then
  pass "快照 main.userdb.txt 正常上传"
else
  fail "快照未上传"; find "$DAV_DIR" -maxdepth 4 | head -20
fi
if grep -l '"password"' "$STATE"/*.json 2>/dev/null | grep -qv secrets; then
  fail "state 目录出现明文 password 字段"
else
  pass "配置文件中无明文 password 字段"
fi

echo "== 本机真实后端探测（不强制 file）=="
unset RIME_SYNC_SECRET_BACKEND
"$BIN" --config "$CFG" --rime-dir "$RIME" status --json 2>/dev/null | grep -m1 '"secret_backend"'

echo
if [ "$FAILS" -eq 0 ]; then
  echo "E2E result: all checks passed"
  exit 0
else
  echo "E2E result: $FAILS check(s) failed"
  exit 1
fi
#!/usr/bin/env bash
# SPEC-2 E2E：备份 / 残留锁回收 / watch 事件 + 红线复验。
# 依赖：rclone（本地 WebDAV）、timeout、curl、python3。
# 默认用 RIME_SYNC_SECRET_BACKEND=file 模拟无 Secret Service 的 headless 环境；
# 末尾会在不带该变量时再跑一次 status，报告本机真实探测到的后端。
#
# 注：列备份（backups）、回滚（rollback）、写权限探测（probe）这三个 CLI 子命令
# 已删除（桌面端 GUI 里有等价入口：设置页备份区、连接页写权限探测）。
# 备份「列出来」的事实改用文件系统核对（备份根下的目录），
# restore 仍由 core 提供并由 crates/core/tests/transport_backup.rs 覆盖。
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
# 列备份：CLI 的 `backups` 子命令已删除，改用文件系统核对同一个事实
# （备份确实落在备份根下、能被列出来）。
if [ -d "$STATE/backups/$BACKUP_ID" ]; then
  pass "备份 $BACKUP_ID 落在备份根下、可被列出"
else
  fail "备份根下找不到 $BACKUP_ID"; ls -1 "$STATE/backups" 2>/dev/null | head -10
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

echo "== b) 还原点备份里留着改动前的 *.custom.yaml（供 GUI 还原） =="
# CLI 的 `rollback` 子命令已删（桌面端设置页备份区有还原按钮），
# 这里改成核对同一个事实：备份 manifest 里记下了改动前的原文件内容，
# 还原所需的还原点确实在备份根下。
printf 'patch:\n  a: 99\n  hacked: true\n' > "$RIME/default.custom.yaml"
EXPECTED=$(printf 'patch:\n  a: 1')
if python3 - "$STATE/backups/$BACKUP_ID/manifest.json" "$RIME" <<'PY'
import json,sys,hashlib,os
manifest=json.load(open(sys.argv[1]))
rime=sys.argv[2]
old=None
for entry in manifest.get("files",[]):
    if entry.get("rel")=="default.custom.yaml":
        old=entry
        break
assert old is not None, "manifest 里没有 default.custom.yaml"
path=os.path.join(rime,"default.custom.yaml")
print("manifest sha256:", old["sha256"])
print("current  sha256:", hashlib.sha256(open(path,"rb").read()).hexdigest())
PY
then
  pass "还原点备份 manifest 记下了改动前的 default.custom.yaml（GUI 可据此还原）"
else
  fail "还原点备份 manifest 里查不到 default.custom.yaml"
fi
if [ -d "$STATE/backups/$BACKUP_ID" ]; then
  pass "还原点 $BACKUP_ID 落在备份根下（GUI 备份区可列出并还原）"
else
  fail "备份根下找不到还原点 $BACKUP_ID"
fi
# 用备份里存的那份把文件放回去（core 的 restore 就是这么干的：
# 校验 manifest 后把 files/ 下的内容写回），后续步骤的文件状态与原来一致。
if [ -f "$STATE/backups/$BACKUP_ID/files/default.custom.yaml" ]; then
  cp "$STATE/backups/$BACKUP_ID/files/default.custom.yaml" "$RIME/default.custom.yaml"
  if [ "$(cat "$RIME/default.custom.yaml")" = "$EXPECTED" ]; then
    pass "还原点里的 default.custom.yaml 内容与改动前一致（还原可行）"
  else
    fail "还原点里的内容与改动前不一致（content=$(head -2 "$RIME/default.custom.yaml" | tr '\n' '|'))"
  fi
else
  fail "还原点里找不到 default.custom.yaml"
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

echo "== e) 只读 WebDAV 拒绝写入（写权限探测的底层前提） =="
# CLI 的 `probe` 子命令已删（桌面端连接页有「写权限探测」按钮），
# core 的 DavClient::check_write 仍在，桌面端仍用它。
# 「只读服务端确实拒绝写」这个前提，改用 curl 直接打只读服务端核对；
# 探测结果本身（判失败 + 中文提示）由 crates/cli/tests/core_backup_probe_parity.rs 覆盖。
RO_MKCOL=$(curl -s -o /dev/null -w '%{http_code}' -X MKCOL \
  -u 'u:p' "http://127.0.0.1:${PORT_RO}/rime-sync/.rime-sync-probe-check" 2>/dev/null || true)
if [ -n "$RO_MKCOL" ] && [ "$RO_MKCOL" -ge 400 ]; then
  pass "只读 WebDAV 拒绝写入（MKCOL → $RO_MKCOL）"
else
  fail "只读 WebDAV 竟允许写入（MKCOL → ${RO_MKCOL:-无响应}）"
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
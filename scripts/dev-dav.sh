#!/usr/bin/env bash
# dev-dav.sh — 本地 WebDAV 测试服务（rclone serve webdav）
#
# 用法:
#   ./scripts/dev-dav.sh [目录]
#   DAV_ADDR=127.0.0.1:18080 DAV_USER=u DAV_PASS=p ./scripts/dev-dav.sh /tmp/dav-root
#
# 参数/环境变量:
#   $1              对外暴露的根目录（默认 <当前目录>/dev-dav-root，不存在会自动创建）
#   DAV_ADDR        监听地址（默认 127.0.0.1:18080）
#   DAV_USER/DAV_PASS   HTTP Basic 账号（默认 u / p）
#   DAV_READ_ONLY   =1 时以只读方式启动（默认可读写）
#
# 脚本在前台运行，Ctrl-C 退出；EXIT/INT/TERM 都会 kill 掉 rclone 子进程。
# 冒烟检查:
#   curl -s -o /dev/null -w '%{http_code}\n' -X PROPFIND -u u:p http://127.0.0.1:18080/
set -euo pipefail

ROOT="${1:-$PWD/dev-dav-root}"
ADDR="${DAV_ADDR:-127.0.0.1:18080}"
DAV_USER="${DAV_USER:-u}"
DAV_PASS="${DAV_PASS:-p}"
READ_ONLY="${DAV_READ_ONLY:-0}"

command -v rclone >/dev/null 2>&1 || { echo "dev-dav: 需要 rclone（apt install rclone）" >&2; exit 127; }

mkdir -p "$ROOT"
ROOT="$(cd "$ROOT" && pwd)"

# 端口预检：避免把已存在的旧实例误判成"本次启动成功"
if command -v ss >/dev/null 2>&1 && ss -ltn 2>/dev/null | grep -q ":${ADDR##*:} "; then
  echo "dev-dav: ${ADDR} 已被占用，换端口：DAV_ADDR=127.0.0.1:18081 $0" >&2
  exit 1
fi

EXTRA=()
[ "$READ_ONLY" = "1" ] && EXTRA+=(--read-only)

PID=""
cleanup() {
  local rc=$?
  if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
    kill "$PID" 2>/dev/null || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      kill -0 "$PID" 2>/dev/null || break
      sleep 0.2
    done
    kill -9 "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  exit "$rc"
}
trap cleanup EXIT INT TERM

echo "dev-dav: 根目录 $ROOT"
echo "dev-dav: 监听 http://$ADDR/  账号 $DAV_USER / $DAV_PASS"

rclone serve webdav "$ROOT" \
  --addr "$ADDR" \
  --user "$DAV_USER" \
  --pass "$DAV_PASS" \
  --log-level INFO \
  "${EXTRA[@]}" &

PID=$!

# 等端口就绪（最多约 10s），便于脚本化调用
READY=0
for _ in $(seq 1 50); do
  if ! kill -0 "$PID" 2>/dev/null; then
    echo "dev-dav: rclone 提前退出（端口占用/参数错误，见上方日志）" >&2
    exit 1
  fi
  if curl -s -o /dev/null -u "$DAV_USER:$DAV_PASS" "http://$ADDR/" 2>/dev/null; then
    READY=1
    break
  fi
  sleep 0.2
done
[ "$READY" = "1" ] || echo "dev-dav: 警告：10s 内未探测到就绪，继续等待 rclone" >&2

echo "dev-dav: 就绪。示例:"
echo "  curl -s -o /dev/null -w '%{http_code}' -X PROPFIND -u $DAV_USER:$DAV_PASS http://$ADDR/"

set +e
wait "$PID"
RC=$?
set -e
exit "$RC"

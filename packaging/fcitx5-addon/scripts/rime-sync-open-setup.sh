#!/bin/sh
# fcitx5 配置页里「打开 Rime 同步（WebDAV 设置向导）」按钮执行的命令。
# fcitx5-config-qt 用 QProcess::startDetached 启动它（PATH 查找）。
set -u

if [ -n "${RIME_SYNC_BIN:-}" ] && [ -x "${RIME_SYNC_BIN}" ]; then
    exec "${RIME_SYNC_BIN}" --setup
fi

for candidate in rime-sync-app /usr/bin/rime-sync-app /usr/local/bin/rime-sync-app; do
    if command -v "$candidate" >/dev/null 2>&1; then
        exec "$candidate" --setup
    fi
done

if command -v notify-send >/dev/null 2>&1; then
    notify-send "Rime 同步" "找不到 rime-sync-app；请安装 rime-sync 或用 RIME_SYNC_BIN 指定路径。"
fi
echo "rime-sync-open-setup: rime-sync-app not found; set RIME_SYNC_BIN" >&2
exit 1

#!/bin/sh
# fcitx5 配置页里「打开 Rime 同步」按钮执行的命令。
# fcitx5-config-qt 用 QProcess::startDetached 启动它（PATH 查找）。
#
# 传 --panel sync：主程序已在运行时（多半缩在托盘里），把这个请求转给它，
# 由它切到「同步」页并把窗口带到前台 —— 输入法里的入口与主程序是同一个界面。
# 同时保留 --setup：未配置 WebDAV 的用户点这个按钮时仍要看到设置向导。
set -u

if [ -n "${RIME_SYNC_BIN:-}" ] && [ -x "${RIME_SYNC_BIN}" ]; then
    exec "${RIME_SYNC_BIN}" --panel sync --setup
fi

for candidate in rime-sync-app /usr/bin/rime-sync-app /usr/local/bin/rime-sync-app; do
    if command -v "$candidate" >/dev/null 2>&1; then
        exec "$candidate" --panel sync --setup
    fi
done

if command -v notify-send >/dev/null 2>&1; then
    notify-send "Rime 同步" "找不到 rime-sync-app；请安装 rime-sync 或用 RIME_SYNC_BIN 指定路径。"
fi
echo "rime-sync-open-setup: rime-sync-app not found; set RIME_SYNC_BIN" >&2
exit 1

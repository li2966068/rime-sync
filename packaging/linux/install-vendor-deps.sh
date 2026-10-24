#!/bin/bash
# install-vendor-deps.sh —— 由 rime-sync 的 postinst 以脱离进程方式启动。
# 职责：把 fcitx5 + fcitx5-rime + librime-data 装上。
#   1) 有网：优先 apt-get（仓库版本与本机发行版匹配，最干净）
#   2) 无网：校验 /usr/share/rime-sync/vendor/VENDOR-INFO 与 /etc/os-release 后
#      dpkg -i 自带的 .deb 闭包
# 全程重试等 dpkg 锁（postinst 里直接装会被锁拒绝），失败也绝不 exit 非零打断安装。
set -u -o pipefail

VENDOR=${VENDOR:-/usr/share/rime-sync/vendor}
MAX_WAIT=${MAX_WAIT:-300}
export DEBIAN_FRONTEND=noninteractive

log() { printf '%s [rime-sync/vendor] %s\n' "$(date '+%F %T')" "$*"; }

im_installed() {
    [ "$(dpkg-query -W -f='${Status}' fcitx5-rime 2>/dev/null || true)" = "install ok installed" ]
}

# 等锁：执行命令，若因 dpkg 锁失败则退避重试，直到 MAX_WAIT 秒。
run_when_unlocked() {
    local deadline errf rc
    deadline=$(( $(date +%s) + MAX_WAIT ))
    errf=$(mktemp)
    while :; do
        if "$@" 2>&1 | tee "$errf"; then
            rm -f "$errf"
            return 0
        fi
        rc=${PIPESTATUS[0]}
        if grep -qi 'lock' "$errf" 2>/dev/null; then
            if [ "$(date +%s)" -ge "$deadline" ]; then
                log "等待 dpkg 锁超时（${MAX_WAIT}s），放弃：$*"
                rm -f "$errf"
                return 1
            fi
            sleep 2
            continue
        fi
        log "命令失败（rc=$rc）：$*"
        cat "$errf" 2>/dev/null
        rm -f "$errf"
        return 1
    done
}

os_field() { # $1=ID|VERSION_ID
    sed -n "s/^$1=//p" /etc/os-release 2>/dev/null | tr -d '"' | head -1
}

vendor_matches_os() {
    local vi="$VENDOR/VENDOR-INFO"
    if [ ! -r "$vi" ]; then
        log "缺少 $vi，无法确认离线包对应的发行版"
        return 1
    fi
    local vd vv od ov
    vd=$(sed -n 's/^distro=//p' "$vi" | head -1)
    vv=$(sed -n 's/^version_id=//p' "$vi" | head -1)
    od=$(os_field ID); ov=$(os_field VERSION_ID)
    log "离线包: distro=${vd:-?} version_id=${vv:-?}；本机: distro=${od:-?} version_id=${ov:-?}"
    if [ -n "$vv" ] && [ "$vv" != "$ov" ]; then
        log "版本不一致：离线 .deb 是为 ${vv} 打的，本机是 ${ov:-未知}；跳过离线安装（不冒险混装）"
        return 1
    fi
    if [ -n "$vd" ] && [ -n "$od" ] && [ "$vd" != "$od" ]; then
        log "发行版不一致（$vd vs $od），跳过离线安装"
        return 1
    fi
    return 0
}

verify_checksums() {
    [ -r "$VENDOR/SHA256SUMS" ] || { log "没有 SHA256SUMS，跳过校验"; return 0; }
    ( cd "$VENDOR" && sha256sum -c --quiet SHA256SUMS ) || {
        log "离线 .deb 校验失败（文件损坏或被改过），中止离线安装"
        return 1
    }
    log "离线 .deb sha256 校验通过"
}

net_reachable() {
    command -v getent >/dev/null 2>&1 || return 1
    timeout 8 getent hosts archive.ubuntu.com >/dev/null 2>&1 \
        || timeout 8 getent hosts deb.debian.org >/dev/null 2>&1
}

main() {
    log "开始检查输入法依赖（vendor=$VENDOR）"
    if im_installed; then
        log "fcitx5-rime 已安装，无事可做"
        return 0
    fi

    if net_reachable; then
        log "检测到网络，尝试 apt-get 安装 fcitx5 fcitx5-rime librime-data"
        if run_when_unlocked timeout 300 apt-get install -y --no-install-recommends \
                -o DPkg::Lock::Timeout=60 \
                -o Acquire::http::Timeout=20 -o Acquire::Retries=1 \
                fcitx5 fcitx5-rime librime-data; then
            im_installed && { log "apt 安装成功"; return 0; }
        fi
        log "apt 安装未成功，转用自带离线包"
    else
        log "未检测到网络（或 DNS 不可用），使用自带离线包"
    fi

    if ! compgen -G "$VENDOR/*.deb" >/dev/null; then
        log "$VENDOR 里没有 .deb，无法离线安装"
        return 1
    fi
    vendor_matches_os || return 1
    verify_checksums || return 1

    # 只装"本机没有或版本更旧"的包：dpkg 不允许降级，硬装整份闭包
    # 会在已经打全安全更新的 22.04 上失败（例如 libc6 已比自带的新）。
    # 多架构系统上更保守：dpkg 里已有这个包（任一架构）就不碰。否则把 amd64 的
    # libbrotli1 升到离线包版本、i386 还停在旧版时，dpkg 会拒绝配置整条依赖链
    # （cannot be configured because libbrotli1:i386 is at a different version）。
    local -a debs=() foreign=()
    local d pkg ver inst any
    mapfile -t foreign < <(dpkg --print-foreign-architectures 2>/dev/null || true)
    for d in "$VENDOR"/*.deb; do
        pkg=$(dpkg-deb -f "$d" Package 2>/dev/null) || continue
        ver=$(dpkg-deb -f "$d" Version 2>/dev/null) || continue
        any=$(dpkg-query -W -f='${Package}' "$pkg" 2>/dev/null | head -1 || true)
        inst=$(dpkg-query -W -f='${Version}' "$pkg" 2>/dev/null | head -1 || true)
        if [ "${#foreign[@]}" -gt 0 ] && [ -n "$any" ]; then
            continue
        fi
        if [ -n "$inst" ] && dpkg --compare-versions "$inst" ge "$ver" 2>/dev/null; then
            continue
        fi
        debs+=("$d")
    done
    if [ "${#debs[@]}" -eq 0 ]; then
        log "自带的 .deb 都已由系统提供（版本不旧于自带），无需安装"
        return 0
    fi

    log "dpkg -i 离线依赖（${#debs[@]} 个包，其余 $(compgen -G "$VENDOR/*.deb" | wc -l) 个系统已有）"
    run_when_unlocked dpkg -i --force-confdef --force-confold "${debs[@]}" || return 1

    if im_installed; then
        log "离线安装成功：fcitx5-rime 已就绪"
    else
        log "dpkg 已执行但 fcitx5-rime 仍未就绪，请手工执行 apt-get -f install 排查"
        return 1
    fi
}

main
log "worker 退出"
exit 0

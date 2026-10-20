#include "rimesyncaddon.h"

void RimeSyncAddon::reloadConfig() {
    // 配置真源是 ~/.config/rime-sync/config.json，由 rime-sync 主程序在每次
    // transport/apply 前读取；addon 侧不需要缓存，因此这里只保留扩展点。
}

fcitx::AddonInstance *RimeSyncAddonFactory::create(fcitx::AddonManager *manager) {
    (void)manager;
    return new RimeSyncAddon();
}

FCITX_ADDON_FACTORY_V2(rimesync, RimeSyncAddonFactory)

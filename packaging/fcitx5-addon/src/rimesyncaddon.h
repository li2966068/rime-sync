#pragma once

#include <fcitx/addonfactory.h>
#include <fcitx/addoninstance.h>
#include <fcitx/addonmanager.h>

// 最小 fcitx5 addon：只负责把 rime-sync 暴露给 fcitx5 的 addon 表，
// 配置界面由同仓库的 rime-sync-config 提供（见 README 的“为什么不是内嵌配置页”）。
class RimeSyncAddon : public fcitx::AddonInstance {
public:
    RimeSyncAddon() = default;
    ~RimeSyncAddon() override = default;

    void reloadConfig() override;
};

class RimeSyncAddonFactory : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override;
};

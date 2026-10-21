#include "rimesyncaddon.h"

#include <fcitx-utils/log.h>

#include <QString>

#include "rimesyncstore.h"

namespace {

QString configPath() { return rimeSyncConfigPath(); }

} // namespace

RimeSyncAddon::RimeSyncAddon() { reloadConfig(); }

void RimeSyncAddon::reloadConfig() {
    const QString path = configPath();
    const RimeSyncWebDavSettings settings = loadRimeSyncSettings(path);

    config_.webdavUrl.setValue(settings.url.toStdString());
    config_.webdavUsername.setValue(settings.username.toStdString());
    config_.remoteDir.setValue(settings.remoteDir.toStdString());
    config_.deviceId.setValue(settings.deviceId.toStdString());
    config_.stopFcitx5DuringApply.setValue(settings.stopFcitx5DuringApply);
    // 密码从不从文件读回，配置页里始终是空的。
    config_.webdavPassword.setValue(std::string());

    FCITX_INFO() << "rime-sync: loaded WebDAV settings from" << path.toStdString();
}

void RimeSyncAddon::setConfig(const fcitx::RawConfig &raw) {
    config_.load(raw, true);

    RimeSyncWebDavSettings settings;
    settings.url = QString::fromStdString(config_.webdavUrl.value());
    settings.username = QString::fromStdString(config_.webdavUsername.value());
    settings.password = QString::fromStdString(config_.webdavPassword.value());
    settings.remoteDir = QString::fromStdString(config_.remoteDir.value());
    settings.deviceId = QString::fromStdString(config_.deviceId.value());
    settings.stopFcitx5DuringApply = config_.stopFcitx5DuringApply.value();

    const QString path = configPath();
    QString error;
    if (!saveRimeSyncSettings(path, settings, &error)) {
        FCITX_ERROR() << "rime-sync: failed to save" << path.toStdString()
                      << error.toStdString();
    } else {
        FCITX_INFO() << "rime-sync: saved WebDAV settings to" << path.toStdString();
    }

    // 密码只用于这一次写盘，不在内存和配置页里保留。
    config_.webdavPassword.setValue(std::string());
}

fcitx::AddonInstance *RimeSyncAddonFactory::create(fcitx::AddonManager *manager) {
    (void)manager;
    return new RimeSyncAddon();
}

#if defined(FCITX_ADDON_FACTORY_V2)
FCITX_ADDON_FACTORY_V2(rimesync, RimeSyncAddonFactory)
#else
// fcitx5 5.0.x 只有 V1；本 addon 一个 .so 只导出一个工厂，符号一致可用。
FCITX_ADDON_FACTORY(RimeSyncAddonFactory)
#endif

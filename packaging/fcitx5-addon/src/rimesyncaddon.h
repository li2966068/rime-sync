#pragma once

#include <string>

#include <fcitx-config/configuration.h>
#include <fcitx-config/option.h>
#include <fcitx-utils/i18n.h>
#include <fcitx/addonfactory.h>
#include <fcitx/addoninstance.h>

/// 密码选项的注解：skipSave() 让 fcitx5 保存 addon 配置时跳过它，
/// 因此 ~/.config/fcitx5/conf/rimesync.conf 里永远不会有明文密码；
/// 选项本身仍然出现在配置页（页面描述来自 dumpDescription，与 skipSave 无关）。
struct RimeSyncSecretAnnotation {
    bool skipDescription() const { return false; }
    bool skipSave() const { return true; }
    void dumpDescription(fcitx::RawConfig &config) const {
        config.setValueByPath(
            "Tooltip",
            _("密码不写进 fcitx 的配置文件，只写进 ~/.config/rime-sync/config.json（0600），"
              "由 rime-sync 主程序下次运行时迁移进系统钥匙串并从该文件删除。"
              "留空表示不修改已保存的密码。"));
    }
};

/// fcitx5-config-qt 通过 DBus 把这份 Configuration 渲染成配置页：
/// 字符串 → 单行输入框，布尔 → 复选框，External → 一个「配置」按钮。
FCITX_CONFIGURATION(
    RimeSyncAddonConfig,
    fcitx::Option<std::string> webdavUrl{this, "WebDAVURL", _("WebDAV 地址"), ""};
    fcitx::Option<std::string> webdavUsername{this, "WebDAVUsername", _("用户名"), ""};
    fcitx::OptionWithAnnotation<std::string, RimeSyncSecretAnnotation> webdavPassword{
        this, "WebDAVPassword", _("密码（留空=不修改）"), ""};
    fcitx::Option<std::string> remoteDir{this, "RemoteDir", _("远端目录"), "rime-sync"};
    fcitx::Option<std::string> deviceId{
        this, "DeviceId", _("设备名 / device_id（留空=自动生成）"), ""};
    fcitx::Option<bool> stopFcitx5DuringApply{
        this, "StopFcitx5DuringApply", _("应用同步时暂停 fcitx5"), true};
    fcitx::ExternalOption openSetup{
        this, "OpenSetup", _("打开 Rime 同步（WebDAV 设置向导）"),
        "rime-sync-open-setup"};);

class RimeSyncAddon : public fcitx::AddonInstance {
public:
    RimeSyncAddon();
    ~RimeSyncAddon() override = default;

    const fcitx::Configuration *getConfig() const override { return &config_; }
    void setConfig(const fcitx::RawConfig &config) override;
    void reloadConfig() override;
    void save() override {}

private:
    RimeSyncAddonConfig config_;
};

class RimeSyncAddonFactory : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override;
};

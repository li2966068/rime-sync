#pragma once

#include <QString>

/// ~/.config/rime-sync/config.json 的 webdav 段 + device_id。
///
/// 与 Rust 主程序（crates/core/src/config.rs）共用同一份文件：读取只取认识的键，
/// 写回时把整份 JSON 读出来改字段，未知键（schema_version、sync_kinds、exclude_globs…）
/// 原样保留，避免 addon 把主程序的配置写丢。
struct RimeSyncWebDavSettings {
    QString url;
    QString username;
    /// 密码：主程序的 WebDavConfig::password 标了 `skip_serializing`，永远不落进它自己
    /// 保存的 config.json；这里只在“用户这次填了密码”时写一次，由主程序下次启动时
    /// migrate_legacy_password() 搬进钥匙串（或 0600 的 secrets.json）并从文件里删掉。
    QString password;
    QString remoteDir; // 默认 "rime-sync"
    QString deviceId;
    bool stopFcitx5DuringApply = true;
};

/// $XDG_CONFIG_HOME/rime-sync/config.json，缺省 ~/.config/rime-sync/config.json。
QString rimeSyncConfigPath();

RimeSyncWebDavSettings loadRimeSyncSettings(const QString &path);

/// QSaveFile 原子替换 + chmod 0600。password 为空表示“不改动已保存的密码”。
bool saveRimeSyncSettings(const QString &path, const RimeSyncWebDavSettings &settings,
                          QString *error);

#include "rimesyncstore.h"

#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonDocument>
#include <QJsonObject>
#include <QSaveFile>

namespace {

QString trimmed(const QString &value) { return value.trimmed(); }

QJsonObject readRoot(const QString &path, bool *ok = nullptr) {
    if (ok) {
        *ok = true;
    }
    QFile file(path);
    if (!file.open(QIODevice::ReadOnly)) {
        return {};
    }
    const QByteArray text = file.readAll();
    if (text.trimmed().isEmpty()) {
        return {};
    }
    const QJsonDocument doc = QJsonDocument::fromJson(text);
    if (!doc.isObject()) {
        if (ok) {
            *ok = false;
        }
        return {};
    }
    return doc.object();
}

bool fail(QString *error, const QString &message) {
    if (error) {
        *error = message;
    }
    return false;
}

} // namespace

QString rimeSyncConfigPath() {
    const QString xdg = qEnvironmentVariable("XDG_CONFIG_HOME").trimmed();
    const QDir base = xdg.isEmpty() ? QDir(QDir::homePath() + QStringLiteral("/.config"))
                                    : QDir(xdg);
    return base.filePath(QStringLiteral("rime-sync/config.json"));
}

RimeSyncWebDavSettings loadRimeSyncSettings(const QString &path) {
    RimeSyncWebDavSettings settings;
    settings.remoteDir = QStringLiteral("rime-sync");

    const QJsonObject root = readRoot(path);
    if (root.isEmpty()) {
        return settings;
    }
    const QJsonObject dav = root.value(QStringLiteral("webdav")).toObject();
    settings.url = dav.value(QStringLiteral("url")).toString();
    settings.username = dav.value(QStringLiteral("username")).toString();
    // 故意不读 password：正常情况下文件里没有它；有也是主程序待迁移的遗留明文，
    // 没必要再显示到界面上。
    const QString remote = dav.value(QStringLiteral("remote_dir")).toString();
    if (!remote.isEmpty()) {
        settings.remoteDir = remote;
    }
    settings.deviceId = root.value(QStringLiteral("device_id")).toString();
    const QJsonValue stop = root.value(QStringLiteral("stop_fcitx5_during_apply"));
    if (stop.isBool()) {
        settings.stopFcitx5DuringApply = stop.toBool();
    }
    return settings;
}

bool saveRimeSyncSettings(const QString &path, const RimeSyncWebDavSettings &settings,
                          QString *error) {
    const QFileInfo info(path);
    const QDir dir = info.dir();
    if (!dir.exists() && !QDir().mkpath(dir.absolutePath())) {
        return fail(error, QStringLiteral("无法创建目录 %1").arg(dir.absolutePath()));
    }

    // 解析失败就别改：宁可报错，也不要把主程序的配置覆盖成 {}
    bool parsed = true;
    QJsonObject root = readRoot(path, &parsed);
    if (!parsed) {
        return fail(error, QStringLiteral("%1 不是合法的 JSON 对象，已放弃写入").arg(path));
    }
    QJsonObject dav = root.value(QStringLiteral("webdav")).toObject();

    QString url = trimmed(settings.url);
    if (!url.isEmpty() && !url.endsWith(QLatin1Char('/'))) {
        url.append(QLatin1Char('/'));
    }
    dav.insert(QStringLiteral("url"), url);
    dav.insert(QStringLiteral("username"), trimmed(settings.username));
    // 空密码 = 保持原样（文件里本来也不该有密码）。
    if (!settings.password.isEmpty()) {
        dav.insert(QStringLiteral("password"), settings.password);
    }
    QString remote = trimmed(settings.remoteDir);
    if (remote.isEmpty()) {
        remote = QStringLiteral("rime-sync");
    }
    dav.insert(QStringLiteral("remote_dir"), remote);
    root.insert(QStringLiteral("webdav"), dav);

    const QString deviceId = trimmed(settings.deviceId);
    if (!deviceId.isEmpty()) {
        root.insert(QStringLiteral("device_id"), deviceId);
    }
    root.insert(QStringLiteral("stop_fcitx5_during_apply"), settings.stopFcitx5DuringApply);

    QSaveFile file(path);
    if (!file.open(QIODevice::WriteOnly)) {
        return fail(error, QStringLiteral("无法写入 %1：%2").arg(path, file.errorString()));
    }
    file.write(QJsonDocument(root).toJson(QJsonDocument::Indented));
    if (!file.commit()) {
        return fail(error, QStringLiteral("写入未完成 %1：%2").arg(path, file.errorString()));
    }
    // 可能含密码，也可能含其他主程序密钥相关的键：只给属主读写。
    QFile::setPermissions(path, QFileDevice::ReadOwner | QFileDevice::WriteOwner);
    return true;
}

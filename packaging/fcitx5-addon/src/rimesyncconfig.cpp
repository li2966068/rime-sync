#include "rimesyncconfig.h"

#include <QDialogButtonBox>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QFormLayout>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLabel>
#include <QLineEdit>
#include <QMessageBox>
#include <QProcess>
#include <QPushButton>
#include <QSaveFile>
#include <QStandardPaths>
#include <QVBoxLayout>

namespace {

// 与 rime-sync 主程序一致：$XDG_CONFIG_HOME/rime-sync/config.json
QString configFilePath() {
    const QString base = QStandardPaths::writableLocation(QStandardPaths::GenericConfigLocation);
    return QDir(base).filePath(QStringLiteral("rime-sync/config.json"));
}

QJsonObject readConfig(const QString &path) {
    QFile file(path);
    if (!file.open(QIODevice::ReadOnly)) {
        return {};
    }
    const QJsonDocument doc = QJsonDocument::fromJson(file.readAll());
    return doc.isObject() ? doc.object() : QJsonObject{};
}

} // namespace

RimeSyncConfigDialog::RimeSyncConfigDialog(QWidget *parent) : QDialog(parent) {
    setWindowTitle(QStringLiteral("Rime 同步 · WebDAV 配置"));
    setMinimumWidth(460);

    urlEdit_ = new QLineEdit(this);
    urlEdit_->setPlaceholderText(QStringLiteral("https://dav.jianguoyun.com/dav/"));
    userEdit_ = new QLineEdit(this);
    userEdit_->setPlaceholderText(QStringLiteral("账号（多为邮箱）"));
    passEdit_ = new QLineEdit(this);
    passEdit_->setEchoMode(QLineEdit::Password);
    passEdit_->setPlaceholderText(QStringLiteral("应用专用密码"));
    remoteDirEdit_ = new QLineEdit(this);
    remoteDirEdit_->setPlaceholderText(QStringLiteral("rime-sync"));

    auto *form = new QFormLayout;
    form->addRow(QStringLiteral("WebDAV 地址"), urlEdit_);
    form->addRow(QStringLiteral("用户名"), userEdit_);
    form->addRow(QStringLiteral("密码"), passEdit_);
    form->addRow(QStringLiteral("远端目录"), remoteDirEdit_);

    auto *hint = new QLabel(
        QStringLiteral("地址通常以 / 结尾；AList 请填 http://主机:5244/dav，并确认已开启 "
                       "Webdav Read / Webdav Manage 权限。"),
        this);
    hint->setWordWrap(true);

    statusLabel_ = new QLabel(this);
    statusLabel_->setWordWrap(true);

    auto *buttons = new QDialogButtonBox(this);
    auto *saveButton = buttons->addButton(QStringLiteral("保存"), QDialogButtonBox::AcceptRole);
    auto *openButton =
        buttons->addButton(QStringLiteral("打开 Rime 同步"), QDialogButtonBox::ActionRole);
    buttons->addButton(QStringLiteral("取消"), QDialogButtonBox::RejectRole);

    connect(saveButton, &QPushButton::clicked, this, &RimeSyncConfigDialog::save);
    connect(openButton, &QPushButton::clicked, this, &RimeSyncConfigDialog::saveAndOpen);
    connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::reject);

    auto *layout = new QVBoxLayout(this);
    layout->addLayout(form);
    layout->addWidget(hint);
    layout->addWidget(statusLabel_);
    layout->addWidget(buttons);

    load();
}

void RimeSyncConfigDialog::load() {
    const QString path = configFilePath();
    const QJsonObject root = readConfig(path);
    const QJsonObject dav = root.value(QStringLiteral("webdav")).toObject();

    urlEdit_->setText(dav.value(QStringLiteral("url")).toString());
    userEdit_->setText(dav.value(QStringLiteral("username")).toString());
    passEdit_->setText(dav.value(QStringLiteral("password")).toString());
    const QString remote = dav.value(QStringLiteral("remote_dir")).toString();
    remoteDirEdit_->setText(remote.isEmpty() ? QStringLiteral("rime-sync") : remote);
    setStatus(QStringLiteral("配置文件：%1").arg(path));
}

bool RimeSyncConfigDialog::saveToDisk() {
    const QString path = configFilePath();
    const QDir dir = QFileInfo(path).dir();
    if (!dir.exists() && !QDir().mkpath(dir.absolutePath())) {
        QMessageBox::critical(this, QStringLiteral("保存失败"),
                              QStringLiteral("无法创建目录：%1").arg(dir.absolutePath()));
        return false;
    }

    QJsonObject root = readConfig(path);
    QJsonObject dav = root.value(QStringLiteral("webdav")).toObject();

    QString url = urlEdit_->text().trimmed();
    if (!url.isEmpty() && !url.endsWith(QLatin1Char('/'))) {
        url.append(QLatin1Char('/'));
    }
    dav.insert(QStringLiteral("url"), url);
    dav.insert(QStringLiteral("username"), userEdit_->text().trimmed());
    // 留空表示不修改已保存的密码，避免每次打开配置页都要重输。
    const QString password = passEdit_->text();
    if (!password.isEmpty()) {
        dav.insert(QStringLiteral("password"), password);
    } else if (!dav.contains(QStringLiteral("password"))) {
        dav.insert(QStringLiteral("password"), QString());
    }
    QString remote = remoteDirEdit_->text().trimmed();
    if (remote.isEmpty()) {
        remote = QStringLiteral("rime-sync");
    }
    dav.insert(QStringLiteral("remote_dir"), remote);
    root.insert(QStringLiteral("webdav"), dav);

    // QSaveFile 原子写；随后收紧到 0600（含密码，不能给同机其他用户读）。
    QSaveFile file(path);
    if (!file.open(QIODevice::WriteOnly)) {
        QMessageBox::critical(this, QStringLiteral("保存失败"),
                              QStringLiteral("无法写入：%1").arg(path));
        return false;
    }
    file.write(QJsonDocument(root).toJson(QJsonDocument::Indented));
    if (!file.commit()) {
        QMessageBox::critical(this, QStringLiteral("保存失败"),
                              QStringLiteral("写入未完成：%1").arg(path));
        return false;
    }
    QFile::setPermissions(path, QFileDevice::ReadOwner | QFileDevice::WriteOwner);
    setStatus(QStringLiteral("已保存到 %1").arg(path));
    return true;
}

bool RimeSyncConfigDialog::launchApp() {
    QString program = qEnvironmentVariable("RIME_SYNC_BIN");
    if (program.isEmpty()) {
        program = QStandardPaths::findExecutable(QStringLiteral("rime-sync"));
    }
    if (program.isEmpty()) {
        program = QStringLiteral("/usr/bin/rime-sync");
    }
    if (!QFileInfo::exists(program)) {
        QMessageBox::warning(
            this, QStringLiteral("找不到 rime-sync"),
            QStringLiteral("请先安装 rime-sync，或用环境变量 RIME_SYNC_BIN 指定可执行文件路径。"));
        return false;
    }
    return QProcess::startDetached(program, QStringList{});
}

void RimeSyncConfigDialog::save() {
    if (saveToDisk()) {
        QMessageBox::information(this, QStringLiteral("已保存"),
                                 QStringLiteral("WebDAV 配置已写入 rime-sync。"));
        accept();
    }
}

void RimeSyncConfigDialog::saveAndOpen() {
    if (!saveToDisk()) {
        return;
    }
    if (launchApp()) {
        accept();
    }
}

void RimeSyncConfigDialog::setStatus(const QString &text) { statusLabel_->setText(text); }

#include "rimesyncconfig.h"

#include <QCheckBox>
#include <QDialogButtonBox>
#include <QFileInfo>
#include <QFormLayout>
#include <QLabel>
#include <QLineEdit>
#include <QMessageBox>
#include <QProcess>
#include <QPushButton>
#include <QStandardPaths>
#include <QVBoxLayout>

#include "rimesyncstore.h"

RimeSyncConfigDialog::RimeSyncConfigDialog(QWidget *parent) : QDialog(parent) {
    setWindowTitle(QStringLiteral("Rime 同步 · WebDAV 配置"));
    setMinimumWidth(460);

    urlEdit_ = new QLineEdit(this);
    urlEdit_->setPlaceholderText(QStringLiteral("https://dav.jianguoyun.com/dav/"));
    userEdit_ = new QLineEdit(this);
    userEdit_->setPlaceholderText(QStringLiteral("账号（多为邮箱）"));
    passEdit_ = new QLineEdit(this);
    passEdit_->setEchoMode(QLineEdit::Password);
    passEdit_->setPlaceholderText(QStringLiteral("留空 = 不修改已保存的密码"));
    remoteDirEdit_ = new QLineEdit(this);
    remoteDirEdit_->setPlaceholderText(QStringLiteral("rime-sync"));
    deviceIdEdit_ = new QLineEdit(this);
    deviceIdEdit_->setPlaceholderText(QStringLiteral("留空 = 由主程序按 Rime 安装自动生成"));
    stopFcitx5Check_ = new QCheckBox(QStringLiteral("应用同步时暂停 fcitx5"), this);

    auto *form = new QFormLayout;
    form->addRow(QStringLiteral("WebDAV 地址"), urlEdit_);
    form->addRow(QStringLiteral("用户名"), userEdit_);
    form->addRow(QStringLiteral("密码"), passEdit_);
    form->addRow(QStringLiteral("远端目录"), remoteDirEdit_);
    form->addRow(QStringLiteral("设备名 / device_id"), deviceIdEdit_);
    form->addRow(QString(), stopFcitx5Check_);

    auto *hint = new QLabel(
        QStringLiteral("地址通常以 / 结尾（保存时会自动补）；AList 请填 http://主机:5244/dav，"
                       "并确认已开启 Webdav Read / Webdav Manage 权限。\n"
                       "密码写进 config.json（0600）后由主程序迁进系统钥匙串，随后从文件删除。"),
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
    const QString path = rimeSyncConfigPath();
    const RimeSyncWebDavSettings settings = loadRimeSyncSettings(path);

    urlEdit_->setText(settings.url);
    userEdit_->setText(settings.username);
    remoteDirEdit_->setText(settings.remoteDir);
    deviceIdEdit_->setText(settings.deviceId);
    stopFcitx5Check_->setChecked(settings.stopFcitx5DuringApply);
    setStatus(QStringLiteral("配置文件：%1").arg(path));
}

bool RimeSyncConfigDialog::saveToDisk() {
    RimeSyncWebDavSettings settings;
    settings.url = urlEdit_->text();
    settings.username = userEdit_->text();
    settings.password = passEdit_->text();
    settings.remoteDir = remoteDirEdit_->text();
    settings.deviceId = deviceIdEdit_->text();
    settings.stopFcitx5DuringApply = stopFcitx5Check_->isChecked();

    QString error;
    if (!saveRimeSyncSettings(rimeSyncConfigPath(), settings, &error)) {
        QMessageBox::critical(this, QStringLiteral("保存失败"), error);
        return false;
    }
    setStatus(QStringLiteral("已保存到 %1").arg(rimeSyncConfigPath()));
    return true;
}

bool RimeSyncConfigDialog::launchApp() {
    QString program = QStandardPaths::findExecutable(QStringLiteral("rime-sync-open-setup"));
    QStringList args;
    if (program.isEmpty()) {
        program = qEnvironmentVariable("RIME_SYNC_BIN");
        args << QStringLiteral("--setup");
    }
    if (program.isEmpty()) {
        program = QStandardPaths::findExecutable(QStringLiteral("rime-sync-app"));
        args = QStringList{QStringLiteral("--setup")};
    }
    if (program.isEmpty() || !QFileInfo::exists(program)) {
        QMessageBox::warning(
            this, QStringLiteral("找不到 rime-sync"),
            QStringLiteral("请先安装 rime-sync，或用环境变量 RIME_SYNC_BIN 指定可执行文件路径。"));
        return false;
    }
    return QProcess::startDetached(program, args);
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

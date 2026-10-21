#pragma once

#include <QDialog>
#include <QString>

class QCheckBox;
class QLabel;
class QLineEdit;

// 独立配置页（没有 fcitx5-configtool 时的退路）：与 addon 共用 rimesyncstore，
// 写同一份 ~/.config/rime-sync/config.json。
class RimeSyncConfigDialog : public QDialog {
    Q_OBJECT

public:
    explicit RimeSyncConfigDialog(QWidget *parent = nullptr);

private slots:
    void save();
    void saveAndOpen();

private:
    void load();
    bool saveToDisk();
    bool launchApp();
    void setStatus(const QString &text);

    QLineEdit *urlEdit_ = nullptr;
    QLineEdit *userEdit_ = nullptr;
    QLineEdit *passEdit_ = nullptr;
    QLineEdit *remoteDirEdit_ = nullptr;
    QLineEdit *deviceIdEdit_ = nullptr;
    QCheckBox *stopFcitx5Check_ = nullptr;
    QLabel *statusLabel_ = nullptr;
};

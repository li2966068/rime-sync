#pragma once

#include <QDialog>
#include <QString>

class QLabel;
class QLineEdit;

// 读写 ~/.config/rime-sync/config.json 的 webdav 段，与 rime-sync 主程序共享。
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
    QLabel *statusLabel_ = nullptr;
};

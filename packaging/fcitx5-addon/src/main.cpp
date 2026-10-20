// rime-sync-config：fcitx5 侧的配置页入口（独立 Qt Widgets 程序）。
#include <QApplication>

#include "rimesyncconfig.h"

int main(int argc, char **argv) {
    QApplication app(argc, argv);
    QApplication::setApplicationName(QStringLiteral("rime-sync-config"));
    QApplication::setApplicationDisplayName(QStringLiteral("Rime 同步 · 配置"));

    RimeSyncConfigDialog dialog;
    dialog.show();
    return app.exec();
}

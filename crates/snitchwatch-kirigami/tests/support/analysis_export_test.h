#pragma once
#include "snitchwatch-kirigami/src/analysis_export_controller.cxxqt.h"
#include <QCoreApplication>
#include <QQmlApplicationEngine>
#include <QVariant>
#include <QKeyEvent>
#include <QWindow>

namespace snitchwatch::test {
inline AnalysisExportController *findAnalysisExportController(QQmlApplicationEngine &engine) {
    if (engine.rootObjects().isEmpty()) return nullptr;
    return engine.rootObjects().first()->findChild<AnalysisExportController *>(QStringLiteral("analysisExportController"));
}
inline void processAnalysisEvents() { QCoreApplication::processEvents(QEventLoop::AllEvents, 10); }
inline QString analysisProbe(QQmlApplicationEngine &engine, const QString &method) {
    if (engine.rootObjects().isEmpty()) return QStringLiteral("no root");
    auto *root = engine.rootObjects().first();
    auto *window = root->findChild<QWindow *>(QStringLiteral("analysisExportMain"));
    if (!window) window = qobject_cast<QWindow *>(root);
    if (method == QStringLiteral("escape")) {
        QKeyEvent press(QEvent::KeyPress, Qt::Key_Escape, Qt::NoModifier);
        QKeyEvent release(QEvent::KeyRelease, Qt::Key_Escape, Qt::NoModifier);
        QCoreApplication::sendEvent(window, &press);
        QCoreApplication::sendEvent(window, &release);
        return QStringLiteral("ok");
    }
    if (method == QStringLiteral("windowClose")) {
        if (!window) return QStringLiteral("no window");
        window->close();
        return QStringLiteral("ok");
    }
    if (method == QStringLiteral("trayHide")) {
        QObject *toggle = nullptr;
        for (auto *object : root->findChildren<QObject *>()) {
            if (object->property("text").toString() == QStringLiteral("Hide window")
                && object->metaObject()->indexOfSignal("triggered()") >= 0) {
                toggle = object;
                break;
            }
        }
        if (!toggle || toggle->property("enabled").toBool()) return QStringLiteral("tray hide stayed enabled");
        QMetaObject::invokeMethod(toggle, "triggered");
        return QStringLiteral("ok");
    }
    QVariant result;
    if (!QMetaObject::invokeMethod(root, method.toUtf8().constData(), Q_RETURN_ARG(QVariant, result))) return QStringLiteral("method failed");
    return result.toString();
}
inline int analysisHeartbeat(QQmlApplicationEngine &engine) {
    return engine.rootObjects().isEmpty() ? -1 : engine.rootObjects().first()->property("beats").toInt();
}
}

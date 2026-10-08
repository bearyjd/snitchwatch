#pragma once

#include "cxx-qt-lib/qcoreapplication.h"
#include <QtGui/QGuiApplication>
#include <QtWidgets/QApplication>
#include <QtWidgets/QWidget>
#include <memory>

namespace snitchwatch {

inline std::unique_ptr<QGuiApplication>
newWidgetApplication(const QVector<QByteArray>& args)
{
    // Qt retains argc/argv, so transfer their owner to the application only
    // after construction succeeds. QObject's virtual destructor retains the
    // QApplication subclass when Rust owns the QGuiApplication base pointer.
    auto argsData = std::make_unique<rust::cxxqtlib1::ApplicationArgsData>(args);
    auto app = std::make_unique<QApplication>(argsData->size(), argsData->data());
    argsData->setParent(app.get());
    argsData.release();
    return app;
}

// A behavioral regression probe: QGuiApplication alone aborts on QWidget.
inline bool widgetApplicationSmoke(const QGuiApplication& app)
{
    if (qobject_cast<const QApplication*>(&app) == nullptr ||
        QCoreApplication::instance() != &app ||
        !QCoreApplication::arguments().contains(QStringLiteral("--snitchwatch-argv-lifetime-probe"))) {
        return false;
    }
    QWidget widget;
    widget.setObjectName(QStringLiteral("snitchwatch-application-probe"));
    return widget.objectName() == QStringLiteral("snitchwatch-application-probe");
}

} // namespace snitchwatch

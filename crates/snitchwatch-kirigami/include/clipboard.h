#pragma once

#include <QtCore/QThread>
#include <QtGui/QClipboard>
#include <QtGui/QGuiApplication>

namespace snitchwatch {

inline void setClipboardText(const QString& text)
{
    auto* app = qobject_cast<QGuiApplication*>(QCoreApplication::instance());
    // Clipboard access belongs to the GUI thread. The QML invokable is called
    // synchronously from the user's copy action on that thread.
    if (app == nullptr || QThread::currentThread() != app->thread()) {
        return;
    }
    app->clipboard()->setText(text, QClipboard::Clipboard);
}

} // namespace snitchwatch

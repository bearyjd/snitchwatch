#pragma once

#include <QtCore/QCoreApplication>
#include <QtCore/QObject>
#include <QtGui/QKeyEvent>

namespace snitchwatch::test {

// QtTest cannot flag a key event as an auto-repeat, and a held key reaches a
// control as exactly that. This filter turns every key event sent with the
// otherwise unused GroupSwitch modifier into the same event, flagged as an
// auto-repeat and without the marker, sent to the same receiver (tests only).
class AutoRepeatKeys : public QObject {
public:
    bool eventFilter(QObject* receiver, QEvent* event) override
    {
        if (event->type() != QEvent::KeyPress && event->type() != QEvent::KeyRelease) {
            return false;
        }
        auto* key = static_cast<QKeyEvent*>(event);
        if (!(key->modifiers() & Qt::GroupSwitchModifier)) {
            return false;
        }
        QKeyEvent repeat(key->type(), key->key(), key->modifiers() & ~Qt::GroupSwitchModifier,
                         key->text(), true, key->count());
        QCoreApplication::sendEvent(receiver, &repeat);
        return true;
    }
};

// Install the filter once; whether it is installed.
inline bool installAutoRepeatKeys()
{
    static AutoRepeatKeys filter;
    QCoreApplication::instance()->installEventFilter(&filter);
    return true;
}

} // namespace snitchwatch::test

#pragma once

#include <QtGui/QAccessible>

namespace snitchwatch::test {

// Turn Qt's accessibility layer on, as an assistive technology connecting
// would: only then do Qt Quick Controls fill in their accessible role and
// name, which the Accessible.name guard reads (tests only).
inline bool activateAccessibility()
{
    QAccessible::setActive(true);
    return QAccessible::isActive();
}

} // namespace snitchwatch::test

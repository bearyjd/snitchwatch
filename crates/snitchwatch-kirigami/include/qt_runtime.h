#pragma once

#include <QtCore/QString>
#include <QtCore/QtGlobal>

namespace snitchwatch {

// The Qt version the process is running against (not the one built with).
inline QString qtRuntimeVersion()
{
    return QString::fromLatin1(qVersion());
}

} // namespace snitchwatch

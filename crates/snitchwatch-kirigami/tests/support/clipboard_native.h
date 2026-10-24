#pragma once

#include <QtGui/QClipboard>
#include <QtGui/QGuiApplication>

namespace snitchwatch::test {

// Read Qt's native clipboard directly: TextEdit::paste() would normalize the
// same control characters as the old writer and conceal data loss.
inline QString clipboardText()
{
    return QGuiApplication::clipboard()->text(QClipboard::Clipboard);
}

} // namespace snitchwatch::test

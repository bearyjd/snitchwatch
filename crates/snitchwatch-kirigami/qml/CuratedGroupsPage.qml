// First-run step "Recommended rules": see CuratedGroupsStep.qml. Pushed by
// main.qml after the silent-mode step, only when the bridge offers groups.
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: page
    title: "Set up Snitchwatch"

    // Injected by main.qml: the CuratedDefaultsModel.
    property CuratedDefaultsModel model
    // Skipped, or finished choosing; main.qml pops this page.
    signal done()
    property alias step: step
    onVisibleChanged: if (!visible && model) model.cancelBroadConsent()

    CuratedGroupsStep {
        id: step
        model: page.model
        onFinished: page.done()
    }
}

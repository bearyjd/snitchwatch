// First-run onboarding step that asks how new connections are treated when
// nobody is asked (see SilentModeChoice.qml). Pushed once by main.qml while
// the persisted choice is still empty; "Continue anyway" is not needed since
// the preselected choice is saved by Continue.
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.Page {
    id: page
    title: "Set up Snitchwatch"

    // Injected by main.qml: the SettingsController.
    property var settings: null
    // The choice was saved; main.qml pops this page.
    signal done()
    // Open the existing recommended background-service rules page.
    signal reviewRecommendedRules()
    property alias choice: choice

    SilentModeChoice {
        id: choice
        anchors.centerIn: parent
        width: Math.min(parent.width - Kirigami.Units.gridUnit * 4, Kirigami.Units.gridUnit * 30)
        settings: page.settings
        onChosen: page.done()
        onReviewRecommendedRules: page.reviewRecommendedRules()
    }
}

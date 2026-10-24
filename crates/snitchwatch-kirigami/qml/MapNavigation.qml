// Pointer handlers observe presses over markers, then take the grab only
// after a drag/pinch is recognized. Ordinary marker clicks remain buttons.
import QtQuick

Item {
    id: navigation
    required property var camera
    property real previousX: 0
    property real previousY: 0
    property real previousPinchScale: 1
    property point previousPinchCenter: Qt.point(0, 0)
    function updatePinch() {
        if (!pinch.active) return;
        camera.panBy(pinch.centroid.position.x - previousPinchCenter.x,
                     pinch.centroid.position.y - previousPinchCenter.y);
        camera.zoomAt(pinch.activeScale / previousPinchScale,
                      pinch.centroid.position.x, pinch.centroid.position.y);
        previousPinchScale = pinch.activeScale;
        previousPinchCenter = pinch.centroid.position;
    }

    WheelHandler {
        target: null
        acceptedDevices: PointerDevice.Mouse | PointerDevice.TouchPad
        onWheel: event => {
            const delta = event.angleDelta.y !== 0 ? event.angleDelta.y : event.pixelDelta.y;
            navigation.camera.zoomAt(Math.pow(2, delta / 480), event.x, event.y);
            event.accepted = true;
            navigation.parent.forceActiveFocus();
        }
    }
    DragHandler {
        id: drag
        target: null
        acceptedButtons: Qt.LeftButton
        acceptedDevices: PointerDevice.Mouse | PointerDevice.TouchScreen | PointerDevice.Stylus
        grabPermissions: PointerHandler.CanTakeOverFromAnything
        onActiveChanged: {
            navigation.previousX = 0;
            navigation.previousY = 0;
            if (active) navigation.parent.forceActiveFocus();
        }
        onActiveTranslationChanged: {
            if (!active) return;
            navigation.camera.panBy(activeTranslation.x - navigation.previousX,
                                    activeTranslation.y - navigation.previousY);
            navigation.previousX = activeTranslation.x;
            navigation.previousY = activeTranslation.y;
        }
    }
    PinchHandler {
        id: pinch
        objectName: "mapPinch"
        target: null
        minimumPointCount: 2
        maximumPointCount: 2
        grabPermissions: PointerHandler.CanTakeOverFromAnything
        onActiveChanged: {
            navigation.previousPinchScale = 1;
            navigation.previousPinchCenter = centroid.position;
            if (active) navigation.parent.forceActiveFocus();
        }
        onActiveScaleChanged: navigation.updatePinch()
        onActiveTranslationChanged: navigation.updatePinch()
    }
}

// Session-only state of the Monitor map band: whether the user collapsed it
// and how tall they dragged it. A singleton so it survives the Connections
// page being re-created by the page stack, but it is never written to
// settings: a fresh start shows the map at its default height.
pragma Singleton
import QtQuick

QtObject {
    property bool collapsed: false
    // Height of the map body in pixels; 0 means "use the default".
    property real bodyHeight: 0
    property var geoScope: ({kind:"",key:"",name:"World",countryKey:"",cityKey:""})
    property var camera: ({ zoom: 1, x: 1800, y: 900 })
}

import QtQuick
import Quickshell
import qs.Commons
import qs.Ui

// Installed by the magpie-rs integration script.
// The executable path is injected as a JSON-escaped QML string.
BarWidget {
    id: root
    moduleName: "magpie-rs.magpie"
    readonly property string magpieExecutable: "__MAGPIE_RS_EXECUTABLE__"
    implicitWidth: button.implicitWidth
    implicitHeight: button.implicitHeight

    BarIconButton {
        id: button
        anchors.fill: parent
        bar: root.bar
        text: String.fromCodePoint(0xf15c6)
        tooltipText: "magpie-rs"
        onPressed: function(mouseButton) {
            Quickshell.execDetached(mouseButton === Qt.RightButton
                ? [root.magpieExecutable, "app"]
                : [root.magpieExecutable, "panel"])
        }
    }
}

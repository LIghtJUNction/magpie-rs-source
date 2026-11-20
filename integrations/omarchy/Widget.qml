import QtQuick
import Quickshell
import qs.Commons
import qs.Ui

// Installed with a JSON-escaped absolute executable path.
BarWidget {
    id: root
    moduleName: "magpie-rs.magpie"
    readonly property string magpieExecutable: "__MAGPIE_RS_EXECUTABLE__"
    implicitWidth: button.implicitWidth
    implicitHeight: button.implicitHeight

    function launch() {
        // The native desktop opens its window with no positional arguments.
        Quickshell.execDetached([root.magpieExecutable])
    }

    BarIconButton {
        id: button
        anchors.fill: parent
        bar: root.bar
        text: String.fromCodePoint(0xf15c6)
        tooltipText: "Magpie"
        onPressed: root.launch()
    }
}

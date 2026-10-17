import QtQuick
import qs.Ui

// One instance per monitor: state lives in the service.
BarWidget {
  id: root
  moduleName: "io.github.langri-sha.ks-fit"

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: "󰖃"
    tooltipText: "KS Fit"
  }
}

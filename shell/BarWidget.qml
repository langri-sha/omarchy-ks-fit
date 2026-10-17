import QtQuick
import qs.Commons
import qs.Ui
import "Model.js" as Model

// One instance per monitor, rebuilt often: state lives in the service. Under
// a third-party replacement bar there is no service, and the glyph stays dim.
// Clicks only open the panel: nothing here drives the belt.
BarWidget {
  id: root
  moduleName: "io.github.langri-sha.ks-fit"

  readonly property var service: bar && bar.shell ? bar.shell.serviceFor(moduleName) : null
  readonly property bool ready: !!service && service.helperStatus === "ready"
  readonly property bool connected: ready && service.connected
  readonly property bool exercising: ready && service.exercising
  readonly property string label: vertical || !service ? "" : service.barText
  readonly property bool demo: !vertical && ready && service.simulated
  readonly property color activeColor: bar ? bar.urgent : Color.accent

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  TextMetrics {
    id: sampleMetrics
    font.family: button.fontFamily
    font.pixelSize: button.fontSize
    text: Model.barSample
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: ""
    labelVisible: false
    hasVisualContent: true
    // Sized from the widest label short of an hour, so the bar doesn't shift
    // every second.
    fixedWidth: root.vertical ? -1 : root.label === "" && !root.demo ? Style.bar.iconSlot
      : Math.ceil(row.implicitWidth - labelText.width
        + (root.label === "" ? 0 : Math.max(sampleMetrics.advanceWidth, labelText.implicitWidth))
        + scaledHorizontalMargin * 2)
    fixedHeight: root.vertical ? Style.bar.iconSlot : -1
    dimmed: !root.connected
    tooltipText: root.service ? root.service.tooltip : "KS Fit — the service is not loaded"

    Row {
      id: row
      anchors.centerIn: parent
      spacing: Style.space(6)

      Text {
        id: glyphText
        textFormat: Text.PlainText
        anchors.verticalCenter: parent.verticalCenter
        text: Model.glyphs.walk
        color: root.exercising ? root.activeColor : button.foreground
        font.family: button.fontFamily
        font.pixelSize: button.fontSize
        renderType: Text.NativeRendering
      }

      Text {
        id: labelText
        textFormat: Text.PlainText
        anchors.verticalCenter: parent.verticalCenter
        visible: root.label !== ""
        width: visible ? implicitWidth : 0
        text: root.label
        color: root.activeColor
        font.family: button.fontFamily
        font.pixelSize: button.fontSize
        renderType: Text.NativeRendering
      }

      Text {
        textFormat: Text.PlainText
        anchors.verticalCenter: parent.verticalCenter
        visible: root.demo
        width: visible ? implicitWidth : 0
        text: "demo"
        color: Qt.darker(button.foreground, 1.4)
        font.family: button.fontFamily
        font.pixelSize: Style.font.caption
        font.bold: true
        renderType: Text.NativeRendering
      }
    }
  }
}

pragma ComponentBehavior: Bound

import QtQuick
import qs.Commons
import "Model.js" as Model

// Column chart from Rectangles, scaled to a nice maximum like the agents
// panel's meters. The column holding "now" takes the accent; zero columns
// keep a hairline so the period's shape stays readable.
Item {
  id: chart

  property var values: []
  property var labels: []
  property int currentIndex: -1
  property int selectedIndex: -1
  property string emptyText: ""
  property color foreground: Color.foreground
  property color accent: Color.accent
  property string fontFamily: Style.font.family

  signal hovered(int index, bool isHovered)

  readonly property var scaled: Model.scaleColumns(values)
  readonly property real labelHeight: Style.font.caption + Style.space(6)
  readonly property real gap: values.length > 12 ? Style.space(2) : Style.space(6)
  readonly property real hairline: Math.max(1, Style.space(1))

  implicitHeight: Style.space(112) + labelHeight

  Item {
    id: plot
    anchors.fill: parent
    anchors.bottomMargin: chart.labelHeight

    Rectangle {
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.bottom: parent.bottom
      height: chart.hairline
      color: Qt.rgba(chart.foreground.r, chart.foreground.g, chart.foreground.b, 0.12)
    }

    Row {
      id: columns
      anchors.fill: parent
      spacing: chart.gap

      Repeater {
        model: chart.scaled

        Item {
          id: column
          required property var modelData
          required property int index
          readonly property real value: modelData.value
          readonly property bool current: index === chart.currentIndex
          readonly property bool selected: index === chart.selectedIndex

          width: (columns.width - columns.spacing * Math.max(0, chart.scaled.length - 1)) / Math.max(1, chart.scaled.length)
          height: columns.height

          Rectangle {
            anchors.bottom: parent.bottom
            width: parent.width
            height: column.value > 0 ? Math.max(Style.space(2), parent.height * column.modelData.fraction) : chart.hairline
            radius: Style.cornerRadius > 0 ? Math.min(Style.space(2), width / 2) : 0
            color: column.current ? Style.selectedStateColor(chart.foreground, chart.accent)
              : Qt.rgba(chart.foreground.r, chart.foreground.g, chart.foreground.b,
                        column.value <= 0 ? 0.25 : column.selected ? 0.8 : 0.45)

            Behavior on height { NumberAnimation { duration: 160; easing.type: Easing.OutCubic } }
          }

          Rectangle {
            visible: column.selected
            anchors.bottom: parent.bottom
            anchors.bottomMargin: -chart.hairline * 3
            anchors.horizontalCenter: parent.horizontalCenter
            width: Math.max(chart.hairline * 3, parent.width * 0.6)
            height: chart.hairline * 2
            color: chart.foreground
          }

          MouseArea {
            anchors.fill: parent
            hoverEnabled: true
            onContainsMouseChanged: chart.hovered(column.index, containsMouse)
          }
        }
      }
    }

    Text {
      textFormat: Text.PlainText
      anchors.centerIn: parent
      visible: chart.emptyText !== ""
      text: chart.emptyText
      color: Qt.darker(chart.foreground, 1.4)
      font.family: chart.fontFamily
      font.pixelSize: Style.font.bodySmall
    }
  }

  Repeater {
    model: chart.labels

    Text {
      required property var modelData
      required property int index
      readonly property real slot: (plot.width - chart.gap * Math.max(0, chart.labels.length - 1)) / Math.max(1, chart.labels.length)

      textFormat: Text.PlainText
      visible: String(modelData) !== ""
      x: index * (slot + chart.gap) + (slot - implicitWidth) / 2
      y: plot.height + Style.space(4)
      text: String(modelData)
      color: index === chart.currentIndex ? chart.foreground : Qt.darker(chart.foreground, 1.4)
      font.family: chart.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: index === chart.currentIndex
    }
  }
}

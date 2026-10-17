pragma ComponentBehavior: Bound

import QtQuick
import qs.Commons
import qs.Ui
import "Model.js" as Model

// The walk in progress: speed, the session's numbers and the controls, with
// Stop the largest thing to hit.
Column {
  id: root

  required property var panel

  readonly property var service: panel.service
  readonly property var snapshot: service ? service.state : null
  readonly property var pad: service ? service.pad : null
  readonly property bool exercising: !!service && service.exercising
  readonly property bool starting: !!pad && (pad.busy === "starting" || pad.belt === "starting")
  readonly property var session: snapshot && snapshot.session ? snapshot.session : ({})
  readonly property var range: Model.speedRange(snapshot)
  readonly property color foreground: panel.foreground
  readonly property color dim: panel.dim
  readonly property string fontFamily: panel.fontFamily

  readonly property var controls: [
    { id: "slower", icon: Model.glyphs.minus, label: "0.5", enabled: range.slower },
    exercising ? { id: "stop", icon: Model.glyphs.stop, label: "Stop", enabled: true }
      : { id: "start", icon: Model.glyphs.play, label: "Start", enabled: Model.beltBlocker("ready", snapshot, "start") === "" },
    { id: "faster", icon: Model.glyphs.plus, label: "0.5", enabled: range.faster }
  ]
  readonly property var sections: [{ name: "speed", count: 3, columns: 3 }]

  function run(control) {
    if (!control || control.enabled === false || !service) return
    switch (control.id) {
      case "slower": service.nudge(-Model.speedStep); break
      case "faster": service.nudge(Model.speedStep); break
      case "stop": service.stop(); break
      case "start": service.start(); break
    }
  }

  function activate(section, index, byArrows) {
    var control = controls[index]
    if (section !== "speed" || !control) return
    if (control.id === "start" && !byArrows) return
    run(control)
  }

  function rescan() {}

  spacing: Style.space(14)

  Item {
    width: parent.width
    height: speedRow.implicitHeight

    Row {
      id: speedRow
      anchors.horizontalCenter: parent.horizontalCenter
      spacing: Style.space(8)

      Text {
        id: speedText
        textFormat: Text.PlainText
        text: root.starting ? "Starting…" : Model.formatSpeed(root.pad ? root.pad.speedKmh : 0)
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: root.starting ? Style.font.display : Style.font.displayLarge * 2
        font.bold: true
      }

      Text {
        textFormat: Text.PlainText
        visible: !root.starting
        anchors.baseline: speedText.baseline
        text: "km/h"
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.title
      }

      Text {
        textFormat: Text.PlainText
        visible: !root.starting && !!root.pad && Math.abs(root.pad.targetKmh - root.pad.speedKmh) >= 0.05
        anchors.baseline: speedText.baseline
        text: "→ " + Model.formatSpeed(root.pad ? root.pad.targetKmh : 0)
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.title
      }
    }
  }

  Grid {
    id: tiles
    width: parent.width
    columns: 4
    spacing: Style.spacing.xs

    readonly property real cell: (width - spacing * (columns - 1)) / columns

    Repeater {
      model: [
        { label: "TIME", value: Model.formatDuration(root.session.activeS) },
        { label: "STEPS", value: Model.formatInt(root.session.steps) },
        { label: "DISTANCE", value: Model.formatKm(root.session.distanceM) },
        { label: "CALORIES", value: Model.formatKcal(root.session.kcal) }
      ]

      Column {
        required property var modelData
        width: tiles.cell
        spacing: Style.space(2)

        Text {
          textFormat: Text.PlainText
          width: parent.width
          horizontalAlignment: Text.AlignHCenter
          text: parent.modelData.value
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.heading
          font.bold: true
          fontSizeMode: Text.HorizontalFit
          minimumPixelSize: Style.font.body
        }

        Text {
          textFormat: Text.PlainText
          width: parent.width
          horizontalAlignment: Text.AlignHCenter
          text: parent.modelData.label
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          font.letterSpacing: 1.2
        }
      }
    }
  }

  Row {
    id: controlRow
    width: parent.width
    spacing: Style.spacing.xs

    readonly property real side: Style.space(80)

    Repeater {
      model: root.controls

      Button {
        required property var modelData
        required property int index
        width: index === 1 ? controlRow.width - (controlRow.side + controlRow.spacing) * 2 : controlRow.side
        iconText: modelData.icon
        text: modelData.label
        tooltipText: modelData.id === "stop" ? "Backspace or 0" : ""
        verticalPadding: Style.spacing.md * 2
        fontSize: index === 1 ? Style.font.title : Style.font.body
        bordered: true
        active: index === 1
        enabled: modelData.enabled !== false
        opacity: enabled ? 1 : 0.4
        hasCursor: root.panel.hasCursor("speed", index)
        foreground: root.foreground
        fontFamily: root.fontFamily
        onHovered: function(on) { root.panel.hoverCursor("speed", index, on) }
        onClicked: root.run(modelData)
      }
    }
  }

  Text {
    textFormat: Text.PlainText
    visible: !!root.snapshot && !!root.snapshot.today
    width: parent.width
    horizontalAlignment: Text.AlignHCenter
    text: root.snapshot && root.snapshot.today ? "Today " + Model.formatSteps(root.snapshot.today.steps) + " · " + Model.todayLine(root.snapshot.today) : ""
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
    elide: Text.ElideRight
  }
}

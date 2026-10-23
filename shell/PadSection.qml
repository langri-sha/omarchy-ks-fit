pragma ComponentBehavior: Bound

import QtQuick
import qs.Commons
import qs.Ui
import "Model.js" as Model

// The pad row under the stats: Bluetooth power, scanning for a pad, and the
// remembered pad with its actions.
Column {
  id: root

  required property var panel

  readonly property var service: panel.service
  readonly property var snapshot: service ? service.state : null
  readonly property color foreground: panel.foreground
  readonly property color dim: panel.dim
  readonly property string fontFamily: panel.fontFamily

  readonly property var actions: Model.padActions(snapshot, service ? service.stoppedAt : 0, service ? service.clock : 0)
  readonly property bool choosing: !!snapshot && snapshot.bluetooth === "on" && !snapshot.device
  property bool showAll: false
  readonly property var scan: Model.scanRows(snapshot, showAll)
  readonly property int scanCount: choosing ? scan.rows.length + (scan.hidden > 0 || showAll ? 1 : 0) : 0

  readonly property var sections: {
    var list = []
    if (actions.length > 0) list.push({ name: "pad", count: actions.length, columns: actions.length })
    if (scanCount > 0) list.push({ name: "scan", count: scanCount, columns: 1 })
    return list
  }

  readonly property string title: {
    if (!snapshot) return ""
    if (snapshot.bluetooth !== "on") return Model.linkText(snapshot)
    if (!snapshot.device) return "No pad selected"
    return snapshot.device.name ? String(snapshot.device.name) : "Unknown pad"
  }
  readonly property string subtitle: {
    if (!snapshot) return ""
    if (snapshot.bluetooth === "off" || snapshot.bluetooth === "blocked") return "KS Fit reaches the pad over Bluetooth."
    if (snapshot.bluetooth !== "on") return ""
    if (!snapshot.device) return snapshot.scan && snapshot.scan.active ? "Looking for pads nearby…"
      : "Switch the pad on and close the KS Fit phone app, then scan."
    if (Model.connected(snapshot)) return Model.exercising(snapshot) ? "Walking" : "Connected"
    return Model.linkText(snapshot)
  }
  readonly property string hint: Model.linkHint(snapshot)

  function run(action) {
    if (!action || action.enabled === false || !service) return
    switch (action.id) {
      case "bluetoothOn": service.bluetoothOn(); break
      case "scan": service.scan(); break
      case "stopScan": service.stopScan(); break
      case "slower": service.nudge(-Model.speedStep); break
      case "faster": service.nudge(Model.speedStep); break
      case "start": service.start(); break
      case "stop": service.stop(); break
      case "disconnect": service.disconnect(); break
      case "connect": service.reconnect(); break
      case "forget": service.forget(); break
    }
  }

  function choose(index) {
    if (index < scan.rows.length) {
      let device = scan.rows[index]
      service.connectTo(device.address, device.name)
    } else {
      showAll = !showAll
    }
  }

  function controlId(section, index) {
    if (section !== "pad") return section + ":" + index
    var action = actions[index]
    return action ? "pad:" + action.id : ""
  }

  // `armed`: arrow keys put the cursor on this very control (PanelContent).
  // Without it Start ignores the keyboard: a resting pointer's hover, or a
  // row that changed under the cursor, must never start the belt.
  function activate(section, index, armed) {
    if (section === "pad") {
      let action = actions[index]
      if (action && action.id === "start" && !armed) return
      run(action)
    } else if (section === "scan") {
      choose(index)
    }
  }

  function rescan() {
    if (choosing && service) service.scan()
  }

  visible: !!snapshot
  spacing: Style.space(10)

  PanelSeparator { foreground: root.foreground }

  Row {
    width: parent.width
    spacing: Style.space(12)

    Text {
      id: icon
      textFormat: Text.PlainText
      anchors.verticalCenter: parent.verticalCenter
      text: !root.snapshot || root.snapshot.bluetooth !== "on" ? Model.glyphs.bluetoothOff
        : root.snapshot.device ? Model.glyphs.walk : Model.glyphs.bluetooth
      color: root.foreground
      opacity: root.service && root.service.connected ? 1 : 0.6
      font.family: root.fontFamily
      font.pixelSize: Style.font.heading
    }

    Column {
      width: parent.width - icon.width - parent.spacing
      anchors.verticalCenter: parent.verticalCenter
      spacing: Style.space(2)

      Text {
        textFormat: Text.PlainText
        width: parent.width
        text: root.title
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
        elide: Text.ElideRight
      }

      Text {
        textFormat: Text.PlainText
        visible: text !== ""
        width: parent.width
        text: root.subtitle
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        wrapMode: Text.WordWrap
      }

      Text {
        textFormat: Text.PlainText
        visible: text !== ""
        width: parent.width
        text: root.hint
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        wrapMode: Text.WordWrap
      }
    }
  }

  Row {
    id: actionRow
    visible: root.actions.length > 0
    width: parent.width
    spacing: Style.spacing.xs

    readonly property real compact: Style.space(40)
    readonly property int labelled: {
      var n = 0
      for (let i = 0; i < root.actions.length; i++) if (root.actions[i].label) n++
      return Math.max(1, n)
    }
    readonly property real wide: (width - spacing * (root.actions.length - 1) - compact * (root.actions.length - labelled)) / labelled

    Repeater {
      model: root.actions

      Button {
        required property var modelData
        required property int index
        width: modelData.label ? actionRow.wide : actionRow.compact
        text: modelData.label || ""
        iconText: modelData.icon || ""
        iconSpinning: modelData.busy === true
        tooltipText: modelData.tooltip || ""
        bordered: true
        active: modelData.primary === true
        enabled: modelData.enabled !== false
        opacity: enabled ? 1 : 0.4
        hasCursor: root.panel.hasCursor("pad", index)
        foreground: root.foreground
        fontFamily: root.fontFamily
        onHovered: function(on) { root.panel.hoverCursor("pad", index, on) }
        onClicked: root.run(modelData)
      }
    }
  }

  Column {
    visible: root.scanCount > 0
    width: parent.width
    spacing: Style.space(2)

    Repeater {
      model: root.scan.rows

      CursorSurface {
        id: deviceRow
        required property var modelData
        required property int index
        width: parent.width
        implicitHeight: deviceText.implicitHeight + Style.spacing.lg * 2
        hasCursor: root.panel.hasCursor("scan", index)
        foreground: root.foreground

        Row {
          id: deviceText
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.leftMargin: Style.spacing.rowPaddingX
          anchors.rightMargin: Style.spacing.rowPaddingX
          anchors.verticalCenter: parent.verticalCenter
          spacing: Style.space(10)
          opacity: deviceRow.modelData.candidate === true ? 1 : 0.6

          Text {
            id: deviceName
            textFormat: Text.PlainText
            width: Math.min(implicitWidth, parent.width - deviceAddress.implicitWidth - parent.spacing)
            text: deviceRow.modelData.name ? String(deviceRow.modelData.name) : "Unknown"
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            elide: Text.ElideRight
          }

          Text {
            id: deviceAddress
            textFormat: Text.PlainText
            text: String(deviceRow.modelData.address)
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            anchors.baseline: deviceName.baseline
          }
        }

        MouseArea {
          anchors.fill: parent
          hoverEnabled: true
          cursorShape: Qt.PointingHandCursor
          onContainsMouseChanged: root.panel.hoverCursor("scan", deviceRow.index, containsMouse)
          onClicked: root.choose(deviceRow.index)
        }
      }
    }

    CursorSurface {
      id: toggleRow
      visible: root.scan.hidden > 0 || root.showAll
      width: parent.width
      implicitHeight: toggleText.implicitHeight + Style.spacing.md * 2
      hasCursor: root.panel.hasCursor("scan", root.scan.rows.length)
      foreground: root.foreground

      Text {
        id: toggleText
        textFormat: Text.PlainText
        anchors.left: parent.left
        anchors.leftMargin: Style.spacing.rowPaddingX
        anchors.verticalCenter: parent.verticalCenter
        text: root.showAll ? "Show pads only" : "Show all devices (" + root.scan.hidden + ")"
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
      }

      MouseArea {
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onContainsMouseChanged: root.panel.hoverCursor("scan", root.scan.rows.length, containsMouse)
        onClicked: root.showAll = !root.showAll
      }
    }
  }
}

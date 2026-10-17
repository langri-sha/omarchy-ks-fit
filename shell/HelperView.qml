pragma ComponentBehavior: Bound

import QtQuick
import qs.Commons
import qs.Ui
import "Model.js" as Model

// Banner over the panel while the helper isn't ready or the QML is stale.
// It never replaces the views: a kept helper may still be driving a belt,
// and its Stop stays below.
BorderSurface {
  id: root

  property var service: null
  property color foreground: Color.foreground
  property string fontFamily: Style.font.family
  property int cursorIndex: -1

  signal hovered(int index, bool isHovered)

  readonly property string status: service ? service.helperStatus : "starting"
  readonly property bool mayBeMoving: !!service && service.mayBeMoving
  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property string buildPath: decodeURIComponent(String(Qt.resolvedUrl("../bin/ks-fit-build")).replace(/^file:\/\//, ""))

  readonly property var actions: {
    switch (status) {
      case "missing":
        return [{ id: "build", label: "Build helper", icon: Model.glyphs.build }, { id: "retry", label: "Retry", icon: Model.glyphs.retry }]
      case "outdated":
        return [{ id: "build", label: "Rebuild helper", icon: Model.glyphs.build }, { id: "retry", label: "Retry", icon: Model.glyphs.retry }]
      case "failed":
        return [{ id: "retry", label: "Retry", icon: Model.glyphs.retry }]
      case "stale":
        return [{ id: "restart", label: "Restart shell", icon: Model.glyphs.restart, enabled: !mayBeMoving }]
    }
    return []
  }
  readonly property string title: {
    switch (status) {
      case "missing": return "KS Fit needs its helper built once."
      case "outdated": return "KS Fit needs its helper rebuilt for this version."
      case "failed": return "The KS Fit helper stopped working."
      case "stale": return "Restart the shell to finish updating."
    }
    return "Starting the KS Fit helper…"
  }
  readonly property string detail: {
    switch (status) {
      case "missing":
      case "outdated":
        return "It compiles the bundled Rust source with cargo in a terminal, which takes a couple of minutes. Nothing leaves the machine except crate downloads."
      case "failed":
        return service && service.helperError !== "" ? service.helperError : "It exited repeatedly."
      case "stale":
        return mayBeMoving ? "KS Fit was updated. Stop the belt first: restarting drops the panel for a moment."
          : "KS Fit was updated while the shell kept running the previous version."
    }
    return ""
  }

  function activate(index) {
    var action = actions[index]
    if (!action || action.enabled === false) return
    // The terminal helper runs its argument through `bash -c` exactly once,
    // so the path is quoted here and nowhere else.
    if (action.id === "build")
      Util.execArgv(["omarchy-launch-floating-terminal-with-presentation", "bash " + Util.shellQuote(buildPath)])
    else if (action.id === "retry" && service)
      service.retryHelper()
    else if (action.id === "restart")
      Util.execArgv(["omarchy-restart-shell"])
  }

  implicitHeight: content.implicitHeight + contentTopInset + contentBottomInset
  color: status === "starting" ? "transparent" : Style.hoverFillFor(foreground, Color.accent)
  borderSpec: status === "starting" ? Border.none() : Border.controlSpec("normal", foreground, Color.accent)
  radius: Style.cornerRadius
  padding: status === "starting" ? 0 : Style.spacing.xxl

  Column {
    id: content
    x: root.contentLeftInset
    y: root.contentTopInset
    width: root.width - root.contentLeftInset - root.contentRightInset
    spacing: Style.space(10)

    Row {
      width: parent.width
      spacing: Style.space(12)

      Text {
        id: icon
        textFormat: Text.PlainText
        anchors.verticalCenter: parent.verticalCenter
        visible: root.status !== "starting"
        text: root.status === "stale" ? Model.glyphs.restart : root.status === "failed" ? Model.glyphs.alert : Model.glyphs.build
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.display
      }

      Column {
        width: parent.width - (icon.visible ? icon.width + parent.spacing : 0)
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(4)

        Text {
          textFormat: Text.PlainText
          width: parent.width
          text: root.title
          color: root.status === "starting" ? root.dim : root.foreground
          font.family: root.fontFamily
          font.pixelSize: root.status === "starting" ? Style.font.bodySmall : Style.font.subtitle
          font.bold: root.status !== "starting"
          wrapMode: Text.WordWrap
        }

        Text {
          textFormat: Text.PlainText
          visible: text !== ""
          width: parent.width
          text: root.detail
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          wrapMode: Text.WordWrap
          maximumLineCount: 4
          elide: Text.ElideRight
        }
      }
    }

    Row {
      visible: root.actions.length > 0
      anchors.right: parent.right
      spacing: Style.spacing.xs

      Repeater {
        model: root.actions

        Button {
          required property var modelData
          required property int index
          text: modelData.label
          iconText: modelData.icon
          bordered: true
          enabled: modelData.enabled !== false
          opacity: enabled ? 1 : 0.4
          hasCursor: root.cursorIndex === index
          foreground: root.foreground
          fontFamily: root.fontFamily
          onHovered: function(on) { root.hovered(index, on) }
          onClicked: root.activate(index)
        }
      }
    }
  }
}

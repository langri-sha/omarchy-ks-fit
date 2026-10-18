import QtQuick
import qs.Commons
import qs.Ui

// Popout owned by BarWidget.qml. The body lives in PanelContent.qml so it
// also renders offscreen, where there is no PanelWindow backend.
Panel {
  id: root
  moduleName: "io.github.langri-sha.ks-fit"
  ipcTarget: ""
  manageIpc: false

  property var anchorItem: null
  property var hostWidget: null
  // The bar identifies popouts by the widget mounted in its slot, never by
  // this nested item.
  readonly property var barIdentity: hostWidget || root
  readonly property var service: hostWidget ? hostWidget.service : null

  function switchPanel(direction) {
    return bar && typeof bar.switchPanelFrom === "function" ? bar.switchPanelFrom(barIdentity, direction) : false
  }

  onOpenedChanged: {
    if (!service) return
    if (opened) {
      content.viewOverride = ""
      content.now = new Date()
      content.resetCursor()
      service.panelOpened(root)
    } else {
      service.panelClosed(root)
    }
  }

  // A hot reload destroys widgets, and this panel with them, without
  // closing it first.
  Component.onDestruction: if (service) service.panelClosed(root)

  KeyboardPanel {
    id: panel
    anchorItem: root.anchorItem
    owner: root.barIdentity
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(400))
    contentHeight: panel.fittedContentHeight(content.implicitHeight)

    // PanelKeyCatcher hands Backspace on as text ("\b") where the platform
    // gives it one; keys it doesn't accept land here, so Stop holds either way.
    Item {
      anchors.fill: parent
      Keys.onPressed: function(event) {
        if (event.key === Qt.Key_Backspace && event.text !== "\b") {
          content.stopKey()
          event.accepted = true
        }
      }

      PanelKeyCatcher {
        id: keyCatcher
        anchors.fill: parent

        onMoveRequested: function(dx, dy) { content.moveCursor(dx, dy) }
        onActivateRequested: content.activateCursor()
        onCloseRequested: root.close()
        onTabRequested: function(direction) { root.switchPanel(direction) }
        onTextKey: function(text) { content.textKey(text) }

        Flickable {
          id: flick
          // At fractional scales a clip edge flush with the content rounds
          // onto the controls' outer borders, so it sits just outside.
          readonly property int bleed: 2
          anchors.fill: parent
          anchors.margins: -bleed
          contentWidth: width
          contentHeight: content.implicitHeight + bleed * 2
          clip: true
          boundsBehavior: Flickable.StopAtBounds
          interactive: contentHeight > height

          PanelContent {
            id: content
            x: flick.bleed
            y: flick.bleed
            width: flick.width - flick.bleed * 2
            bar: root.bar
            service: root.service
          }
        }
      }
    }
  }
}

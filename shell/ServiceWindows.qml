import QtQuick
import Quickshell
import Quickshell.Wayland

// The service's window. Kept out of Service.qml: without a PanelWindow
// backend (offscreen renders) a file that mentions one does not compile.
Item {
  id: root

  property var service: null
  property bool guardReady: false

  // An idle inhibitor needs a mapped surface. This one is invisible and
  // click-through, and lives as long as the service.
  PanelWindow {
    id: guard
    implicitWidth: 1
    implicitHeight: 1
    color: "transparent"
    mask: Region {}
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.layer: WlrLayer.Background
    WlrLayershell.namespace: "omarchy-ks-fit-guard"
  }

  // Keeps the screen awake while the belt may be moving, a lost link
  // included: a locked screen hides the panel's Stop from someone still
  // walking. Hyprland ignores an inhibitor that is already enabled when the
  // layer surface maps, so it is only enabled a moment after the window
  // exists.
  IdleInhibitor {
    window: guard
    enabled: root.guardReady && !!root.service && root.service.mayBeMoving
  }

  Timer {
    id: guardTimer
    interval: 800
    running: true
    onTriggered: root.guardReady = true
  }

  Connections {
    target: Quickshell
    function onScreensChanged() {
      root.guardReady = false
      guardTimer.restart()
    }
  }
}

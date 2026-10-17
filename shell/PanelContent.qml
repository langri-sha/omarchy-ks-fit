pragma ComponentBehavior: Bound

import QtQuick
import qs.Commons
import qs.Ui
import "Model.js" as Model

// Panel body and its keyboard cursor. No PanelWindow types in here, so the
// render harness can instantiate it offscreen.
Item {
  id: root

  property var bar: null
  property var service: null

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  readonly property string helperStatus: service ? service.helperStatus : "starting"
  readonly property var snapshot: service ? service.state : null
  readonly property bool connected: !!service && service.connected
  readonly property bool exercising: !!service && service.exercising
  // Ticks while visible; also jumps when a walk ends, so its line shows up
  // without waiting for the next tick.
  property var now: new Date()
  readonly property string sessionMark: Model.sessionMark(snapshot)
  onSessionMarkChanged: now = new Date()

  // Stats or Workout follows the belt unless the user picks one; any change
  // of the belt hands the choice back.
  property string viewOverride: ""
  readonly property string view: viewOverride !== "" ? viewOverride : exercising ? "workout" : "stats"
  readonly property var views: [{ value: "stats", label: "Stats" }, { value: "workout", label: "Workout" }]
  readonly property Item activeView: view === "workout" ? workoutView : statsView

  onExercisingChanged: viewOverride = ""
  onConnectedChanged: if (!connected) viewOverride = ""
  onViewChanged: normalizeCursor()

  function setView(value) {
    viewOverride = value === (exercising ? "workout" : "stats") ? "" : value
  }

  // One cursor for keyboard and mouse, bluetooth-panel style: nothing is lit
  // until the first arrow key or hover. Start only answers a cursor that
  // arrow keys moved there (cursorByArrows).
  property bool cursorActive: false
  property bool cursorByArrows: false
  property string focusSection: ""
  property int selectedIndex: 0

  function sections() {
    var list = []
    if (connected) list.push({ name: "view", count: views.length, columns: views.length })
    if (banner.visible && banner.actions.length > 0)
      list.push({ name: "helper", count: banner.actions.length, columns: banner.actions.length })
    return list.concat(activeView.sections)
  }

  function sectionAt(list, name) {
    for (var i = 0; i < list.length; i++) if (list[i].name === name) return i
    return -1
  }

  function hasCursor(section, index) {
    return cursorActive && focusSection === section && selectedIndex === index
  }

  function cursorIndexIn(section) {
    return cursorActive && focusSection === section ? selectedIndex : -1
  }

  function hoverCursor(section, index, on) {
    if (!on) return
    cursorActive = true
    cursorByArrows = false
    focusSection = section
    selectedIndex = index
  }

  function resetCursor() {
    cursorActive = false
    cursorByArrows = false
    var list = sections()
    focusSection = list.length > 0 ? list[0].name : ""
    selectedIndex = 0
  }

  function normalizeCursor() {
    var list = sections()
    var at = sectionAt(list, focusSection)
    if (at >= 0 && selectedIndex < list[at].count) return
    var shown = cursorActive
    resetCursor()
    cursorActive = shown && list.length > 0
  }

  function moveCursor(dx, dy) {
    var list = sections()
    if (list.length === 0) return
    cursorByArrows = true
    if (!cursorActive) {
      normalizeCursor()
      cursorActive = true
      return
    }
    var at = sectionAt(list, focusSection)
    if (at < 0) {
      resetCursor()
      cursorActive = true
      return
    }
    var section = list[at]
    var cols = Math.max(1, section.columns)
    if (dx !== 0) {
      var next = selectedIndex + dx
      if (next >= 0 && next < section.count && Math.floor(next / cols) === Math.floor(selectedIndex / cols)) selectedIndex = next
      return
    }
    var below = selectedIndex + dy * cols
    if (below >= 0 && below < section.count) {
      selectedIndex = below
      return
    }
    var target = at + dy
    if (target < 0 || target >= list.length) return
    var to = list[target]
    var toCols = Math.max(1, to.columns)
    var col = cols > 1 && toCols > 1 ? Math.round((selectedIndex % cols) * (toCols - 1) / (cols - 1)) : 0
    var row = dy > 0 ? 0 : Math.ceil(to.count / toCols) - 1
    focusSection = to.name
    selectedIndex = Math.min(to.count - 1, row * toCols + col)
  }

  function activateCursor() {
    if (!cursorActive) return
    if (focusSection === "view") setView(views[selectedIndex].value)
    else if (focusSection === "helper") banner.activate(selectedIndex)
    else activeView.activate(focusSection, selectedIndex, cursorByArrows)
  }

  // Every key here is harmless except the stop keys; none of them starts
  // the belt.
  function textKey(t) {
    if (t === "\b" || t === "0") {
      stopKey()
      return
    }
    if (!service || view !== "stats") return
    switch (t) {
      case "d": service.selectPeriod("day"); break
      case "w": service.selectPeriod("week"); break
      case "m": service.selectPeriod("month"); break
      case "y": service.selectPeriod("year"); break
      case "s": service.selectMetric("steps"); break
      case "c": service.selectMetric("calories"); break
      case ",":
      case "<": service.shiftRange(-1); break
      case ".":
      case ">": service.shiftRange(1); break
      case "r": statsView.rescan(); break
    }
  }

  function stopKey() {
    if (service && service.connected) service.stop()
  }

  Timer {
    interval: 30000
    repeat: true
    running: root.visible
    onTriggered: root.now = new Date()
  }

  implicitHeight: column.implicitHeight

  Column {
    id: column
    width: parent.width
    spacing: Style.space(14)

    PanelHero {
      width: parent.width
      title: "KS Fit"
      meta: root.snapshot ? Model.linkText(root.snapshot) : ""
      detail: root.service && root.service.simulated && root.helperStatus === "ready" ? "DEMO" : ""
      foreground: root.foreground
      fontFamily: root.fontFamily
      iconOpacity: root.connected ? 1 : 0.5
      iconComponent: Component {
        Text {
          textFormat: Text.PlainText
          text: Model.glyphs.walk
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.display
        }
      }
      trailingControl: Component {
        ButtonGroup {
          visible: root.connected
          spacing: Style.spacing.xs
          options: root.views
          value: root.view
          focusable: false
          cursorIndex: root.cursorIndexIn("view")
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.caption
          onChanged: function(value) { root.setView(value) }
          onHovered: function(index, on) { root.hoverCursor("view", index, on) }
        }
      }
    }

    HelperView {
      id: banner
      visible: root.helperStatus !== "ready"
      width: parent.width
      service: root.service
      foreground: root.foreground
      fontFamily: root.fontFamily
      cursorIndex: root.cursorIndexIn("helper")
      onHovered: function(index, on) { root.hoverCursor("helper", index, on) }
    }

    StatsView {
      id: statsView
      visible: root.view === "stats"
      width: parent.width
      panel: root
    }

    WorkoutView {
      id: workoutView
      visible: root.view === "workout"
      width: parent.width
      panel: root
    }

    Text {
      textFormat: Text.PlainText
      readonly property bool alarm: !!root.service && root.service.stopFailure !== ""
      visible: text !== ""
      width: parent.width
      text: root.service ? root.service.notice : ""
      color: alarm ? Color.urgent : root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      font.bold: alarm
      elide: Text.ElideRight
    }
  }
}

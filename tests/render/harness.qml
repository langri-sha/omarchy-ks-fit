import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

ShellRoot {
  id: harness

  readonly property string pluginDir: Quickshell.env("PLUGIN_DIR")
  readonly property string pluginId: "io.github.langri-sha.ks-fit"
  readonly property bool mock: Quickshell.env("KS_FIT_RENDER_MOCK") === "1"
  readonly property var periods: ["day", "week", "month", "year"]
  readonly property var metrics: ["steps", "calories"]
  property var service: null
  property var manifest: null
  property int step: 0
  property real stepStarted: Date.now()
  property bool busy: false
  property bool settled: false
  property bool failed: false

  function log(message) { console.log("HARNESS " + message) }

  function fail(reason) {
    if (failed) return
    failed = true
    log("FAIL " + reason)
    Qt.quit()
  }

  FileView { id: manifestFile; path: harness.pluginDir + "/manifest.json"; blockLoading: true }

  Process {
    id: placeHelper
    property var done: null
    command: ["ln", "-s", Quickshell.env("KS_FIT_RENDER_SOURCE"), Quickshell.env("KS_FIT_HELPER")]
    onExited: function(code) {
      if (code !== 0) harness.fail("could not place the helper")
      else placeHelper.done()
    }
  }

  QtObject {
    id: fakeShell
    readonly property string pluginId: harness.pluginId
    property var entry: ({ id: harness.pluginId, demo: !harness.mock })
    readonly property var barConfig: ({ layout: { left: [], center: [entry], right: [] } })
    function serviceFor(id) { return id === harness.pluginId ? harness.service : null }
    function toggle(id, payload) { return false }
    function updateEntryInline(id, settings) {
      if (id !== harness.pluginId) return false
      var next = { id: id }
      for (var key in settings) if (key !== "id") next[key] = settings[key]
      entry = next
      return true
    }
  }

  QtObject {
    id: fakeBar
    readonly property color foreground: Color.bar.text
    readonly property color barForeground: Color.bar.text
    readonly property color background: Color.bar.background
    readonly property color urgent: Color.bar.active
    readonly property string fontFamily: Style.font.family
    readonly property string position: "top"
    readonly property bool vertical: false
    readonly property int barSize: Style.bar.sizeHorizontal
    readonly property bool foregroundAnimationEnabled: false
    readonly property var shell: fakeShell
    property var activePopout: null
    function showTooltip(target, text) {}
    function hideTooltip(target) {}
    function registerClickTarget(target) {}
    function unregisterClickTarget(target) {}
    function requestPopout(owner) { activePopout = owner }
    function releasePopout(owner) { if (activePopout === owner) activePopout = null }
    function switchPanelFrom(owner, direction) { return false }
    function moduleWidgets(id) { return [] }
    function run(command) {}
  }

  QtObject { id: openPanel }

  Component.onCompleted: {
    var component = Qt.createComponent("file://" + pluginDir + "/shell/Service.qml", Component.PreferSynchronous)
    if (component.status !== Component.Ready) {
      fail("Service.qml did not load: " + component.errorString())
      return
    }
    manifest = JSON.parse(manifestFile.text())
    var instance = component.createObject(null)
    instance.shell = fakeShell
    instance.manifest = manifest
    service = instance
  }

  FloatingWindow {
    implicitWidth: 1000
    implicitHeight: 1400
    color: Color.background

    Column {
      x: 16
      y: 16
      spacing: 16

      Rectangle {
        id: barStrip
        width: 440
        height: Style.bar.sizeHorizontal
        color: Color.bar.background

        Row {
          anchors.centerIn: parent
          height: parent.height
          spacing: Style.space(4)

          Loader {
            height: parent.height
            width: item ? item.implicitWidth : 0
            source: "file://" + Quickshell.env("OMARCHY_PATH") + "/shell/plugins/panels/clock/BarWidget.qml"
            onLoaded: {
              item.bar = fakeBar
              item.settings = { format: "HH:mm" }
            }
          }

          Loader {
            height: parent.height
            width: item ? item.implicitWidth : 0
            source: "file://" + harness.pluginDir + "/shell/BarWidget.qml"
            onLoaded: item.bar = fakeBar
          }
        }
      }

      BorderSurface {
        id: card
        readonly property Item content: loader.item

        width: Style.space(400) + contentLeftInset + contentRightInset
        height: loader.item ? loader.item.implicitHeight + contentTopInset + contentBottomInset : 100
        color: Color.popups.background
        borderSpec: Border.surfaceSpec("popups", "border", Color.popups.border, Math.max(1, Style.space(2)))
        padding: Style.spacing.popupPadding
        radius: Style.cornerRadius

        Loader {
          id: loader
          x: card.contentLeftInset
          y: card.contentTopInset
          width: card.width - card.contentLeftInset - card.contentRightInset
          source: "file://" + harness.pluginDir + "/shell/PanelContent.qml"
          onLoaded: {
            item.bar = fakeBar
            item.service = Qt.binding(function() { return harness.service })
          }
        }
      }
    }
  }

  function grab(items, next) {
    if (items.length === 0) {
      next()
      return
    }
    var target = items[0]
    target[0].grabToImage(function(result) {
      var path = Quickshell.env("OUT_DIR") + "/" + target[1] + ".png"
      if (!result.saveToFile(path)) fail("could not save " + path)
      log("saved " + target[1])
      grab(items.slice(1), next)
    })
  }

  function snapshot() { return service ? service.state : null }
  function pad() { var s = snapshot(); return s && s.pad ? s.pad : null }

  function statsSteps() {
    var list = []
    for (var p = 0; p < periods.length; p++) {
      for (var m = 0; m < metrics.length; m++) {
        list.push(statsStep(periods[p], metrics[m]))
      }
    }
    return list
  }

  function statsStep(period, metric) {
    return {
      name: "stats " + period + " " + metric,
      before: function() {
        service.selectPeriod(period)
        service.selectMetric(metric)
      },
      until: function() {
        var stats = service.currentStats
        return !!stats && stats.period === period && stats.offset === 0
      },
      settle: 400,
      run: function(done) { grab([[card, "stats-" + period + "-" + metric]], done) }
    }
  }

  readonly property var steps: [
    {
      name: "helper missing",
      until: function() { return service.helperStatus === "missing" },
      settle: 300,
      run: function(done) { grab([[card, "helper-missing"], [barStrip, "bar-missing"]], done) }
    },
    {
      name: "build",
      run: function(done) {
        placeHelper.done = function() {
          service.retryHelper()
          done()
        }
        placeHelper.running = true
      }
    },
    {
      name: "ready",
      until: function() { return service.helperStatus === "ready" && !!snapshot() },
      run: function(done) {
        service.panelOpened(openPanel)
        done()
      }
    },
    {
      name: "scan",
      until: function() { return !!snapshot() && !snapshot().device && snapshot().bluetooth === "on" },
      run: function(done) {
        service.scan()
        done()
      }
    },
    {
      name: "scan list",
      until: function() { return snapshot().scan.devices.length >= (harness.mock ? 4 : 2) },
      settle: 300,
      run: function(done) { grab([[card, "scan"]], done) }
    },
    {
      name: "connect",
      run: function(done) {
        var devices = snapshot().scan.devices
        service.connectTo(devices[0].address, devices[0].name)
        done()
      }
    },
    {
      name: "connected",
      until: function() { return service.connected && !!service.currentStats },
      settle: 400,
      run: function(done) { grab([[barStrip, "bar-idle"]], done) }
    }
  ].concat(statsSteps()).concat([
    {
      name: "weight",
      before: function() {
        service.selectPeriod("week")
        service.selectMetric("calories")
      },
      run: function(done) {
        service.setWeight(Math.round(service.weightKg) + 1)
        done()
      }
    },
    {
      name: "weight written",
      until: function() { return fakeShell.entry.weightKg === 71 && snapshot().weightKg === 71 },
      settle: 300,
      run: function(done) { grab([[card, "stats-weight"]], done) }
    },
    {
      name: "hover on start",
      before: function() {
        service.selectPeriod("week")
        service.selectMetric("steps")
      },
      run: function(done) {
        card.content.resetCursor()
        card.content.hoverCursor("pad", 1, true)
        card.content.activateCursor()
        card.content.textKey(" ")
        done()
      }
    },
    {
      name: "hover left the belt alone",
      settle: 1500,
      run: function(done) {
        if (service.exercising) fail("a hovered Start started the belt")
        done()
      }
    },
    {
      name: "arrow going nowhere after a hover",
      run: function(done) {
        var content = card.content
        content.resetCursor()
        content.hoverCursor("pad", 1, true)
        content.moveCursor(0, 1)
        if (!content.hasCursor("pad", 1)) {
          fail("the pad row is no longer last, so this arrow moved")
          return
        }
        content.activateCursor()
        done()
      }
    },
    {
      name: "a stuck arrow left the belt alone",
      settle: 1500,
      run: function(done) {
        if (service.exercising) fail("an arrow press that went nowhere armed Start")
        service.disconnect()
        done()
      }
    },
    {
      name: "arrows onto forget",
      until: function() { return snapshot().link === "disconnected" && snapshot().linkDetail === "paused" },
      run: function(done) {
        var content = card.content
        content.resetCursor()
        for (var i = 0; i < 60 && !(content.cursorActive && content.focusSection === "pad"); i++)
          content.moveCursor(0, 1)
        content.moveCursor(1, 0)
        if (content.controlAt(content.focusSection, content.selectedIndex) !== "pad:forget") {
          fail("arrow keys never reached Forget")
          return
        }
        service.reconnect()
        done()
      }
    },
    {
      name: "start took forget's place",
      until: function() { return service.connected },
      settle: 300,
      run: function(done) {
        if (card.content.controlAt(card.content.focusSection, card.content.selectedIndex) !== "pad:start") {
          fail("expected Start under the cursor once connected")
          return
        }
        card.content.activateCursor()
        done()
      }
    },
    {
      name: "a shifted row left the belt alone",
      settle: 1500,
      run: function(done) {
        if (service.exercising) fail("Start answered an arrow press meant for Forget")
        else if (!snapshot().device) fail("Forget ran after the row changed")
        done()
      }
    },
    {
      name: "arrows onto start",
      run: function(done) {
        var content = card.content
        content.resetCursor()
        for (var i = 0; i < 60 && !(content.cursorActive && content.focusSection === "pad"); i++)
          content.moveCursor(0, 1)
        while (content.focusSection === "pad" && content.selectedIndex > 1) content.moveCursor(-1, 0)
        while (content.focusSection === "pad" && content.selectedIndex < 1) content.moveCursor(1, 0)
        if (!content.hasCursor("pad", 1)) {
          fail("arrow keys never reached Start")
          return
        }
        content.activateCursor()
        done()
      }
    },
    {
      name: "starting",
      until: function() { return !!pad() && pad().belt === "starting" },
      settle: 300,
      run: function(done) { grab([[card, "workout-starting"]], done) }
    },
    {
      name: "running",
      until: function() { return !!pad() && pad().belt === "running" && Math.abs(pad().speedKmh - pad().targetKmh) < 0.05 },
      run: function(done) {
        if (harness.mock) service.send({ cmd: "mock", fastForwardS: 754 })
        done()
      }
    },
    {
      name: "walking",
      until: function() { return !!snapshot().session && (!harness.mock || snapshot().session.activeS >= 754) },
      settle: 400,
      run: function(done) { grab([[barStrip, "bar-live"], [card, "workout"]], done) }
    },
    {
      name: "stale while walking",
      run: function(done) {
        service.manifest = { version: "0.0.0-stale" }
        done()
      }
    },
    {
      name: "stale banner",
      until: function() { return service.helperStatus === "stale" },
      settle: 300,
      run: function(done) {
        grab([[card, "stale-walking"]], function() {
          service.manifest = harness.manifest
          done()
        })
      }
    },
    {
      name: "stop key",
      until: function() { return service.helperStatus === "ready" },
      run: function(done) {
        card.content.textKey("\b")
        done()
      }
    },
    {
      name: "stopped",
      until: function() { return !service.exercising && !!snapshot().lastSession && card.content.view === "stats" },
      settle: 500,
      run: function(done) { grab([[card, "stats-after-walk"]], done) }
    },
    {
      name: "bluetooth off",
      mockOnly: true,
      run: function(done) {
        service.send({ cmd: "mock", bluetooth: "blocked" })
        done()
      }
    },
    {
      name: "bluetooth off view",
      mockOnly: true,
      until: function() { return snapshot().bluetooth === "blocked" },
      settle: 300,
      run: function(done) { grab([[card, "bluetooth-off"], [barStrip, "bar-bluetooth-off"]], done) }
    },
    {
      name: "pad away",
      mockOnly: true,
      run: function(done) {
        service.send({ cmd: "mock", bluetooth: "on", linkDetail: "not-found" })
        done()
      }
    },
    {
      name: "pad away view",
      mockOnly: true,
      until: function() { return snapshot().bluetooth === "on" && snapshot().linkDetail === "not-found" },
      settle: 300,
      run: function(done) { grab([[card, "not-found"]], done) }
    },
    {
      name: "demo",
      mockOnly: true,
      run: function(done) {
        fakeShell.updateEntryInline(harness.pluginId, { demo: true })
        done()
      }
    },
    {
      name: "demo helper",
      mockOnly: true,
      until: function() { return service.helperStatus === "ready" && service.simulated && !!snapshot() },
      run: function(done) {
        service.connectTo("C8:47:8C:12:34:56", "KS-ST-A1P")
        done()
      }
    },
    {
      name: "demo connected",
      mockOnly: true,
      until: function() { return service.connected && !!service.currentStats },
      settle: 400,
      run: function(done) { grab([[barStrip, "bar-demo"], [card, "stats-demo"]], done) }
    }
  ])

  Timer {
    interval: 100
    repeat: true
    running: harness.service !== null
    onTriggered: {
      if (harness.busy || harness.failed) return
      if (harness.step >= harness.steps.length) {
        harness.log("ok")
        Qt.quit()
        return
      }
      var current = harness.steps[harness.step]
      if (current.mockOnly && !harness.mock) {
        harness.step++
        return
      }
      if (current.before && !current.beforeDone) {
        current.beforeDone = true
        current.before()
      }
      if (Date.now() - harness.stepStarted > 20000) {
        harness.fail("timed out waiting for " + current.name + " (helper " + harness.service.helperStatus
          + ", state " + JSON.stringify(harness.service.state) + ")")
        return
      }
      if (current.until && !current.until()) return
      if (current.settle && !harness.settled) {
        harness.settled = true
        harness.busy = true
        settleTimer.interval = current.settle
        settleTimer.start()
        return
      }
      harness.busy = true
      current.run(function() {
        harness.step++
        harness.stepStarted = Date.now()
        harness.settled = false
        harness.busy = false
      })
    }
  }

  Timer {
    id: settleTimer
    onTriggered: harness.busy = false
  }
}

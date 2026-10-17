import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import "Model.js" as Model
import "Version.js" as Version

// Headless singleton the shell creates once and keeps across plugin reloads
// (manifest keepLoaded). It owns the helper process and all state; the bar
// widgets and the panel only bind to it. A QtObject rather than an Item, so
// `state` can hold the helper's snapshot without shadowing Item.state.
QtObject {
  id: root

  property var shell: null
  property var manifest: null

  readonly property string pluginId: "io.github.langri-sha.ks-fit"
  readonly property string launcherPath: localPath("../bin/ks-fit-launcher")

  // Settings live on the widget's bar-layout entry. The manifest's defaults
  // are metadata only, so every read carries its fallback.
  readonly property var settings: Model.layoutEntry(shell ? shell.barConfig : null, pluginId)
  readonly property real settingsWeightKg: Model.settingsWeight(settings)
  readonly property bool demo: Model.coerceBool(settings.demo, false)
  // The mode the helper runs in, or starts in next. A `demo` flip reaches it
  // only while the belt stands still: switching helpers would take the
  // controls away from a pad someone is walking on.
  property bool helperDemo: false
  readonly property bool demoBlocked: demo !== helperDemo && exercising

  // After `omarchy plugin update` the kept service still runs the old QML.
  readonly property bool stale: !!manifest && !!manifest.version && manifest.version !== Version.version
  // starting → ready | missing | outdated | failed
  property string helper: "starting"
  property string helperError: ""
  property string helperVersion: ""
  property bool simulated: false
  // What the views branch on: `stale` wins over the helper's own state.
  readonly property string helperStatus: stale ? "stale" : helper

  // The helper's last `state` snapshot, or null while it isn't running.
  property var state: null
  readonly property bool connected: Model.connected(state)
  readonly property bool exercising: Model.exercising(state)
  readonly property var pad: connected ? state.pad : null
  readonly property var today: state && state.today ? state.today : null
  readonly property real weightKg: Model.weightFor(state, settingsWeightKg)
  readonly property string barText: helperStatus === "ready" ? Model.barText(state) : ""
  readonly property string tooltip: Model.tooltip(state, helperStatus)

  // Kept here so every monitor's panel agrees and it survives a close.
  property string statsPeriod: "week"
  property string statsMetric: "steps"
  property int statsOffset: 0
  // Replies keyed `period:offset`; bump statsRevision on every change.
  property var statsCache: ({})
  property int statsRevision: 0
  property int statsRequestId: 0
  readonly property var currentStats: statsRevision >= 0 ? statsFor(statsPeriod, statsOffset) : null

  property string errorNotice: ""
  // Stays until the belt is seen standing or the user tries again.
  property string stopFailure: ""
  readonly property string notice: stopFailure !== "" ? stopFailure
    : demoBlocked ? Model.demoBlockedText(demo) : errorNotice
  // Open panels, at most one per monitor. Held by object so a panel that a hot
  // reload destroys can take itself out (Component.onDestruction).
  property var panels: []
  readonly property int openPanels: panels.length

  property bool destroying: false
  property bool restarting: false
  property bool lockHeld: false
  property int attempts: 0
  property var exits: []
  property string lastStderr: ""

  function localPath(relative) {
    return decodeURIComponent(String(Qt.resolvedUrl(relative)).replace(/^file:\/\//, ""))
  }

  function send(command) {
    if (helper !== "ready" || !helperProcess.running) return false
    helperProcess.write(JSON.stringify(command) + "\n")
    return true
  }

  function configure() {
    var command = Model.configureCommand(settings)
    return command ? send(command) : false
  }
  function wake() { return send({ cmd: "wake" }) }
  function scan() { return send({ cmd: "scan" }) }
  function stopScan() { return send({ cmd: "stopScan" }) }
  function reconnect() { return send({ cmd: "connect" }) }
  function disconnect() { return send({ cmd: "disconnect" }) }
  function forget() { return send({ cmd: "forget" }) }
  function start() { return beltCommand("start") }
  function stop() { return beltCommand("stop") }
  // The daemon decides from the pad's latest status, not from this snapshot.
  function toggleBelt() { return beltCommand("toggle") }

  function beltCommand(cmd) {
    if (!send({ cmd: cmd })) return false
    stopFailure = ""
    return true
  }

  function connectTo(address, name) {
    var command = { cmd: "connect", address: String(address) }
    if (name !== undefined && name !== null && name !== "") command.name = String(name)
    return send(command)
  }

  function setSpeed(kmh) {
    var value = Number(kmh)
    return isFinite(value) ? send({ cmd: "speed", kmh: Math.round(value * 10) / 10 }) : false
  }

  function nudge(deltaKmh) {
    var delta = Number(deltaKmh)
    return isFinite(delta) && delta !== 0 ? send({ cmd: "nudge", deltaKmh: delta }) : false
  }

  // IPC answers right away, so it can only report what the cached state
  // already rules out; the daemon's own refusals arrive as `notice`.
  function ipcBelt(cmd) {
    var effective = cmd === "toggle" ? (exercising ? "stop" : "start") : cmd
    var blocker = Model.beltBlocker(helper, state, effective)
    if (blocker !== "") return blocker
    return beltCommand(cmd) ? "ok" : "helper-" + helper
  }

  // rfkill, like the shell's own Bluetooth panel: the daemon never changes
  // adapter power itself.
  function bluetoothOn() {
    Quickshell.execDetached(["omarchy-bluetooth-power", "on"])
  }

  function statsFor(period, offset) {
    return statsCache[Model.statsKey(period, offset)] || null
  }

  function requestStats(period, offset) {
    if (!Model.isPeriod(period)) return false
    statsRequestId++
    return send({ cmd: "stats", id: statsRequestId, period: period, offset: Math.min(0, Math.round(Number(offset) || 0)) })
  }

  function refreshStats() { return requestStats(statsPeriod, statsOffset) }

  function selectPeriod(period) {
    if (!Model.isPeriod(period)) return
    if (period !== statsPeriod) {
      statsPeriod = period
      statsOffset = 0
    }
    refreshStats()
  }

  function selectMetric(metric) {
    if (metric === "steps" || metric === "calories") statsMetric = metric
  }

  function shiftRange(delta) {
    var next = Math.min(0, statsOffset + Math.round(Number(delta) || 0))
    if (next === statsOffset) return
    statsOffset = next
    refreshStats()
  }

  function panelOpened(panel) {
    if (panel && panels.indexOf(panel) === -1) panels = panels.concat([panel])
    wake()
    refreshStats()
  }

  function panelClosed(panel) {
    panels = panels.filter(function(open) { return !!open && open !== panel })
  }

  function showNotice(text) {
    errorNotice = text
    noticeTimer.restart()
  }

  function handleLine(line) {
    var message
    try {
      message = JSON.parse(line)
    } catch (e) {
      console.warn("ks-fit: unreadable helper line: " + line)
      return
    }
    if (!message || typeof message !== "object") return
    switch (message.type) {
      case "hello": handleHello(message); break
      case "fatal": handleFatal(message); break
      case "state": handleState(message); break
      case "stats": handleStats(message); break
      case "error": handleError(message); break
    }
  }

  // A new helper may be the simulator where the real one ran, or the other
  // way round: nothing cached from before carries over.
  function handleHello(message) {
    state = null
    statsCache = ({})
    statsRevision++
    helperVersion = String(message.version || "")
    simulated = message.simulated === true
    if (helperVersion !== Version.version) {
      console.warn("ks-fit: helper " + helperVersion + " doesn't match the plugin's " + Version.version)
      helper = "outdated"
      stopHelper()
      return
    }
    helper = "ready"
    helperError = ""
    configure()
    send({ cmd: "sync" })
    if (openPanels > 0) {
      wake()
      refreshStats()
    }
  }

  function handleFatal(message) {
    var next = Model.helperForFatal(message.code)
    lockHeld = next === "starting"
    if (lockHeld) return
    helper = next
    if (helper === "failed") helperError = String(message.message || message.code || "The helper stopped")
  }

  function handleError(message) {
    if (!Model.stopFailed(message)) {
      showNotice(Model.errorText(message))
      return
    }
    stopFailure = Model.stopFailureText
    Util.execArgv(["omarchy-notification-send", "--app-name", "KS Fit", "-u", "critical",
                   "-g", Model.glyphs.walk, Model.stopFailureText])
  }

  function handleState(message) {
    var closed = state !== null && Model.sessionMark(message) !== Model.sessionMark(state)
    state = message
    if (Model.connected(message) && !Model.exercising(message)) stopFailure = ""
    if (closed) refreshStats()
  }

  function handleStats(message) {
    if (!Model.isPeriod(message.period)) return
    statsCache[Model.statsKey(message.period, message.offset)] = message
    statsRevision++
  }

  function startHelper() {
    if (destroying || helperProcess.running) return
    lastStderr = ""
    lockHeld = false
    helperProcess.stdinEnabled = true
    helperProcess.running = true
  }

  // Closing stdin lets the helper shut down on its own terms; SIGTERM only
  // if it hasn't within 3 s.
  function stopHelper() {
    if (!helperProcess.running) return
    helperProcess.stdinEnabled = false
    stopTimer.restart()
  }

  function restartHelper() {
    if (!helperProcess.running) {
      startHelper()
      return
    }
    restarting = true
    stopHelper()
  }

  function helperStopped() {
    uptimeTimer.stop()
    stopTimer.stop()
    state = null
    if (destroying) return
    if (restarting) {
      restarting = false
      helper = "starting"
      Qt.callLater(startHelper)
      return
    }
    if (stale || helper === "missing" || helper === "outdated" || helper === "failed") return
    helper = "starting"
    attempts++
    restartTimer.interval = Model.backoffMs(attempts)
    if (lockHeld) {
      lockHeld = false
      restartTimer.restart()
      return
    }
    var now = Date.now()
    var recent = exits.filter(function(t) { return now - t < 60000 })
    recent.push(now)
    exits = recent
    if (recent.length >= 3) {
      helper = "failed"
      helperError = lastStderr || "The helper keeps exiting"
      return
    }
    restartTimer.restart()
  }

  // Runs on Retry and from bin/ks-fit-build. `missing` and `outdated` stay up
  // until the helper answers, so a retry doesn't make the panel flicker.
  function retryHelper() {
    if (stale || helper === "ready") return
    if (helperProcess.running) {
      if (helper !== "starting") restartHelper()
      return
    }
    if (helper === "failed") helper = "starting"
    helperError = ""
    attempts = 0
    exits = []
    restartTimer.stop()
    startHelper()
  }

  readonly property Process helperProcess: Process {
    command: [root.launcherPath].concat(root.helperDemo ? ["--simulate"] : [])
    stdinEnabled: true
    stdout: SplitParser {
      onRead: function(line) { root.handleLine(line) }
    }
    stderr: SplitParser {
      onRead: function(line) {
        root.lastStderr = line
        console.warn("ks-fit: " + line)
      }
    }
    // Restart from runningChanged, not exited: a binary that fails to start
    // never emits exited.
    onRunningChanged: {
      if (running) uptimeTimer.restart()
      else root.helperStopped()
    }
  }

  // Offscreen tooling has no PanelWindow backend, so the window lives in a
  // file of its own and simply fails to load there.
  readonly property Loader windows: Loader {
    source: "ServiceWindows.qml"
    onLoaded: item.service = root
  }

  readonly property Timer restartTimer: Timer {
    onTriggered: root.startHelper()
  }

  readonly property Timer stopTimer: Timer {
    interval: 3000
    onTriggered: if (root.helperProcess.running) root.helperProcess.running = false
  }

  readonly property Timer uptimeTimer: Timer {
    interval: 30000
    onTriggered: root.attempts = 0
  }

  readonly property Timer noticeTimer: Timer {
    interval: 6000
    onTriggered: root.errorNotice = ""
  }

  readonly property Timer statsTimer: Timer {
    interval: 60000
    repeat: true
    running: root.openPanels > 0 && root.statsOffset === 0 && root.helper === "ready"
    onTriggered: root.refreshStats()
  }

  onSettingsWeightKgChanged: configure()

  function applyDemo() {
    if (demo === helperDemo || exercising) return
    helperDemo = demo
    if (helperProcess.running) restartHelper()
  }

  // Injection fires this before the deferred start, while nothing runs yet.
  onDemoChanged: applyDemo()
  onExercisingChanged: applyDemo()

  // The shell assigns shell and manifest right after createObject().
  Component.onCompleted: Qt.callLater(startHelper)
  Component.onDestruction: destroying = true

  // `omarchy-shell io.github.langri-sha.ks-fit <fn> [arg]`, e.g. from
  // Hyprland binds. Answers come from cached state: the CLI gives up after 2 s.
  readonly property IpcHandler ipc: IpcHandler {
    target: "io.github.langri-sha.ks-fit"

    function start(): string { return root.ipcBelt("start") }
    function stop(): string { return root.ipcBelt("stop") }
    function toggle(): string { return root.ipcBelt("toggle") }
    function faster(): void { root.nudge(Model.speedStep) }
    function slower(): void { root.nudge(-Model.speedStep) }
    function speed(kmh: real): void { root.setSpeed(kmh) }
    function status(): string { return Model.statusJson(root.helperStatus, root.state) }
    // bin/ks-fit-build calls this once the helper is in place.
    function retryHelper(): void { root.retryHelper() }
  }
}

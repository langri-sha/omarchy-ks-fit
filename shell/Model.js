.pragma library
// Pure helpers for the plugin's QML. The tests load this file into a bare
// node:vm context with the pragma line stripped, so nothing here may reach
// for QML globals or the current time.

var glyphs = {
  walk: '\u{F0583}', // md-walk
}

var speedStep = 0.5
// Widest bar label short of an hour or ten thousand steps, so the bar
// doesn't shift as the numbers grow.
var barSample = '00:00 · 0,000'
var periods = ['day', 'week', 'month', 'year']
var backoffSteps = [1000, 2000, 5000, 15000, 30000]

function coerceBool(value, fallback) {
  if (typeof value === 'boolean') return value
  if (typeof value === 'number') return value !== 0
  if (typeof value === 'string') {
    var s = value.trim().toLowerCase()
    if (s === 'true' || s === '1' || s === 'yes' || s === 'on') return true
    if (s === 'false' || s === '0' || s === 'no' || s === 'off') return false
  }
  return fallback
}

function coerceNumber(value, fallback, min, max) {
  var n =
    typeof value === 'number'
      ? value
      : typeof value === 'string' && value.trim() !== ''
        ? Number(value.trim())
        : NaN
  if (!isFinite(n)) return fallback
  if (min !== undefined && n < min) n = min
  if (max !== undefined && n > max) n = max
  return n
}

// The plugin's own entry in the bar layout, where its settings live.
function layoutEntry(barConfig, pluginId) {
  var layout = barConfig && barConfig.layout ? barConfig.layout : {}
  var sections = ['left', 'center', 'right']
  for (var s = 0; s < sections.length; s++) {
    var entries = layout[sections[s]] || []
    for (var i = 0; i < entries.length; i++)
      if (entries[i] && entries[i].id === pluginId) return entries[i]
  }
  return {}
}

// Delay before restart number `attempt` (1-based).
function backoffMs(attempt) {
  return backoffSteps[
    Math.max(0, Math.min(backoffSteps.length - 1, attempt - 1))
  ]
}

// What a `fatal` from the launcher or the helper means for the panel. A
// held lock is a predecessor still shutting down: worth another try.
function helperForFatal(code) {
  if (code === 'helper-missing') return 'missing'
  if (code === 'helper-outdated') return 'outdated'
  if (code === 'already-running') return 'starting'
  return 'failed'
}

function connected(state) {
  return !!state && state.link === 'connected' && !!state.pad
}

// The belt moves or is about to: the walker is on it.
function exercising(state) {
  if (!connected(state)) return false
  var pad = state.pad
  return (
    pad.belt === 'starting' || pad.belt === 'running' || pad.busy === 'starting'
  )
}

function canNudge(state) {
  return connected(state) && state.pad.mode !== 'auto'
}

function pad2(n) {
  return n < 10 ? '0' + n : String(n)
}

// Written out rather than toLocaleString: QML's engine and node disagree.
function formatInt(n) {
  var value = Math.round(Number(n) || 0)
  var digits = String(Math.abs(value))
  var out = ''
  while (digits.length > 3) {
    out = ',' + digits.slice(-3) + out
    digits = digits.slice(0, -3)
  }
  return (value < 0 ? '-' : '') + digits + out
}

function formatDuration(seconds) {
  var total = Math.max(0, Math.floor(Number(seconds) || 0))
  var h = Math.floor(total / 3600)
  var m = Math.floor((total % 3600) / 60)
  var s = total % 60
  return h > 0 ? h + ':' + pad2(m) + ':' + pad2(s) : m + ':' + pad2(s)
}

function formatSpeed(kmh) {
  return (Math.round((Number(kmh) || 0) * 10) / 10).toFixed(1)
}

function deviceName(device) {
  return device && device.name ? String(device.name) : 'the pad'
}

// A few words for the bar tooltip.
function linkSummary(state) {
  if (!state) return ''
  if (state.bluetooth === 'off' || state.bluetooth === 'blocked')
    return 'Bluetooth is off'
  if (state.bluetooth === 'missing') return 'no Bluetooth adapter'
  if (state.bluetooth === 'unavailable') return 'Bluetooth unavailable'
  if (state.link === 'connected') return 'connected'
  if (!state.device) return 'no pad selected'
  if (state.link === 'searching')
    return 'looking for ' + deviceName(state.device)
  if (state.link === 'connecting') return 'connecting'
  return 'disconnected'
}

function barText(state) {
  if (!exercising(state)) return ''
  var session = state.session || {}
  return formatDuration(session.activeS) + ' · ' + formatInt(session.steps)
}

var helperTexts = {
  starting: 'starting the helper',
  missing: 'the helper is not built yet',
  outdated: 'the helper needs a rebuild for this version',
  failed: 'the helper stopped working',
  stale: 'restart the shell to finish updating',
}

function tooltip(state, helperStatus) {
  if (helperStatus !== 'ready')
    return 'KS Fit — ' + (helperTexts[helperStatus] || helperTexts.starting)
  if (!state) return 'KS Fit'
  if (exercising(state)) {
    if (state.pad.belt !== 'running') return 'KS Fit — starting the belt'
    var session = state.session || {}
    return (
      'KS Fit — walking at ' +
      formatSpeed(state.pad.speedKmh) +
      ' km/h · ' +
      formatDuration(session.activeS) +
      ' · ' +
      formatInt(session.steps) +
      ' steps'
    )
  }
  var steps = state.today ? Number(state.today.steps) || 0 : 0
  var text = 'KS Fit — ' + linkSummary(state)
  return steps > 0 ? text + ' · ' + formatInt(steps) + ' steps today' : text
}

// Why a belt command can't go out right now, in the daemon's error codes, or
// '' when it can. `stop` only ever needs a link.
function beltBlocker(helper, state, cmd) {
  if (helper !== 'ready') return 'helper-' + helper
  if (
    state &&
    (state.bluetooth === 'off' || state.bluetooth === 'blocked') &&
    !connected(state)
  )
    return 'bluetooth-off'
  if (!connected(state)) return 'not-connected'
  if (cmd === 'stop') return ''
  if (exercising(state)) return ''
  var pad = state.pad
  if (pad.imperial === true) return 'imperial-units'
  if (pad.shared === true) return 'in-use'
  if (pad.busy) return 'busy'
  return ''
}

function isPeriod(period) {
  return periods.indexOf(period) !== -1
}

function statsKey(period, offset) {
  return period + ':' + (Number(offset) || 0)
}

// Changes when a session closes, so the charts can refresh.
function sessionMark(state) {
  var last = state && state.lastSession
  return last ? last.id + ':' + last.endedAt : ''
}

function demoBlockedText(demo) {
  return demo
    ? 'Demo mode starts once the belt stops'
    : 'Demo mode ends once the belt stops'
}

var errorTexts = {
  'not-connected': 'The pad is not connected',
  'bluetooth-off': 'Bluetooth is off',
  busy: 'The pad is busy, try again in a moment',
  'auto-mode': 'Speed is set by the pad in auto mode',
  'start-not-acknowledged': "The pad didn't start",
  'mode-not-acknowledged': "The pad didn't change mode",
  'stop-not-acknowledged': "The pad didn't confirm the stop",
  'no-device': 'No pad selected',
}

function errorText(message) {
  var m = message || {}
  return (
    errorTexts[m.code] || String(m.message || m.code || 'Something went wrong')
  )
}

function statusJson(helper, state) {
  var s = state || {}
  var pad = s.pad || null
  return JSON.stringify({
    helper: helper,
    link: s.link || null,
    belt: pad ? pad.belt : null,
    speedKmh: pad ? pad.speedKmh : null,
    session: s.session || null,
    today: s.today || null,
  })
}

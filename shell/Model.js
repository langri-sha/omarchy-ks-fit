.pragma library
// Pure helpers for the plugin's QML. The tests load this file into a bare
// node:vm context with the pragma line stripped, so nothing here may reach
// for QML globals or the current time.

var speedStep = 0.5
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

// What a `fatal` from the launcher or the helper means for the panel.
function helperForFatal(code) {
  if (code === 'helper-missing') return 'missing'
  if (code === 'helper-outdated') return 'outdated'
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

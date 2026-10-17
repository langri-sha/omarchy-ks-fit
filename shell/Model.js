.pragma library
// Pure helpers for the plugin's QML. The tests load this file into a bare
// node:vm context with the pragma line stripped, so nothing here may reach
// for QML globals or the current time.

var glyphs = {
  walk: '\u{F0583}', // md-walk
  bluetooth: '\u{F00AF}', // md-bluetooth
  bluetoothOff: '\u{F00B2}', // md-bluetooth_off
  search: '\u{F0349}', // md-magnify
  play: '\u{F040A}', // md-play
  stop: '\u{F04DB}', // md-stop
  minus: '\u{F0374}', // md-minus
  plus: '\u{F0415}', // md-plus
  previous: '\u{F0141}', // md-chevron_left
  next: '\u{F0142}', // md-chevron_right
  link: '\u{F0337}', // md-link
  unlink: '\u{F0338}', // md-link_off
  forget: '\u{F01B4}', // md-delete
  close: '\u{F0156}', // md-close
  build: '\u{F1323}', // md-hammer_wrench
  retry: '\u{F0450}', // md-refresh
  restart: '\u{F0709}', // md-restart
  alert: '\u{F05D6}', // md-alert_circle_outline
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

// The entry's body weight, or 0 when it sets none.
function settingsWeight(entry) {
  return coerceNumber(entry ? entry.weightKg : undefined, 0, 20, 300)
}

// Only what the bar entry sets; the daemon keeps its persisted value for the
// rest. null when there is nothing to send.
function configureCommand(entry) {
  var weight = settingsWeight(entry)
  return weight > 0 ? { cmd: 'configure', weightKg: weight } : null
}

// What calories are estimated for: the daemon's value once it reports one.
function weightFor(state, entryWeight) {
  var reported = state ? Number(state.weightKg) : NaN
  if (isFinite(reported) && reported > 0) return reported
  return entryWeight > 0 ? entryWeight : 70
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

var stopFailureText = "KS Fit couldn't stop the belt — use the remote"

// A stop the pad didn't carry out: someone may still be walking.
function stopFailed(message) {
  var m = message || {}
  return (
    m.code === 'stop-not-acknowledged' ||
    (m.code === 'not-connected' && m.cmd === 'stop')
  )
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
  'imperial-units': 'The pad is set to miles; switch it to kilometres first',
  'in-use': 'Another app is driving the pad',
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

var monthNames = [
  'January',
  'February',
  'March',
  'April',
  'May',
  'June',
  'July',
  'August',
  'September',
  'October',
  'November',
  'December',
]
var dayNames = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']
var bucketUnits = { day: 'hour', week: 'day', month: 'day', year: 'month' }

function formatKm(metres) {
  var km = Math.max(0, Number(metres) || 0) / 1000
  if (km < 10) return km.toFixed(2) + ' km'
  if (km < 100) return km.toFixed(1) + ' km'
  return formatInt(km) + ' km'
}

function formatKcal(kcal) {
  return formatInt(kcal) + ' kcal'
}

function formatSteps(steps) {
  var n = Math.round(Number(steps) || 0)
  return formatInt(n) + (n === 1 ? ' step' : ' steps')
}

function formatActive(seconds) {
  var total = Math.max(0, Math.round(Number(seconds) || 0))
  if (total < 60) return total + ' s'
  var minutes = Math.floor(total / 60)
  if (minutes < 60) return minutes + ' min'
  var rest = minutes % 60
  return Math.floor(minutes / 60) + ' h' + (rest > 0 ? ' ' + rest + ' min' : '')
}

function formatWeight(kg) {
  var value = Math.round((Number(kg) || 0) * 10) / 10
  return (value % 1 === 0 ? String(value) : value.toFixed(1)) + ' kg'
}

function metricValue(bucket, metric) {
  if (!bucket) return 0
  return Number(metric === 'calories' ? bucket.kcal : bucket.steps) || 0
}

function formatMetric(value, metric) {
  return metric === 'calories' ? formatKcal(value) : formatSteps(value)
}

// The smallest 1, 2, 2.5 or 5 × 10ⁿ at or above `value`: the chart's top.
function niceMax(value) {
  var v = Number(value) || 0
  if (v <= 0) return 1
  var magnitude = Math.pow(10, Math.floor(Math.log(v) / Math.LN10))
  var steps = [1, 2, 2.5, 5, 10]
  for (var i = 0; i < steps.length; i++)
    if (steps[i] * magnitude >= v - 1e-9) return steps[i] * magnitude
  return 10 * magnitude
}

// "2026-10-04" as numbers. Never `new Date(text)`: that reads UTC midnight.
function parseDate(text) {
  var m = /^(\d{4})-(\d{2})-(\d{2})/.exec(String(text || ''))
  return m ? { year: +m[1], month: +m[2], day: +m[3] } : null
}

function weekday(date) {
  return new Date(Date.UTC(date.year, date.month - 1, date.day)).getUTCDay()
}

function shortDate(date, withWeekday) {
  var text = date.day + ' ' + monthNames[date.month - 1].slice(0, 3)
  return withWeekday ? dayNames[weekday(date)] + ' ' + text : text
}

function rangeLabel(stats) {
  var start = stats ? parseDate(stats.start) : null
  if (!start) return ''
  var end = parseDate(stats.end) || start
  switch (stats.period) {
    case 'day':
      return shortDate(start, true)
    case 'week':
      return shortDate(start, false) + ' – ' + shortDate(end, false)
    case 'month':
      return monthNames[start.month - 1] + ' ' + start.year
    case 'year':
      return String(start.year)
  }
  return ''
}

// One label per bucket, '' where thinned so neighbours never overlap.
function axisLabels(stats) {
  var count = stats && stats.buckets ? stats.buckets.length : 0
  var out = []
  for (var i = 0; i < count; i++) {
    switch (stats.period) {
      case 'day':
        out.push(i % 6 === 0 ? pad2(i) : '')
        break
      case 'week':
        out.push('MTWTFSS'.charAt(i))
        break
      case 'month':
        out.push(i % 7 === 0 ? String(i + 1) : '')
        break
      case 'year':
        out.push('JFMAMJJASOND'.charAt(i))
        break
      default:
        out.push('')
    }
  }
  return out
}

// What a single column stands for, for the value line above the chart.
function bucketLabel(stats, index) {
  var start = stats ? parseDate(stats.start) : null
  if (!start || index < 0) return ''
  switch (stats.period) {
    case 'day':
      return pad2(index) + ':00–' + pad2((index + 1) % 24) + ':00'
    case 'week':
    case 'month': {
      var at = new Date(Date.UTC(start.year, start.month - 1, start.day + index))
      return shortDate(
        {
          year: at.getUTCFullYear(),
          month: at.getUTCMonth() + 1,
          day: at.getUTCDate(),
        },
        true,
      )
    }
    case 'year':
      return monthNames[index % 12] + ' ' + start.year
  }
  return ''
}

// The column holding `now` (a local Date), or -1 outside the current period.
function currentBucketIndex(stats, now) {
  if (!stats || Number(stats.offset) !== 0 || !now) return -1
  var today =
    now.getFullYear() + '-' + pad2(now.getMonth() + 1) + '-' + pad2(now.getDate())
  if (today < String(stats.start) || today > String(stats.end)) return -1
  var index = -1
  switch (stats.period) {
    case 'day':
      index = now.getHours()
      break
    case 'week':
      index = (now.getDay() + 6) % 7
      break
    case 'month':
      index = now.getDate() - 1
      break
    case 'year':
      index = now.getMonth()
      break
  }
  var count = stats.buckets ? stats.buckets.length : 0
  return index >= 0 && index < count ? index : -1
}

function peakIndex(stats, metric) {
  var buckets = stats && stats.buckets ? stats.buckets : []
  var best = -1
  for (var i = 0; i < buckets.length; i++)
    if (
      metricValue(buckets[i], metric) > 0 &&
      (best < 0 ||
        metricValue(buckets[i], metric) > metricValue(buckets[best], metric))
    )
      best = i
  return best
}

function hasActivity(stats) {
  return !!stats && Number(stats.activeBuckets) > 0
}

// e.g. "31,200 steps · 6,240 a day on 5 active days"
function summary(stats, metric) {
  if (!hasActivity(stats) || !stats.totals) return ''
  var total = metricValue(stats.totals, metric)
  var active = Number(stats.activeBuckets)
  var unit = bucketUnits[stats.period] || 'day'
  var article = unit === 'hour' ? 'an' : 'a'
  var preposition = unit === 'day' ? 'on' : 'in'
  return (
    formatMetric(total, metric) +
    ' · ' +
    formatInt(total / active) +
    ' ' +
    article +
    ' ' +
    unit +
    ' ' +
    preposition +
    ' ' +
    active +
    ' active ' +
    unit +
    (active === 1 ? '' : 's')
  )
}

function weightHint(kg) {
  return 'estimated for ' + formatWeight(kg)
}

// The panel header's meta line.
function linkText(state) {
  if (!state) return ''
  switch (state.bluetooth) {
    case 'off':
    case 'blocked':
      return 'Bluetooth is off'
    case 'missing':
      return 'No Bluetooth adapter'
    case 'unavailable':
      return 'Bluetooth unavailable'
  }
  var name = state.device && state.device.name ? String(state.device.name) : ''
  var pad = name || 'the pad'
  var Pad = name || 'The pad'
  if (state.link === 'connected') return (name || 'Pad') + ' · connected'
  if (!state.device) return 'No pad selected'
  if (state.link === 'searching') return 'Looking for ' + pad + '…'
  if (state.link === 'connecting') return 'Connecting to ' + pad + '…'
  switch (state.linkDetail) {
    case 'not-found':
      return Pad + " isn't nearby"
    case 'link-lost':
      return 'Lost the connection, retrying'
    case 'unsupported-ftms':
      return Pad + " uses FTMS, which isn't supported yet"
    case 'unsupported-ks-text':
      return Pad + " uses a protocol that isn't supported"
    case 'not-a-walkingpad':
      return Pad + " isn't a WalkingPad"
    case 'connect-failed':
      return "Couldn't connect to " + pad
  }
  return 'Disconnected'
}

// A dim second line under the pad row where the meta alone leaves the user
// guessing.
function linkHint(state) {
  if (!state) return ''
  if (state.link === 'disconnected' && state.linkDetail === 'not-found')
    return 'Is it switched on? The KS Fit phone app also holds the connection while it is open.'
  if (connected(state) && state.pad.imperial === true)
    return 'The pad counts in miles. Switch it to kilometres to drive it from here.'
  if (connected(state) && state.pad.shared === true)
    return 'Another app is driving the pad right now.'
  return ''
}

// The last walk, for 15 minutes after it ended.
function recentSession(state, nowSeconds) {
  var last = state && state.lastSession
  if (!last || !last.endedAt) return null
  var age = Number(nowSeconds) - Number(last.endedAt)
  return age >= 0 && age < 900 ? last : null
}

function sessionLine(session) {
  var s = session || {}
  var parts = [
    formatDuration(s.activeS),
    formatSteps(s.steps),
    formatKm(s.distanceM),
    formatKcal(s.kcal),
  ]
  if (s.estimated === true) parts.push('estimated')
  return parts.join(' · ')
}

function todayLine(today) {
  var t = today || {}
  return (
    formatKcal(t.kcal) +
    ' · ' +
    formatKm(t.distanceM) +
    ' · ' +
    formatActive(t.activeS) +
    ' active'
  )
}

function speedRange(state) {
  if (!canNudge(state)) return { slower: false, faster: false }
  var pad = state.pad
  var target = Number(pad.targetKmh) || 0
  return {
    slower: target > (Number(pad.minKmh) || 0.5) + 1e-6,
    faster: target < (Number(pad.maxKmh) || 6) - 1e-6,
  }
}

// The pad row's buttons, by link and belt state. Start never appears for a
// belt that moves, and every stop path stays available while connected.
function padActions(state) {
  if (!state) return []
  if (state.bluetooth === 'off' || state.bluetooth === 'blocked')
    return [{ id: 'bluetoothOn', label: 'Turn on', icon: glyphs.bluetooth }]
  if (state.bluetooth !== 'on') return []
  if (!state.device) {
    var scanning = !!(state.scan && state.scan.active)
    return [
      scanning
        ? { id: 'stopScan', label: 'Scanning…', icon: glyphs.search, busy: true }
        : { id: 'scan', label: 'Scan for pads', icon: glyphs.search },
    ]
  }
  if (connected(state)) {
    if (exercising(state))
      return [{ id: 'stop', label: 'Stop', icon: glyphs.stop, primary: true }]
    var range = speedRange(state)
    var pad = state.pad
    var startable = !pad.busy && pad.imperial !== true && pad.shared !== true
    return [
      { id: 'slower', icon: glyphs.minus, enabled: range.slower },
      {
        id: 'start',
        label: 'Start · ' + formatSpeed(pad.targetKmh) + ' km/h',
        icon: glyphs.play,
        primary: true,
        enabled: startable,
      },
      { id: 'faster', icon: glyphs.plus, enabled: range.faster },
      { id: 'disconnect', icon: glyphs.unlink, tooltip: 'Disconnect' },
    ]
  }
  if (state.link === 'searching' || state.link === 'connecting')
    return [{ id: 'disconnect', label: 'Cancel', icon: glyphs.close }]
  return [
    { id: 'connect', label: 'Connect', icon: glyphs.link },
    { id: 'forget', icon: glyphs.forget, tooltip: 'Forget this pad' },
  ]
}

// Scan results to list: pads first, the rest only on request.
function scanRows(state, showAll) {
  var devices = state && state.scan && state.scan.devices ? state.scan.devices : []
  var rows = []
  var hidden = 0
  for (var i = 0; i < devices.length; i++) {
    var device = devices[i]
    if (!device || !device.address) continue
    if (device.candidate === true || showAll) rows.push(device)
    else hidden++
  }
  return { rows: rows, hidden: hidden }
}

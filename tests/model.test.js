import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import vm from 'node:vm'

const source = readFileSync(
  new URL('../shell/Model.js', import.meta.url),
  'utf8',
).replace(/^\.pragma library$/m, '')

const load = () => {
  const context = vm.createContext({})
  vm.runInContext(source, context, { filename: 'Model.js' })
  return context
}

const M = load()

const plain = (value) => JSON.parse(JSON.stringify(value))

const pad = (fields) => ({
  belt: 'idle',
  mode: 'manual',
  speedKmh: 0,
  targetKmh: 3,
  minKmh: 0.5,
  maxKmh: 6,
  busy: null,
  ...fields,
})

const state = (fields) => ({
  type: 'state',
  bluetooth: 'on',
  link: 'connected',
  linkDetail: null,
  device: { address: 'AA:BB:CC:DD:EE:FF', name: 'KS-ST-A1P' },
  autoConnect: true,
  scan: { active: false, devices: [] },
  pad: pad(),
  session: null,
  lastSession: null,
  today: {
    date: '2026-10-04',
    steps: 6012,
    distanceM: 4310,
    activeS: 3900,
    kcal: 251.3,
    sessions: 3,
  },
  weightKg: 70,
  ...fields,
})

const session = {
  id: 12,
  startedAt: 1759570000,
  activeS: 754,
  steps: 1023,
  distanceM: 870,
  kcal: 41.2,
  avgKmh: 4.2,
  maxKmh: 4.5,
}

test('module loads without the pragma line', () => {
  assert.equal(source.includes('.pragma'), false)
  assert.equal(typeof M.exercising, 'function')
})

test('coerceBool accepts the spellings settings arrive in', () => {
  assert.equal(M.coerceBool(true, false), true)
  assert.equal(M.coerceBool('true', false), true)
  assert.equal(M.coerceBool(' Off ', true), false)
  assert.equal(M.coerceBool(1, false), true)
  assert.equal(M.coerceBool(0, true), false)
  assert.equal(M.coerceBool(undefined, true), true)
  assert.equal(M.coerceBool('maybe', false), false)
})

test('coerceNumber clamps and falls back', () => {
  assert.equal(M.coerceNumber(82, 70, 20, 300), 82)
  assert.equal(M.coerceNumber('72.5', 70, 20, 300), 72.5)
  assert.equal(M.coerceNumber(5, 70, 20, 300), 20)
  assert.equal(M.coerceNumber(900, 70, 20, 300), 300)
  assert.equal(M.coerceNumber('', 70, 20, 300), 70)
  assert.equal(M.coerceNumber('heavy', 70, 20, 300), 70)
  assert.equal(M.coerceNumber(null, 70, 20, 300), 70)
  assert.equal(M.coerceNumber(true, 70, 20, 300), 70)
})

test('layoutEntry finds the plugin in any bar section', () => {
  const entry = { id: 'io.github.langri-sha.ks-fit', weightKg: 82 }
  const barConfig = {
    layout: {
      left: [{ id: 'omarchy.workspaces' }],
      center: [{ id: 'omarchy.clock' }, entry],
      right: [],
    },
  }
  assert.deepEqual(
    plain(M.layoutEntry(barConfig, 'io.github.langri-sha.ks-fit')),
    entry,
  )
  assert.deepEqual(plain(M.layoutEntry(barConfig, 'other')), {})
  assert.deepEqual(plain(M.layoutEntry(null, 'other')), {})
  assert.deepEqual(plain(M.layoutEntry({ layout: { left: [null] } }, 'x')), {})
})

test('backoffMs steps up and stays at 30 s', () => {
  assert.deepEqual(
    [1, 2, 3, 4, 5, 6, 20].map((n) => M.backoffMs(n)),
    [1000, 2000, 5000, 15000, 30000, 30000, 30000],
  )
})

test('helperForFatal tells a missing build and a held lock from a broken helper', () => {
  assert.equal(M.helperForFatal('helper-missing'), 'missing')
  assert.equal(M.helperForFatal('helper-outdated'), 'outdated')
  assert.equal(M.helperForFatal('already-running'), 'starting')
  assert.equal(M.helperForFatal('data-dir'), 'failed')
  assert.equal(M.helperForFatal(undefined), 'failed')
})

test('connected needs the link and a pad', () => {
  assert.equal(M.connected(state()), true)
  assert.equal(M.connected(state({ pad: null })), false)
  assert.equal(M.connected(state({ link: 'connecting', pad: null })), false)
  assert.equal(M.connected(null), false)
})

test('exercising covers the start sequence and a moving belt', () => {
  assert.equal(M.exercising(state()), false)
  assert.equal(M.exercising(state({ pad: pad({ busy: 'starting' }) })), true)
  assert.equal(M.exercising(state({ pad: pad({ belt: 'starting' }) })), true)
  assert.equal(M.exercising(state({ pad: pad({ belt: 'running' }) })), true)
  assert.equal(M.exercising(state({ pad: pad({ belt: 'standby' }) })), false)
  assert.equal(
    M.exercising(state({ pad: pad({ belt: 'running', busy: 'stopping' }) })),
    true,
  )
  assert.equal(M.exercising(state({ link: 'disconnected', pad: null })), false)
  assert.equal(M.exercising(null), false)
})

test('canNudge is off in auto mode and without a pad', () => {
  assert.equal(M.canNudge(state()), true)
  assert.equal(M.canNudge(state({ pad: pad({ mode: 'auto' }) })), false)
  assert.equal(M.canNudge(state({ pad: null })), false)
})

test('statsKey and isPeriod', () => {
  assert.equal(M.statsKey('week', 0), 'week:0')
  assert.equal(M.statsKey('year', -2), 'year:-2')
  assert.equal(M.statsKey('day', undefined), 'day:0')
  assert.deepEqual(
    ['day', 'week', 'month', 'year', 'decade'].map((p) => M.isPeriod(p)),
    [true, true, true, true, false],
  )
})

test('sessionMark changes when a session closes', () => {
  const last = { ...session, id: 11, endedAt: 1759561900 }
  assert.equal(M.sessionMark(state()), '')
  assert.equal(M.sessionMark(null), '')
  assert.equal(M.sessionMark(state({ lastSession: last })), '11:1759561900')
  assert.notEqual(
    M.sessionMark(state({ lastSession: last })),
    M.sessionMark(state({ lastSession: { ...last, endedAt: 1759562000 } })),
  )
})

test('errorText words the helper codes and falls back to its message', () => {
  assert.equal(
    M.errorText({ type: 'error', code: 'not-connected', cmd: 'start' }),
    'The pad is not connected',
  )
  assert.equal(
    M.errorText({ type: 'error', code: 'bad-command', message: 'unknown' }),
    'unknown',
  )
  assert.equal(M.errorText({ code: 'novel' }), 'novel')
  assert.equal(M.errorText(null), 'Something went wrong')
})

test('statusJson is compact and null-safe', () => {
  const running = state({
    pad: pad({ belt: 'running', speedKmh: 3.5 }),
    session,
  })
  assert.deepEqual(JSON.parse(M.statusJson('ready', running)), {
    helper: 'ready',
    link: 'connected',
    belt: 'running',
    speedKmh: 3.5,
    session,
    today: running.today,
  })
  assert.deepEqual(JSON.parse(M.statusJson('missing', null)), {
    helper: 'missing',
    link: null,
    belt: null,
    speedKmh: null,
    session: null,
    today: null,
  })
})

test('formatInt groups thousands', () => {
  assert.deepEqual(
    [0, 7, 999, 1000, 1023, 31200, 1234567, -4210, 41.6, undefined].map((n) =>
      M.formatInt(n),
    ),
    [
      '0',
      '7',
      '999',
      '1,000',
      '1,023',
      '31,200',
      '1,234,567',
      '-4,210',
      '42',
      '0',
    ],
  )
})

test('formatDuration switches to hours past an hour', () => {
  assert.deepEqual(
    [0, 5, 59, 754, 3599, 3600, 3900, 36061, -3, null].map((s) =>
      M.formatDuration(s),
    ),
    [
      '0:00',
      '0:05',
      '0:59',
      '12:34',
      '59:59',
      '1:00:00',
      '1:05:00',
      '10:01:01',
      '0:00',
      '0:00',
    ],
  )
})

test('formatSpeed keeps one decimal', () => {
  assert.deepEqual(
    [3.5, 3, 0, 2.04, 5.96, undefined].map((v) => M.formatSpeed(v)),
    ['3.5', '3.0', '0.0', '2.0', '6.0', '0.0'],
  )
})

test('barText shows the session only while exercising', () => {
  const running = state({ pad: pad({ belt: 'running' }), session })
  assert.equal(M.barText(running), '12:34 · 1,023')
  assert.equal(
    M.barText(state({ pad: pad({ busy: 'starting' }), session: null })),
    '0:00 · 0',
  )
  assert.equal(M.barText(state({ session })), '')
  assert.equal(M.barText(null), '')
})

test('barSample is as wide as labels short of an hour', () => {
  const running = state({ pad: pad({ belt: 'running' }), session })
  assert.equal(M.barSample.length, M.barText(running).length)
  assert.equal(M.barSample.replace(/0/g, '9'), '99:99 · 9,999')
})

test('linkSummary has a few words for every link situation', () => {
  assert.equal(M.linkSummary(state()), 'connected')
  assert.equal(
    M.linkSummary(state({ bluetooth: 'blocked', pad: null })),
    'Bluetooth is off',
  )
  assert.equal(M.linkSummary(state({ bluetooth: 'off' })), 'Bluetooth is off')
  assert.equal(
    M.linkSummary(state({ bluetooth: 'missing' })),
    'no Bluetooth adapter',
  )
  assert.equal(
    M.linkSummary(state({ bluetooth: 'unavailable' })),
    'Bluetooth unavailable',
  )
  assert.equal(
    M.linkSummary(state({ link: 'disconnected', device: null, pad: null })),
    'no pad selected',
  )
  assert.equal(
    M.linkSummary(state({ link: 'searching', pad: null })),
    'looking for KS-ST-A1P',
  )
  assert.equal(
    M.linkSummary(
      state({ link: 'searching', device: { address: 'A' }, pad: null }),
    ),
    'looking for the pad',
  )
  assert.equal(
    M.linkSummary(state({ link: 'connecting', pad: null })),
    'connecting',
  )
  assert.equal(
    M.linkSummary(state({ link: 'disconnected', pad: null })),
    'disconnected',
  )
  assert.equal(M.linkSummary(null), '')
})

test('tooltip is one line for every helper and link situation', () => {
  assert.equal(
    M.tooltip(state(), 'ready'),
    'KS Fit — connected · 6,012 steps today',
  )
  assert.equal(
    M.tooltip(state({ today: { steps: 0 } }), 'ready'),
    'KS Fit — connected',
  )
  assert.equal(
    M.tooltip(
      state({ pad: pad({ belt: 'running', speedKmh: 3.5 }), session }),
      'ready',
    ),
    'KS Fit — walking at 3.5 km/h · 12:34 · 1,023 steps',
  )
  assert.equal(
    M.tooltip(state({ pad: pad({ busy: 'starting' }) }), 'ready'),
    'KS Fit — starting the belt',
  )
  assert.equal(M.tooltip(null, 'ready'), 'KS Fit')
  assert.equal(
    M.tooltip(null, 'missing'),
    'KS Fit — the helper is not built yet',
  )
  assert.equal(
    M.tooltip(state(), 'stale'),
    'KS Fit — restart the shell to finish updating',
  )
  assert.equal(M.tooltip(null, undefined), 'KS Fit — starting the helper')
  for (const status of ['starting', 'missing', 'outdated', 'failed', 'stale'])
    assert.ok(!M.tooltip(state(), status).includes('\n'))
})

test('glyphs are Material Design code points', () => {
  for (const glyph of Object.values(M.glyphs)) {
    const cp = glyph.codePointAt(0)
    assert.ok(cp >= 0xf0001 && cp <= 0xf1af0, glyph)
  }
  assert.equal(M.glyphs.walk, '\u{F0583}')
})

test('beltBlocker lets stop through on any link and gates start', () => {
  assert.equal(M.beltBlocker('ready', state(), 'start'), '')
  assert.equal(M.beltBlocker('ready', state(), 'stop'), '')
  assert.equal(M.beltBlocker('missing', state(), 'stop'), 'helper-missing')
  assert.equal(M.beltBlocker('starting', null, 'start'), 'helper-starting')
  assert.equal(M.beltBlocker('ready', null, 'stop'), 'not-connected')
  assert.equal(
    M.beltBlocker(
      'ready',
      state({ bluetooth: 'blocked', link: 'disconnected', pad: null }),
      'stop',
    ),
    'bluetooth-off',
  )
  assert.equal(
    M.beltBlocker('ready', state({ link: 'searching', pad: null }), 'start'),
    'not-connected',
  )
  assert.equal(
    M.beltBlocker('ready', state({ pad: pad({ busy: 'stopping' }) }), 'start'),
    'busy',
  )
  assert.equal(
    M.beltBlocker('ready', state({ pad: pad({ busy: 'stopping' }) }), 'stop'),
    '',
  )
  assert.equal(
    M.beltBlocker('ready', state({ pad: pad({ imperial: true }) }), 'start'),
    'imperial-units',
  )
  assert.equal(
    M.beltBlocker('ready', state({ pad: pad({ shared: true }) }), 'start'),
    'in-use',
  )
  assert.equal(
    M.beltBlocker('ready', state({ pad: pad({ belt: 'running' }) }), 'start'),
    '',
  )
})

test('demoBlockedText says which way the switch is waiting', () => {
  assert.equal(M.demoBlockedText(true), 'Demo mode starts once the belt stops')
  assert.equal(M.demoBlockedText(false), 'Demo mode ends once the belt stops')
})

test('configureCommand carries only the weight the bar entry sets', () => {
  assert.deepEqual(plain(M.configureCommand({ id: 'x', weightKg: 82 })), {
    cmd: 'configure',
    weightKg: 82,
  })
  assert.deepEqual(plain(M.configureCommand({ weightKg: '72.5' })), {
    cmd: 'configure',
    weightKg: 72.5,
  })
  assert.deepEqual(plain(M.configureCommand({ weightKg: 900 })), {
    cmd: 'configure',
    weightKg: 300,
  })
  assert.equal(M.configureCommand({ id: 'x', demo: true }), null)
  assert.equal(M.configureCommand({ weightKg: 'heavy' }), null)
  assert.equal(M.configureCommand(null), null)
})

test('weightFor prefers the daemon, then the bar entry, then 70 kg', () => {
  assert.equal(M.weightFor(state({ weightKg: 72.5 }), 82), 72.5)
  assert.equal(M.weightFor(state({ weightKg: undefined }), 82), 82)
  assert.equal(M.weightFor(null, 0), 70)
  assert.equal(M.settingsWeight({ weightKg: 82 }), 82)
  assert.equal(M.settingsWeight({}), 0)
})

test('stopFailed singles out stops the pad did not carry out', () => {
  assert.equal(
    M.stopFailed({ code: 'stop-not-acknowledged', cmd: 'stop' }),
    true,
  )
  assert.equal(
    M.stopFailed({ code: 'stop-not-acknowledged', cmd: 'toggle' }),
    true,
  )
  assert.equal(M.stopFailed({ code: 'not-connected', cmd: 'stop' }), true)
  assert.equal(M.stopFailed({ code: 'not-connected', cmd: 'start' }), false)
  assert.equal(M.stopFailed({ code: 'busy', cmd: 'stop' }), false)
  assert.equal(M.stopFailed(null), false)
  assert.ok(M.stopFailureText.includes('remote'))
})

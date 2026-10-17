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

const weekStats = (fields) => ({
  type: 'stats',
  id: 7,
  period: 'week',
  offset: 0,
  start: '2026-09-28',
  end: '2026-10-04',
  buckets: [5321, 0, 7200, 6100, 4800, 0, 7779].map((steps, i) => ({
    key: `2026-09-${28 + i}`,
    steps,
    kcal: steps / 25,
    distanceM: steps * 0.7,
    activeS: steps / 2,
  })),
  totals: { steps: 31200, kcal: 1290.5, distanceM: 22900, activeS: 20100 },
  activeBuckets: 5,
  ...fields,
})

const zeroBuckets = (n) =>
  Array.from({ length: n }, () => ({
    steps: 0,
    kcal: 0,
    distanceM: 0,
    activeS: 0,
  }))

test('formatKm, formatKcal, formatSteps and formatActive', () => {
  assert.deepEqual(
    [0, 870, 4310, 22900, 123456].map((m) => M.formatKm(m)),
    ['0.00 km', '0.87 km', '4.31 km', '22.9 km', '123 km'],
  )
  assert.equal(M.formatKcal(41.2), '41 kcal')
  assert.equal(M.formatKcal(1290.5), '1,291 kcal')
  assert.equal(M.formatSteps(1), '1 step')
  assert.equal(M.formatSteps(1023), '1,023 steps')
  assert.deepEqual(
    [0, 45, 60, 1500, 3600, 3900].map((s) => M.formatActive(s)),
    ['0 s', '45 s', '1 min', '25 min', '1 h', '1 h 5 min'],
  )
  assert.equal(M.formatWeight(70), '70 kg')
  assert.equal(M.formatWeight(72.5), '72.5 kg')
})

test('niceMax rounds up to 1, 2, 2.5 or 5 times a power of ten', () => {
  assert.deepEqual(
    [0, -3, 0.3, 1, 7, 12, 21, 2600, 7000, 12345, 50000].map((v) =>
      M.niceMax(v),
    ),
    [1, 1, 0.5, 1, 10, 20, 25, 5000, 10000, 20000, 50000],
  )
})

test('parseDate reads the parts without going through UTC', () => {
  assert.deepEqual(plain(M.parseDate('2026-10-04')), {
    year: 2026,
    month: 10,
    day: 4,
  })
  assert.equal(M.parseDate('soon'), null)
  assert.equal(M.weekday(M.parseDate('2026-10-04')), 0)
  assert.equal(M.weekday(M.parseDate('2026-09-28')), 1)
})

test('rangeLabel names the period', () => {
  assert.equal(M.rangeLabel(weekStats()), '28 Sep – 4 Oct')
  assert.equal(
    M.rangeLabel({ period: 'day', start: '2026-10-04', end: '2026-10-04' }),
    'Sun 4 Oct',
  )
  assert.equal(
    M.rangeLabel({ period: 'month', start: '2026-10-01', end: '2026-10-31' }),
    'October 2026',
  )
  assert.equal(
    M.rangeLabel({ period: 'year', start: '2026-01-01', end: '2026-12-31' }),
    '2026',
  )
  assert.equal(M.rangeLabel(null), '')
})

test('axisLabels thins each period so labels never collide', () => {
  const labelled = (period, n) =>
    plain(M.axisLabels({ period, buckets: zeroBuckets(n) })).filter(
      (l) => l !== '',
    )
  assert.deepEqual(labelled('day', 24), ['00', '06', '12', '18'])
  assert.deepEqual(labelled('week', 7), ['M', 'T', 'W', 'T', 'F', 'S', 'S'])
  assert.deepEqual(labelled('month', 31), ['1', '8', '15', '22', '29'])
  assert.deepEqual(labelled('month', 28), ['1', '8', '15', '22'])
  assert.equal(labelled('year', 12).join(''), 'JFMAMJJASOND')
  assert.equal(
    M.axisLabels({ period: 'day', buckets: zeroBuckets(24) }).length,
    24,
  )
  assert.deepEqual([...M.axisLabels(null)], [])
})

test('bucketLabel names a single column', () => {
  assert.equal(M.bucketLabel(weekStats(), 0), 'Mon 28 Sep')
  assert.equal(M.bucketLabel(weekStats(), 6), 'Sun 4 Oct')
  assert.equal(
    M.bucketLabel({ period: 'day', start: '2026-10-04' }, 13),
    '13:00–14:00',
  )
  assert.equal(
    M.bucketLabel({ period: 'day', start: '2026-10-04' }, 23),
    '23:00–00:00',
  )
  assert.equal(
    M.bucketLabel({ period: 'month', start: '2026-10-01' }, 30),
    'Sat 31 Oct',
  )
  assert.equal(
    M.bucketLabel({ period: 'year', start: '2026-01-01' }, 9),
    'October 2026',
  )
  assert.equal(M.bucketLabel(weekStats(), -1), '')
})

test('currentBucketIndex finds now only in the current period', () => {
  const sunday = new Date(2026, 9, 4, 13, 5)
  assert.equal(M.currentBucketIndex(weekStats(), sunday), 6)
  assert.equal(
    M.currentBucketIndex(weekStats(), new Date(2026, 8, 28, 0, 1)),
    0,
  )
  assert.equal(M.currentBucketIndex(weekStats({ offset: -1 }), sunday), -1)
  assert.equal(
    M.currentBucketIndex(weekStats(), new Date(2026, 9, 5, 8, 0)),
    -1,
  )
  assert.equal(
    M.currentBucketIndex(
      {
        period: 'day',
        offset: 0,
        start: '2026-10-04',
        end: '2026-10-04',
        buckets: zeroBuckets(24),
      },
      sunday,
    ),
    13,
  )
  assert.equal(
    M.currentBucketIndex(
      {
        period: 'month',
        offset: 0,
        start: '2026-10-01',
        end: '2026-10-31',
        buckets: zeroBuckets(31),
      },
      sunday,
    ),
    3,
  )
  assert.equal(
    M.currentBucketIndex(
      {
        period: 'year',
        offset: 0,
        start: '2026-01-01',
        end: '2026-12-31',
        buckets: zeroBuckets(12),
      },
      sunday,
    ),
    9,
  )
  assert.equal(M.currentBucketIndex(null, sunday), -1)
})

test('metricValue and peakIndex follow the metric', () => {
  const stats = weekStats()
  assert.equal(M.metricValue(stats.buckets[0], 'steps'), 5321)
  assert.equal(M.metricValue(stats.buckets[0], 'calories'), 5321 / 25)
  assert.equal(M.metricValue(null, 'steps'), 0)
  assert.equal(M.peakIndex(stats, 'steps'), 6)
  assert.equal(M.peakIndex(weekStats({ buckets: zeroBuckets(7) }), 'steps'), -1)
})

test('summary words the totals by metric and period', () => {
  assert.equal(
    M.summary(weekStats(), 'steps'),
    '31,200 steps · 6,240 a day on 5 active days',
  )
  assert.equal(
    M.summary(weekStats(), 'calories'),
    '1,291 kcal · 258 a day on 5 active days',
  )
  assert.equal(
    M.summary(
      {
        period: 'day',
        totals: { steps: 6012, kcal: 251.3 },
        activeBuckets: 4,
      },
      'steps',
    ),
    '6,012 steps · 1,503 an hour in 4 active hours',
  )
  assert.equal(
    M.summary(
      { period: 'year', totals: { steps: 2600, kcal: 96 }, activeBuckets: 1 },
      'steps',
    ),
    '2,600 steps · 2,600 a month in 1 active month',
  )
  assert.equal(M.summary(weekStats({ activeBuckets: 0 }), 'steps'), '')
  assert.equal(M.summary(null, 'steps'), '')
  assert.equal(M.hasActivity(weekStats()), true)
  assert.equal(M.hasActivity(weekStats({ activeBuckets: 0 })), false)
  assert.equal(M.weightHint(70), 'estimated for 70 kg')
})

test('linkText words every link situation for the header', () => {
  const away = (linkDetail) =>
    M.linkText(state({ link: 'disconnected', linkDetail, pad: null }))
  assert.equal(M.linkText(state()), 'KS-ST-A1P · connected')
  assert.equal(M.linkText(state({ bluetooth: 'off' })), 'Bluetooth is off')
  assert.equal(M.linkText(state({ bluetooth: 'blocked' })), 'Bluetooth is off')
  assert.equal(
    M.linkText(state({ bluetooth: 'missing' })),
    'No Bluetooth adapter',
  )
  assert.equal(
    M.linkText(state({ bluetooth: 'unavailable' })),
    'Bluetooth unavailable',
  )
  assert.equal(
    M.linkText(state({ link: 'disconnected', device: null, pad: null })),
    'No pad selected',
  )
  assert.equal(
    M.linkText(state({ link: 'searching', pad: null })),
    'Looking for KS-ST-A1P…',
  )
  assert.equal(
    M.linkText(state({ link: 'connecting', pad: null })),
    'Connecting to KS-ST-A1P…',
  )
  assert.equal(away('not-found'), "KS-ST-A1P isn't nearby")
  assert.equal(away('paused'), 'Disconnected')
  assert.equal(away('link-lost'), 'Lost the connection, retrying')
  assert.equal(
    away('unsupported-ftms'),
    "KS-ST-A1P uses FTMS, which isn't supported yet",
  )
  assert.equal(
    away('unsupported-ks-text'),
    "KS-ST-A1P uses a protocol that isn't supported",
  )
  assert.equal(away('not-a-walkingpad'), "KS-ST-A1P isn't a WalkingPad")
  assert.equal(away('connect-failed'), "Couldn't connect to KS-ST-A1P")
  assert.equal(away(null), 'Disconnected')
  assert.equal(
    M.linkText(
      state({
        link: 'disconnected',
        linkDetail: 'not-found',
        device: { address: 'A', name: null },
        pad: null,
      }),
    ),
    "The pad isn't nearby",
  )
  assert.equal(M.linkText(null), '')
})

test('linkHint explains a missing pad and refusals without alarm', () => {
  assert.match(
    M.linkHint(
      state({ link: 'disconnected', linkDetail: 'not-found', pad: null }),
    ),
    /phone app/,
  )
  assert.match(M.linkHint(state({ pad: pad({ imperial: true }) })), /miles/)
  assert.match(M.linkHint(state({ pad: pad({ shared: true }) })), /Another app/)
  assert.equal(M.linkHint(state()), '')
  assert.equal(M.linkHint(null), '')
})

test('recentSession shows the last walk for 15 minutes', () => {
  const last = { ...session, id: 11, endedAt: 1759561900 }
  const s = state({ lastSession: last })
  assert.equal(M.recentSession(s, 1759561900 + 60).id, 11)
  assert.equal(M.recentSession(s, 1759561900 + 899).id, 11)
  assert.equal(M.recentSession(s, 1759561900 + 900), null)
  assert.equal(M.recentSession(s, 1759561900 - 5), null)
  assert.equal(M.recentSession(state(), 0), null)
})

test('sessionLine and todayLine', () => {
  assert.equal(
    M.sessionLine({
      activeS: 1800,
      steps: 2600,
      distanceM: 2100,
      kcal: 96,
    }),
    '30:00 · 2,600 steps · 2.10 km · 96 kcal',
  )
  assert.equal(
    M.sessionLine({ ...session, estimated: true }),
    '12:34 · 1,023 steps · 0.87 km · 41 kcal · estimated',
  )
  assert.equal(
    M.todayLine(state().today),
    '251 kcal · 4.31 km · 1 h 5 min active',
  )
})

test('speedRange disables the ends and auto mode', () => {
  assert.deepEqual(plain(M.speedRange(state())), { slower: true, faster: true })
  assert.deepEqual(
    plain(M.speedRange(state({ pad: pad({ targetKmh: 0.5 }) }))),
    { slower: false, faster: true },
  )
  assert.deepEqual(plain(M.speedRange(state({ pad: pad({ targetKmh: 6 }) }))), {
    slower: true,
    faster: false,
  })
  assert.deepEqual(plain(M.speedRange(state({ pad: pad({ mode: 'auto' }) }))), {
    slower: false,
    faster: false,
  })
  assert.deepEqual(plain(M.speedRange(null)), { slower: false, faster: false })
})

test('padActions offers what each pad situation allows', () => {
  const ids = (s) => plain(M.padActions(s)).map((a) => a.id)
  assert.deepEqual(ids(state({ bluetooth: 'blocked', pad: null })), [
    'bluetoothOn',
  ])
  assert.deepEqual(ids(state({ bluetooth: 'missing', pad: null })), [])
  assert.deepEqual(
    ids(state({ link: 'disconnected', device: null, pad: null })),
    ['scan'],
  )
  assert.deepEqual(
    ids(
      state({
        link: 'disconnected',
        device: null,
        pad: null,
        scan: { active: true, devices: [] },
      }),
    ),
    ['stopScan'],
  )
  assert.deepEqual(ids(state()), ['slower', 'start', 'faster', 'disconnect'])
  assert.deepEqual(ids(state({ pad: pad({ belt: 'running' }) })), ['stop'])
  assert.deepEqual(ids(state({ pad: pad({ busy: 'starting' }) })), ['stop'])
  assert.deepEqual(ids(state({ link: 'searching', pad: null })), ['disconnect'])
  assert.deepEqual(ids(state({ link: 'disconnected', pad: null })), [
    'connect',
    'forget',
  ])
  assert.deepEqual(ids(null), [])
  const start = M.padActions(state({ pad: pad({ targetKmh: 3 }) }))[1]
  assert.equal(start.label, 'Start · 3.0 km/h')
  assert.equal(start.enabled, true)
  assert.equal(
    M.padActions(state({ pad: pad({ imperial: true }) }))[1].enabled,
    false,
  )
  assert.equal(
    M.padActions(state({ pad: pad({ shared: true }) }))[1].enabled,
    false,
  )
})

test('scanRows lists pads first and hides the rest until asked', () => {
  const devices = [
    { address: 'A', name: 'KS-ST-A1P', rssi: -58, candidate: true },
    { address: 'B', name: 'Some Headphones', rssi: -40, candidate: false },
    { address: 'C', name: null, rssi: -80, candidate: false },
    { name: 'no address', candidate: true },
  ]
  const s = state({ scan: { active: true, devices } })
  const collapsed = M.scanRows(s, false)
  assert.deepEqual(
    plain(collapsed.rows).map((d) => d.address),
    ['A'],
  )
  assert.equal(collapsed.hidden, 2)
  const all = M.scanRows(s, true)
  assert.deepEqual(
    plain(all.rows).map((d) => d.address),
    ['A', 'B', 'C'],
  )
  assert.equal(all.hidden, 0)
  assert.equal(M.scanRows(null, false).rows.length, 0)
})

test('errorText words the pad refusals', () => {
  assert.match(M.errorText({ code: 'imperial-units' }), /miles/)
  assert.match(M.errorText({ code: 'in-use' }), /Another app/)
})

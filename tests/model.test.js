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

test('helperForFatal tells a missing build from a broken helper', () => {
  assert.equal(M.helperForFatal('helper-missing'), 'missing')
  assert.equal(M.helperForFatal('helper-outdated'), 'outdated')
  assert.equal(M.helperForFatal('already-running'), 'failed')
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

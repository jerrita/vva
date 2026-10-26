// Run: node --experimental-strip-types --test sessionSync.test.mjs
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { applySessionDelta, sessionSummary, subscribeSession } from './src/sessionSync.ts'

const snapshot = () => ({
  revision: 1, state: 'Responding...', persisted: true, notices: [], background_tasks: {},
  session: {
    id: 'synthetic', workspace_root: '/synthetic', created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z', current_model: 'test/model',
    current_variant: 'high', web_search: true,
    turns: [{ id: 'turn', role: 'user', messages: [{ parts: [{ type: 'text', text: 'sample' }] }] }],
  },
  draft: [{ type: 'reasoning', content: '思考' }, { type: 'text', text: '起点🙂' }],
})

test('task-only deltas update inline status without replacing transcript or draft', () => {
  const before = snapshot()
  const tasks = { task_sample: { task_id: 'task_sample', status: 'completed', output: 'ready', truncated: false } }
  const after = applySessionDelta(before, delta({ background_tasks: tasks }))
  assert.equal(after.background_tasks, tasks)
  assert.equal(after.session, before.session)
  assert.equal(after.draft, before.draft)
  assert.deepEqual(before.background_tasks, {})
  assert.deepEqual(applySessionDelta(after, delta({ base_revision: 5, revision: 6, background_tasks: {} })).background_tasks, {})
})
const delta = (extra = {}) => ({ base_revision: 1, revision: 5, state: 'Responding...', persisted: true, ...extra })

test('session list projection preserves cleared metadata and search configuration', () => {
  const before = snapshot()
  const { turns, ...info } = before.session
  const after = applySessionDelta(before, delta({
    session: {
      ...info, title: null, current_variant: null, parent: null, agent: null,
      web_search: false, editor: 'replace', depth: 2,
    },
    state: '',
  }))
  assert.deepEqual(sessionSummary(after.session, after.state), {
    id: 'synthetic', title: null, workspace_root: '/synthetic',
    created_at: '2026-01-01T00:00:00Z', turn_count: 1, current_model: 'test/model',
    current_variant: null, web_search: false, parent: null, agent: null, state: '',
  })
})

test('text/reasoning suffixes preserve history references and the old snapshot', () => {
  const before = snapshot()
  const after = applySessionDelta(before, delta({ draft: { len: 2, updates: [
    { type: 'reasoning', index: 0, delta: '继续' },
    { type: 'text', index: 1, delta: '新增🙂' },
  ] } }))
  assert.equal(after.session, before.session)
  assert.equal(after.draft[0].content, '思考继续')
  assert.equal(after.draft[1].text, '起点🙂新增🙂')
  assert.equal(before.draft[1].text, '起点🙂')
})

test('commit, optional metadata clear, and revert apply atomically', () => {
  const before = snapshot()
  const { current_variant, turns, ...info } = before.session
  const turn = { id: 'answer', role: 'assistant', messages: [{ parts: before.draft }] }
  const after = applySessionDelta(before, delta({
    session: info, turns: { from: 1, items: [turn] },
    draft: { len: 0, updates: [] }, state: '',
  }))
  assert.equal(after.session.current_variant, undefined)
  assert.equal(after.session.turns[0], before.session.turns[0])
  assert.equal(after.session.turns[1], turn)
  assert.deepEqual(after.draft, [])
  assert.equal(after.state, '')
  const reverted = applySessionDelta(after, delta({
    base_revision: 5, revision: 9, turns: { from: 0, items: [] },
  }))
  assert.deepEqual(reverted.session.turns, [])
})

test('part replacement preserves citations and supports relocation and growth', () => {
  const before = snapshot()
  const cited = { ...before.draft[1], annotations: [{
    type: 'url_citation', url: 'https://example.com', title: 'example', start_index: 0, end_index: 2,
  }] }
  const after = applySessionDelta(before, delta({ draft: { len: 3, updates: [
    { type: 'set', index: 0, part: cited },
    { type: 'set', index: 1, part: before.draft[0] },
    { type: 'set', index: 2, part: { type: 'text', text: 'new' } },
  ] } }))
  assert.deepEqual(after.draft, [cited, before.draft[0], { type: 'text', text: 'new' }])
})

test('invalid versions and malformed patches fail without changing the baseline', () => {
  const before = snapshot()
  for (const change of [
    { base_revision: 0 }, { revision: 1 },
    { turns: { from: 2, items: [] } },
    { draft: { len: 3, updates: [] } },
    { draft: { len: 2, updates: [{ type: 'text', index: 0, delta: 'bad' }] } },
  ]) assert.throws(() => applySessionDelta(before, delta(change)))
  assert.deepEqual(before, snapshot())
})

test('subscription resets on reconnect, rejects broken chains, and closes on cleanup', () => {
  const original = globalThis.EventSource
  let source
  class FakeEventSource {
    handlers = new Map()
    closed = false
    constructor() { source = this }
    addEventListener(name, handler) { this.handlers.set(name, handler) }
    close() { this.closed = true }
    emit(name, data) { this.handlers.get(name)({ data: JSON.stringify(data) }) }
  }
  globalThis.EventSource = FakeEventSource
  try {
    const received = [], errors = []
    const close = subscribeSession('synthetic', value => received.push(value), error => errors.push(error))
    source.emit('snapshot', snapshot())
    source.emit('delta', delta())
    // A new server connection can establish a lower revision after a process restart.
    source.emit('snapshot', { ...snapshot(), revision: 0 })
    source.emit('delta', delta({ base_revision: 0, revision: 2 }))
    assert.deepEqual(received.map(value => value.revision), [1, 5, 0, 2])
    source.emit('delta', delta())
    assert.equal(errors.length, 1)
    assert.equal(source.closed, true)
    close()
  } finally {
    globalThis.EventSource = original
  }
})

test('shell output deltas trim UTF-16 safely and preserve unrelated references', () => {
  const before = snapshot()
  const output = { revision: 1, start: 0, text: '🙂旧\n保留', total_bytes: 14, finished: false }
  const other = { ...output, text: 'other' }
  before.shell_outputs = { command: output, other }
  const after = applySessionDelta(before, delta({ shell_outputs: [{
    type: 'append', id: 'command', base_revision: 1, revision: 4,
    start: 8, drop: 4, text: '新增', total_bytes: 20, finished: false, error: null,
  }] }))
  assert.equal(after.shell_outputs.command.text, '保留新增')
  assert.equal(after.shell_outputs.other, other)
  assert.equal(after.session, before.session)
  assert.equal(after.draft, before.draft)
  assert.equal(before.shell_outputs.command, output)
  const finished = applySessionDelta(after, delta({ base_revision: 5, revision: 6,
    shell_outputs: [{ type: 'set', id: 'command', output: { ...output, revision: 5, text: '100%', finished: true } },
      { type: 'remove', id: 'other' }],
  }))
  assert.equal(finished.shell_outputs.command.text, '100%')
  assert.equal(finished.shell_outputs.command.finished, true)
  assert.equal(finished.shell_outputs.other, undefined)
})

test('shell output rejects stale revisions and invalid trim ranges', () => {
  const before = snapshot()
  before.shell_outputs = { command: { revision: 3, start: 0, text: 'ready', total_bytes: 5, finished: false } }
  for (const extra of [{ base_revision: 2 }, { revision: 3 }, { drop: 6 }, { drop: -1 }, { start: -1 }]) {
    assert.throws(() => applySessionDelta(before, delta({ shell_outputs: [{
      type: 'append', id: 'command', base_revision: 3, revision: 4, start: 0,
      drop: 0, text: '', total_bytes: 5, finished: true, error: null, ...extra,
    }] })), /命令输出增量/)
  }
  assert.equal(before.shell_outputs.command.text, 'ready')
})

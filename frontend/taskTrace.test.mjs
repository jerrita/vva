// Run: node --experimental-strip-types --test taskTrace.test.mjs
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { taskTrace, taskLabel } from './src/utils/taskTrace.ts'

const call = (id, name, args) => ({ type: 'tool_call', id, name, arguments: JSON.stringify(args) })
const result = (id, extra = {}) => ({ type: 'tool_result', tool_call_id: id, content: '', is_error: false, ...extra })

test('background queries attach to their launch across intervening actions', () => {
  const error = result('poll', { is_error: true, content: 'query rejected' })
  const parts = [
    call('launch', 'Shell', { run_in_background: true }),
    result('launch', { background_task_id: 'task_sample' }),
    call('other', 'Read', { path: 'sample.txt' }), result('other'),
    call('poll', 'TaskOutput', { task_id: 'task_sample' }), error,
    call('stop', 'TaskStop', { task_id: 'task_sample' }), result('stop'),
  ]
  const tasks = { task_sample: { task_id: 'task_sample', status: 'stopped', output: '', truncated: false } }
  const trace = taskTrace(parts, tasks)
  assert.equal(trace.tasks, tasks)
  assert.equal(trace.queryOwners.get('poll'), 'launch')
  assert.equal(trace.queryOwners.has('stop'), false)
  assert.equal(trace.queries.get('launch')[0].result, error)
})

test('follow-up conversations and unowned queries remain standalone', () => {
  const trace = taskTrace([
    call('agent', 'Agent', { run_in_background: true }),
    result('agent', { background_task_id: 'sess_sample' }),
    call('join', 'WaitSession', { session_id: 'sess_sample' }), result('join'),
    call('send', 'SendMessage', { session_id: 'sess_sample', text: 'next task' }), result('send'),
    call('followup', 'WaitSession', { session_id: 'sess_sample' }), result('followup'),
    call('orphan', 'TaskOutput', { task_id: 'task_missing' }), result('orphan'),
    { type: 'tool_call', id: 'streaming', name: 'TaskOutput', arguments: '{"task_id":' },
  ], {})
  assert.deepEqual([...trace.queryOwners], [['join', 'agent']])
})

test('task labels preserve failure exit codes and restart interruption', () => {
  assert.equal(taskLabel({ status: 'failed', exit_code: 7 }), '退出码 7')
  assert.equal(taskLabel({ status: 'interrupted' }), '已中断')
  assert.equal(taskLabel({ status: 'running' }), '后台运行中')
})

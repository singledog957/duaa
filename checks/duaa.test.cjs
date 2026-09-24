const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const vm = require('node:vm')
const path = require('node:path')

test('userscript bridge matches the current frontend contract', async () => {
  const values = new Map()
  const requests = []
  const window = {}
  window.top = window
  window.self = window
  const context = {
    window,
    unsafeWindow: window,
    URLSearchParams,
    console,
    GM: {
      getValue: async (key, fallback) => values.get(key) ?? fallback,
      setValue: async (key, value) => { values.set(key, value) },
    },
    GM_xmlhttpRequest(details) {
      requests.push(details)
      let body = { STATUS: '0', result: {} }
      if (details.url.includes('login_buaa.do')) body.result = { id: 'class-1', realName: '张三' }
      if (details.url.includes('get_stu_course_sched.action')) body.result = [
        { id: 's1', courseId: '11', courseName: '高数', teacherName: '甲', classBeginTime: '2026-09-28 08:00:00', signStatus: '0' },
      ]
      if (details.url.includes('get_timestamp.action')) body = { STATUS: '0', timestamp: 12345 }
      if (details.url.includes('stu_scan_sign.action')) body.result = { stuSignStatus: '1' }
      details.onload({ status: 200, responseText: JSON.stringify(body) })
    },
  }
  const source = fs.readFileSync(path.join(__dirname, '..', 'duaa.js'), 'utf8')
  vm.runInNewContext(source, context)
  const bridge = window.__checkinBridge
  assert.equal(await bridge.probeAvailability(), true)
  const day = await bridge.querySchedule(' AbC ', '20260928')
  assert.equal(day.student_name, '张三')
  assert.equal(day.schedules[0].course_id, '11')
  assert.equal(day.schedules[0].status, 0)
  assert.equal(bridge.matchSchedule(day.schedules[0], ['{"course_id":"12","name":"高数"}']), true)
  assert.ok(values.has('tk:abc'))
  await bridge.checkin('ABC', 's1')
  assert.ok(requests.some((r) => r.url.includes('timestamp=12345')))
  await bridge.saveLoginName('ABC', 'override')
  await bridge.querySchedule('abc', '20260928')
  assert.ok(requests.some((r) => r.url.includes('phone=override')))
})

const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const test = require('node:test');

const source = fs.readFileSync(path.join(__dirname, '../deploy/dashboard/static/app.js'), 'utf8')
  .split('/* ── Event Bindings')[0];

function dashboard() {
  let rows = [];
  const fields = {
    'config-poll-interval': { value: '5' },
    'config-auto-window': { value: '15' },
    'config-log-file': { value: 'logs/duaa.log' },
  };
  const document = {
    getElementById: id => fields[id],
    querySelectorAll: () => rows,
  };
  const app = vm.runInNewContext(source + '\n({state, splitList, studentCardHtml, syncDomToConfigState})', { document });
  app.state.config = { students: [] };
  const decode = s => s.replace(/&(amp|lt|gt|quot|#39);/g,
    (_, name) => ({ amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" }[name]));
  app.renderRows = (students, channel = '') => {
    rows = students.map((student, i) => {
      const html = app.studentCardHtml(student, i);
      const values = Object.fromEntries([...html.matchAll(/data-field="([^"]+)"[^>]*value="([^"]*)"/g)]
        .map(([, name, value]) => [name, { value: decode(value) }]));
      return { querySelector: selector => selector === '[data-channel]'
        ? { value: channel } : values[selector.match(/data-field="([^"]+)"/)[1]] };
    });
  };
  return app;
}

test('saving legacy JSON course targets preserves IDs, names, and delimiters', () => {
  const app = dashboard();
  const course_ids = [
    JSON.stringify({ course_id: '91999', name: '高等数学，进阶' }),
    JSON.stringify({ course_id: '92000', name: 'A, "B" \\ C' }),
    '英语',
  ];
  app.state.config.students = [{ student_id: 's1', course_ids }];
  app.renderRows(app.state.config.students);
  app.syncDomToConfigState();
  assert.deepEqual(Array.from(app.state.config.students[0].course_ids), course_ids);
  assert.deepEqual(Array.from(app.splitList('体育，91999\n英语, *')), ['体育', '91999', '英语', '*']);
});

test('channel changes and adding/removing rows preserve unsaved passwords exactly', () => {
  const app = dashboard();
  const password = '  new "password" & <secret>  ';
  const student = { student_id: 's1', password, has_password: true, course_ids: ['*'] };
  app.state.config.students = [student];
  app.renderRows(app.state.config.students, 'http://127.0.0.1:9101/secret');
  app.syncDomToConfigState();
  app.state.config.students.push({ student_id: 's2', password: 'second', course_ids: ['*'] });
  app.renderRows(app.state.config.students);
  app.syncDomToConfigState();
  app.state.config.students.splice(1, 1);
  app.renderRows(app.state.config.students);
  app.syncDomToConfigState();
  assert.equal(app.state.config.students[0].password, password);
});

test('an unchanged saved password stays empty in the browser', () => {
  const app = dashboard();
  app.state.config.students = [{ student_id: 's1', has_password: true, password: '', course_ids: ['*'] }];
  app.renderRows(app.state.config.students);
  app.syncDomToConfigState();
  assert.equal(app.state.config.students[0].password, '');
});

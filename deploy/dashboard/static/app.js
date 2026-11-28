'use strict';

const $ = (id) => document.getElementById(id);

const VIEWS = {
  overview: { title: '系统总览', subtitle: '自动签到服务、8081 账号容器与宿主机运行状态' },
  accounts: { title: '8081 账号容器', subtitle: '创建或删除企业微信容器，扫码登录并验证 /secret 凭据接口' },
  config: { title: '签到配置', subtitle: '按学生灵活选择 8081 官方签名模式或 8082 直连模式，支持热加载' },
  infra: { title: '监控与容器', subtitle: '宿主机资源指标与 Docker Compose 容器状态' },
  logs: { title: '签到日志', subtitle: '实时查看 duaa 自动签到与热加载运行记录' },
};

const state = {
  view: 'overview',
  overview: null,
  config: null,
  logs: null,
  logLevel: 'all',
  zoomAccount: null,
  deleteTarget: null,
  imageVersions: {},
  loading: false,
  logsLoading: false,
  configLoading: false,
  ttlEnabled: false,
  ttlDeadlineMs: null,
};

const esc = (v) => String(v ?? '').replace(/[&<>'"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', "'": '&#39;', '"': '&quot;' }[c]));
const pct = (v) => Number.isFinite(Number(v)) ? Math.max(0, Math.min(100, Number(v))) : 0;
const pill = (kind, text) => `<span class="pill ${kind}">${esc(text)}</span>`;
const localTime = (v) => v ? new Date(v).toLocaleString('zh-CN', { hour12: false }) : '--';

const bytes = (v, perSec = false) => {
  if (v === null || v === undefined || !Number.isFinite(Number(v))) return '--';
  let n = Number(v), u = 0;
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  while (Math.abs(n) >= 1024 && u < units.length - 1) { n /= 1024; u++; }
  return `${n.toFixed(u === 0 ? 0 : n >= 100 ? 0 : 1)} ${units[u]}${perSec ? '/s' : ''}`;
};

const duration = (sec) => {
  if (!Number.isFinite(Number(sec))) return '--';
  const d = Math.floor(sec / 86400), h = Math.floor(sec % 86400 / 3600), m = Math.floor(sec % 3600 / 60);
  return d ? `${d} 天 ${h} 小时` : h ? `${h} 小时 ${m} 分钟` : `${m} 分钟`;
};

function toast(msg) {
  const el = $('toast');
  el.textContent = msg;
  el.classList.remove('hidden');
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => el.classList.add('hidden'), 5000);
}

async function api(path, options = {}) {
  const response = await fetch(path, { cache: 'no-store', ...options });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
  return body;
}

async function apiJson(path, method, payload) {
  return api(path, {
    method,
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload ?? {}),
  });
}

function qrUrl(name) {
  const v = state.imageVersions[name] || (state.imageVersions[name] = Date.now());
  return `/qr/${encodeURIComponent(name)}.png?v=${v}`;
}

/* ── View Router ──────────────────────────────────────────────────────────── */

function switchView(name) {
  if (!VIEWS[name]) return;
  state.view = name;
  document.querySelectorAll('.nav-item').forEach(btn => {
    btn.classList.toggle('active', btn.dataset.view === name);
  });
  document.querySelectorAll('.view').forEach(sec => {
    sec.classList.toggle('active', sec.id === `view-${name}`);
  });
  $('page-title').textContent = VIEWS[name].title;
  $('page-subtitle').textContent = VIEWS[name].subtitle;
  if (name === 'logs') loadLogs(false);
  if (name === 'config' && !state.config) loadConfig(false);
}

/* ── Overview ─────────────────────────────────────────────────────────────── */


function syncAutoShutdown(info) {
  if (!info || !info.enabled || info.remaining_seconds === null || info.remaining_seconds === undefined) {
    state.ttlEnabled = false;
    state.ttlDeadlineMs = null;
    $('ttl-box').classList.add('hidden');
    return;
  }
  state.ttlEnabled = true;
  state.ttlDeadlineMs = Date.now() + Number(info.remaining_seconds) * 1000;
  $('ttl-box').classList.remove('hidden');
  renderTtlCountdown();
}

function renderTtlCountdown() {
  if (!state.ttlEnabled || !state.ttlDeadlineMs) return;
  const rem = Math.max(0, Math.floor((state.ttlDeadlineMs - Date.now()) / 1000));
  const h = Math.floor(rem / 3600);
  const m = Math.floor((rem % 3600) / 60);
  const s = rem % 60;
  $('ttl-countdown').textContent = h > 0
    ? `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
    : `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
  if (rem === 0) {
    $('global-status').className = 'pill neutral';
    $('global-status').textContent = '看板已自动关闭';
  }
}

function renderOverviewTab() {
  const data = state.overview || {};
  const host = data.host || {};
  const compose = data.compose || {};
  const accounts = data.accounts || [];
  const duaa = (compose.containers || []).find(c => c.service === 'duaa' || c.name === 'duaa');

  const duaaRunning = duaa?.state?.toLowerCase() === 'running';
  $('kpi-duaa').textContent = duaaRunning ? '运行中' : '未运行';
  $('kpi-duaa-sub').textContent = duaa?.status || '请检查 duaa 容器';

  const accRunning = accounts.filter(a => a.container_state === 'running').length;
  $('kpi-accounts').textContent = `${accRunning} / ${accounts.length}`;
  $('kpi-accounts-sub').textContent = accounts.length ? '个 8081 账号容器在线' : '尚未创建 8081 账号容器';
  $('nav-badge-accounts').textContent = String(accounts.length);

  const students = (state.config && state.config.students) || [];
  const cnt8081 = students.filter(s => Boolean(s.secret_url)).length;
  const cnt8082 = students.length - cnt8081;
  $('kpi-students').textContent = `${students.length} 人`;
  $('kpi-students-sub').textContent = `8081 官方 ${cnt8081} 人 · 8082 直连 ${cnt8082} 人`;
  $('nav-badge-students').textContent = String(students.length);

  const mem = host.memory || {};
  $('kpi-host').textContent = host.ok ? (host.cpu_percent === null ? '在线' : `CPU ${host.cpu_percent}%`) : '异常';
  $('kpi-host-sub').textContent = host.ok ? `${host.hostname} · 内存 ${mem.used_percent ?? '--'}% · 负载 ${(host.load_average || [])[0] ?? '--'}` : (host.error || '采集失败');

  const healthy = host.ok && compose.ok && duaaRunning;
  $('global-status').className = `pill ${healthy ? 'ok' : 'warn'}`;
  $('global-status').textContent = healthy ? '系统运行正常' : '需关注状态';
  $('updated-at').textContent = `更新于 ${localTime(data.generated_at)}`;
  syncAutoShutdown(data.auto_shutdown);
}

/* ── 8081 Accounts Tab ────────────────────────────────────────────────────── */

function healthChip(label, ok) {
  return `<div class="health-chip ${ok ? 'ok' : ''}"><i></i>${esc(label)}</div>`;
}

function renderAccountsTab() {
  const accounts = (state.overview && state.overview.accounts) || [];
  const grid = $('accounts-grid');
  if (!accounts.length) {
    grid.innerHTML = '<div class="empty-state">暂无 8081 账号容器。可在上方填写实例名（如 stu1）一键创建；如果仅使用 8082 直连模式则无需创建容器。</div>';
    return;
  }
  grid.innerHTML = accounts.map(acc => {
    const running = acc.container_state === 'running';
    const h = acc.health || {};
    const providerOk = running && !h.error && h.ok !== false;
    const online = running && Boolean(h.wwlocal && h.tproxy);
    const statusPill = !running ? pill('neutral', '已停止') : online ? pill('ok', '组件正常') : pill('warn', '需检查');
    const preview = running
      ? `<button class="screen-btn" data-zoom="${esc(acc.name)}" title="点击放大扫码">
           <img src="${qrUrl(acc.name)}" alt="${esc(acc.name)} 登录画面" loading="lazy">
           <span class="screen-badge">点击放大 · ${new Date().toLocaleTimeString('zh-CN', { hour12: false })}</span>
         </button>`
      : `<div class="screen-offline">容器已停止，点击下方“启动”恢复运行</div>`;

    return `<article class="account-card">
      <div class="account-head">
        <div class="account-title">
          <strong>${esc(acc.name)}</strong>
          <span class="port-tag">127.0.0.1:${esc(acc.http_port || '--')}</span>
        </div>
        ${statusPill}
      </div>
      ${preview}
      <div class="account-body">
        <div class="health-row">
          ${healthChip('Provider', providerOk)}
          ${healthChip('企业微信', Boolean(h.wwlocal))}
          ${healthChip('渲染进程', Boolean(h.renderer))}
          ${healthChip('透明代理', Boolean(h.tproxy))}
        </div>
        ${h.error && running ? `<div class="alert error">${esc(h.error)}</div>` : ''}
        <div class="account-toolbar">
          <div class="btn-group">
            <button class="btn ghost sm" data-ui-action="qr" data-acc="${esc(acc.name)}" ${running ? '' : 'disabled'}>弹出二维码</button>
            <button class="btn ghost sm" data-ui-action="login" data-acc="${esc(acc.name)}" ${running ? '' : 'disabled'}>点 Login</button>
            <button class="btn ghost sm" data-test-secret="${esc(acc.name)}" ${running ? '' : 'disabled'}>测 Secret</button>
            <button class="btn ghost sm" data-refresh-qr="${esc(acc.name)}" ${running ? '' : 'disabled'}>刷新</button>
          </div>
          <div class="btn-group">
            ${running
              ? `<button class="btn ghost sm" data-lifecycle="stop" data-acc="${esc(acc.name)}">停止</button>`
              : `<button class="btn primary sm" data-lifecycle="start" data-acc="${esc(acc.name)}">启动</button>`}
            <button class="btn ghost sm" data-lifecycle="restart" data-acc="${esc(acc.name)}">重启</button>
            <button class="btn danger sm" data-delete-acc="${esc(acc.name)}">删除</button>
          </div>
        </div>
      </div>
    </article>`;
  }).join('');
}

/* ── Config Tab (Unified 8081 / 8082) ─────────────────────────────────────── */

function splitList(v) {
  // config.sh stores JSON objects as strings. Delimiters inside those objects
  // (including escaped quotes in course names) belong to the entry itself.
  const entries = [];
  let entry = '', depth = 0, quoted = false, escaped = false;
  for (const ch of String(v || '')) {
    if (quoted) {
      if (escaped) escaped = false;
      else if (ch === '\\') escaped = true;
      else if (ch === '"') quoted = false;
    } else if (depth > 0 && ch === '"') {
      quoted = true;
    } else if (ch === '{' || ch === '[') {
      depth++;
    } else if (ch === '}' || ch === ']') {
      depth = Math.max(0, depth - 1);
    } else if (depth === 0 && /[,，\n]/.test(ch)) {
      if (entry.trim()) entries.push(entry.trim());
      entry = '';
      continue;
    }
    entry += ch;
  }
  if (entry.trim()) entries.push(entry.trim());
  return entries;
}

function channelSelectHtml(currentUrl, idx) {
  const accounts = (state.overview && state.overview.accounts) || [];
  const knownUrls = new Set(accounts.map(a => a.secret_url).filter(Boolean));
  const isCustom = Boolean(currentUrl && !knownUrls.has(currentUrl));
  const opts = [`<option value="" ${!currentUrl ? 'selected' : ''}>8082 直连（无需容器）</option>`];
  for (const acc of accounts) {
    if (!acc.secret_url) continue;
    const sel = currentUrl === acc.secret_url ? 'selected' : '';
    const st = acc.container_state === 'running' ? '' : ' · 已停';
    opts.push(`<option value="${esc(acc.secret_url)}" ${sel}>8081 · ${esc(acc.name)} (:${esc(acc.http_port)}${st})</option>`);
  }
  opts.push(`<option value="__custom__" ${isCustom ? 'selected' : ''}>8081 · 自定义 Provider URL…</option>`);
  return { html: `<select data-channel="${idx}">${opts.join('')}</select>`, isCustom };
}

function channelBadge(url) {
  if (!url) return pill('ok', '8082 直连');
  const accounts = (state.overview && state.overview.accounts) || [];
  const hit = accounts.find(a => a.secret_url === url);
  return pill('accent', hit ? `8081 · ${hit.name}` : '8081 自定义');
}

function studentCardHtml(s, idx) {
  const url = (s.secret_url || '').trim();
  const pwHint = s.has_password ? '留空保持原密码' : '必填（统一认证密码）';
  const ch = channelSelectHtml(url, idx);
  const showCustom = ch.isCustom || s._customUrl;

  return `<article class="student-card" data-index="${idx}">
    <div class="student-head">
      <div class="student-title">
        <strong>${esc(s.student_id || '新学生')}</strong>
        ${s.name && s.name !== s.student_id ? `<span class="muted small">${esc(s.name)}</span>` : ''}
        ${channelBadge(url)}
      </div>
      <button class="btn danger sm" type="button" data-remove-student="${idx}">移除</button>
    </div>

    <div class="student-grid">
      <label>
        <span>学号</span>
        <input data-field="student_id" type="text" value="${esc(s.student_id || '')}" placeholder="22373000" autocomplete="off">
      </label>
      <label>
        <span>备注姓名（可选）</span>
        <input data-field="name" type="text" value="${esc(s.name || '')}" placeholder="留空默认用学号" autocomplete="off">
      </label>
      <label>
        <span>统一认证（SSO）密码</span>
        <input data-field="password" type="password" value="${esc(s.password || '')}" placeholder="${esc(pwHint)}" autocomplete="new-password">
      </label>

      <label>
        <span>签到方式（选容器即走 8081，失败回退 8082）</span>
        ${ch.html}
      </label>
      <label>
        <span>签到课程（* 表示全签，逗号分隔）</span>
        <input data-field="course_ids" type="text" value="${esc((s.course_ids || ['*']).join(', '))}" placeholder="* 或 91999, 高等数学" autocomplete="off">
      </label>
      <label>
        <span>排除课程（可选，优先跳过）</span>
        <input data-field="exclude_courses" type="text" value="${esc((s.exclude_courses || []).join(', '))}" placeholder="体育, 91998" autocomplete="off">
      </label>

      <label class="custom-url-row ${showCustom ? '' : 'hidden'}" data-custom-wrap="${idx}">
        <span>自定义 8081 Secret Provider URL</span>
        <input data-field="secret_url" type="text" value="${esc(url)}" placeholder="http://127.0.0.1:9101/secret" autocomplete="off">
      </label>
    </div>
  </article>`;
}

function syncDomToConfigState() {
  if (!state.config) return;
  const rows = [...document.querySelectorAll('.student-card')];
  if (!rows.length && (state.config.students || []).length) return;
  const prevStudents = state.config.students || [];
  state.config = {
    ...state.config,
    poll_interval_minutes: Number($('config-poll-interval').value) || state.config.poll_interval_minutes,
    auto_window_minutes: Number($('config-auto-window').value) || state.config.auto_window_minutes,
    log_file: $('config-log-file').value.trim() || 'logs/duaa.log',
    students: rows.map((row, i) => {
      const val = (f) => {
        const el = row.querySelector(`[data-field="${f}"]`);
        return el ? el.value.trim() : '';
      };
      const sel = row.querySelector('[data-channel]');
      const chVal = sel ? sel.value : '';
      const secretUrl = chVal === '__custom__' ? val('secret_url') : chVal;
      const prev = prevStudents[i] || {};
      return {
        student_id: val('student_id'),
        name: val('name'),
        password: row.querySelector('[data-field="password"]')?.value || '',
        has_password: Boolean(prev.has_password || val('password')),
        secret_url: secretUrl,
        _customUrl: chVal === '__custom__',
        course_ids: splitList(val('course_ids')),
        exclude_courses: splitList(val('exclude_courses')),
      };
    }),
  };
}

function renderConfigTab() {
  const cfg = state.config;
  if (!cfg) return;
  $('config-poll-interval').value = cfg.poll_interval_minutes;
  $('config-auto-window').value = cfg.auto_window_minutes;
  $('config-log-file').value = cfg.log_file || 'logs/duaa.log';
  $('config-meta').textContent = `${cfg.path || 'config.json'} · ${cfg.modified_at ? '更新于 ' + localTime(cfg.modified_at) : '尚未保存'}`;
  const students = cfg.students || [];
  $('config-students').innerHTML = students.length
    ? students.map(studentCardHtml).join('')
    : '<div class="empty-state">尚未配置学生，点击右上角“+ 新增学生”开始。</div>';
  $('config-error').classList.add('hidden');
  renderOverviewTab();
}

/* ── Infra Tab ────────────────────────────────────────────────────────────── */

function metricCard(label, val, sub, percent) {
  const n = percent === null ? null : pct(percent);
  const lvl = n === null ? '' : n >= 90 ? 'bad' : n >= 75 ? 'warn' : '';
  return `<article class="metric-card">
    <div class="metric-top"><span>${esc(label)}</span><span>${n !== null ? n.toFixed(1) + '%' : ''}</span></div>
    <div class="metric-val">${val}</div>
    ${n !== null ? `<div class="bar"><span class="${lvl}" style="width:${n}%"></span></div>` : ''}
    <div class="metric-sub">${sub}</div>
  </article>`;
}

function renderInfraTab() {
  const data = state.overview || {};
  const h = data.host || {};
  if (!h.ok) {
    $('host-metrics').innerHTML = `<div class="alert error">${esc(h.error || '无法采集宿主机指标')}</div>`;
    $('host-details').innerHTML = '';
  } else {
    const mem = h.memory || {}, disk = (h.disks || [])[0] || {}, net = h.network || {}, load = h.load_average || [];
    $('host-caption').textContent = `${h.hostname} · ${h.os} · ${h.kernel}`;
    $('host-metrics').innerHTML = [
      metricCard('CPU 使用率', h.cpu_percent === null ? '采样中' : `${Number(h.cpu_percent).toFixed(1)}%`, `${h.cpu_count} 核 · 负载 ${load.map(v => Number(v).toFixed(2)).join(' / ')}`, h.cpu_percent),
      metricCard('内存', bytes(mem.used_bytes), `总计 ${bytes(mem.total_bytes)} · 可用 ${bytes(mem.available_bytes)}`, mem.used_percent),
      metricCard('磁盘', bytes(disk.used_bytes), `${esc(disk.path || '/')} · 剩余 ${bytes(disk.free_bytes)}`, disk.used_percent),
      metricCard('网络接收', bytes(net.receive_bytes_per_second, true), `累计 ${bytes(net.received_bytes)}`, null),
      metricCard('网络发送', bytes(net.send_bytes_per_second, true), `累计 ${bytes(net.sent_bytes)}`, null),
    ].join('');
    const swap = mem.swap_total_bytes ? `${bytes(mem.swap_used_bytes)} / ${bytes(mem.swap_total_bytes)}` : '未启用';
    const temps = (h.temperatures || []).map(t => `${esc(t.name)} <strong>${esc(t.celsius)}°C</strong>`).join(' · ') || '无传感器数据';
    $('host-details').innerHTML = `
      <div class="detail-box">运行时间：<strong>${duration(h.uptime_seconds)}</strong><br>架构：<strong>${esc(h.architecture)}</strong> · Swap：<strong>${swap}</strong></div>
      <div class="detail-box">磁盘空间${(h.disks || []).map(d => `<br>${esc(d.path)}：<strong>${bytes(d.free_bytes)}</strong> 可用 / ${bytes(d.total_bytes)}`).join('')}</div>
      <div class="detail-box">温度<br>${temps}</div>`;
  }

  const c = data.compose || {}, counts = c.counts || {};
  $('compose-caption').textContent = `${counts.running || 0} / ${counts.total || 0} 个容器运行中`;
  $('compose-error').classList.toggle('hidden', !c.error);
  $('compose-error').textContent = c.error || '';
  $('compose-containers').innerHTML = (c.containers || []).length
    ? c.containers.map(item => {
        const running = item.state?.toLowerCase() === 'running';
        let opBtn = '<span class="muted small">—</span>';
        if (item.name === 'duaa') {
          opBtn = '<button class="btn ghost sm" data-restart-duaa="1" type="button">重启</button>';
        } else if (item.name.startsWith('duaa-acc-')) {
          const accName = item.name.slice('duaa-acc-'.length);
          opBtn = `<button class="btn ghost sm" data-lifecycle="restart" data-acc="${esc(accName)}" type="button">重启</button>`;
        }
        return `<tr>
          <td><strong>${esc(item.project || '--')}</strong><small>${esc(item.service || '--')}</small></td>
          <td><strong>${esc(item.name || '--')}</strong><small>${esc(item.status || '')}</small></td>
          <td>${pill(running ? 'ok' : 'bad', item.state || '--')}</td>
          <td>${esc(item.image ? item.image.replace('sha256:', '').slice(0, 24) : '--')}</td>
          <td>${esc(item.ports || '--')}</td>
          <td>${opBtn}</td>
        </tr>`;
      }).join('')
    : '<tr><td colspan="6" class="empty-cell">未发现关联容器</td></tr>';
}

/* ── Logs Tab ─────────────────────────────────────────────────────────────── */

function logClass(line) {
  const l = line.toLowerCase();
  if (/checkin ok|stusignstatus["']?[:=]["']?1|签到成功|config reloaded/.test(l)) return 'success';
  if (/error|failed|panic|exception|签到失败/.test(l)) return 'error';
  if (/warn|retry|重试/.test(l)) return 'warn';
  return 'info';
}

function renderLogsTab() {
  const data = state.logs;
  if (!data) return;
  $('log-path').textContent = data.name || '--';
  $('log-error').classList.toggle('hidden', !data.error);
  $('log-error').textContent = data.error || '';

  const all = data.lines || [];
  const preview = all.slice(-10);
  $('overview-logs').innerHTML = preview.length
    ? preview.map(line => `<span class="log-line ${logClass(line)}">${esc(line)}</span>`).join('')
    : '<span class="muted">暂无日志</span>';

  const kw = $('log-filter').value.trim();
  const filtered = all.filter(line => {
    const cls = logClass(line);
    if (state.logLevel !== 'all' && cls !== state.logLevel) return false;
    if (kw && !line.toLowerCase().includes(kw.toLowerCase())) return false;
    return true;
  });
  const hl = kw ? new RegExp(`(${kw.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')})`, 'ig') : null;
  $('logs').innerHTML = filtered.length
    ? filtered.map(line => `<span class="log-line ${logClass(line)}">${hl ? esc(line).replace(hl, '<mark>$1</mark>') : esc(line)}</span>`).join('')
    : '<span class="muted">没有匹配的日志记录</span>';
  $('log-stats').textContent = `显示 ${filtered.length} / ${all.length} 行 · ${bytes(data.size_bytes || 0)} · ${localTime(data.read_at)}`;
  if ($('log-follow').checked) $('logs').scrollTop = $('logs').scrollHeight;
}

/* ── Data Loaders ─────────────────────────────────────────────────────────── */

async function loadOverview(showErr = true) {
  if (state.loading) return;
  state.loading = true;
  $('refresh-all').disabled = true;
  try {
    state.overview = await api('/api/overview');
    renderOverviewTab();
    renderAccountsTab();
    renderInfraTab();
    if (state.view !== 'config' && state.config) renderConfigTab();
  } catch (err) {
    $('global-status').className = 'pill bad';
    $('global-status').textContent = '连接异常';
    if (showErr) toast(`刷新失败：${err.message}`);
  } finally {
    state.loading = false;
    $('refresh-all').disabled = false;
  }
}

async function loadConfig(showErr = true) {
  if (state.configLoading) return;
  state.configLoading = true;
  try {
    state.config = await api('/api/config');
    renderConfigTab();
  } catch (err) {
    $('config-error').textContent = `配置读取失败：${err.message}`;
    $('config-error').classList.remove('hidden');
    if (showErr) toast(`配置读取失败：${err.message}`);
  } finally {
    state.configLoading = false;
  }
}

async function loadLogs(showErr = true) {
  if (state.logsLoading) return;
  state.logsLoading = true;
  try {
    const lines = $('log-lines').value || '200';
    state.logs = await api(`/api/logs?lines=${encodeURIComponent(lines)}`);
    renderLogsTab();
  } catch (err) {
    if (showErr) toast(`日志读取失败：${err.message}`);
  } finally {
    state.logsLoading = false;
  }
}

/* ── Actions ──────────────────────────────────────────────────────────────── */

function refreshQr(name) {
  state.imageVersions[name] = Date.now();
  renderAccountsTab();
  if (state.zoomAccount === name) refreshZoom();
}

function refreshZoom() {
  if (!state.zoomAccount) return;
  state.imageVersions[state.zoomAccount] = Date.now();
  $('zoom-image').src = qrUrl(state.zoomAccount);
  $('zoom-time').textContent = `更新于 ${new Date().toLocaleTimeString('zh-CN', { hour12: false })}`;
}

function openZoom(name) {
  state.zoomAccount = name;
  $('zoom-title').textContent = `${name} · 企业微信登录画面`;
  $('zoom').classList.remove('hidden');
  refreshZoom();
}

function closeZoom() {
  state.zoomAccount = null;
  $('zoom').classList.add('hidden');
}

function openDeleteModal(name) {
  state.deleteTarget = name;
  $('delete-title').textContent = `删除账号容器：${name}`;
  $('delete-desc').textContent = `确认停止并删除容器 duaa-acc-${name} 及其 Compose 实例目录吗？`;
  $('delete-remove-volume').checked = false;
  $('delete-modal').classList.remove('hidden');
}

function closeDeleteModal() {
  state.deleteTarget = null;
  $('delete-modal').classList.add('hidden');
}

async function triggerUiAction(name, action, btn) {
  if (btn) btn.disabled = true;
  try {
    await apiJson(`/api/accounts/${encodeURIComponent(name)}/action`, 'POST', { action });
    toast(action === 'qr' ? `${name} 已切换/刷新二维码，画面马上更新` : `${name} 已点击 Login，请在手机企业微信确认`);
    setTimeout(() => refreshQr(name), 1200);
    setTimeout(() => refreshQr(name), 3200);
  } catch (err) {
    toast(`操作失败：${err.message}`);
  } finally {
    if (btn) btn.disabled = false;
  }
}

async function testSecret(name, btn) {
  if (btn) btn.disabled = true;
  toast(`正在测试 ${name} 的 /secret 接口（需数秒）…`);
  try {
    const res = await apiJson(`/api/accounts/${encodeURIComponent(name)}/test-secret`, 'POST', {});
    toast(`${name} 8081 凭据获取成功（${res.elapsed_seconds}s，taskid=${res.taskid_preview}）`);
  } catch (err) {
    toast(`${name} 测试失败：${err.message}`);
  } finally {
    if (btn) btn.disabled = false;
  }
}

async function lifecycleOp(name, op, btn) {
  if (btn) btn.disabled = true;
  try {
    await apiJson(`/api/accounts/${encodeURIComponent(name)}/lifecycle`, 'POST', { op });
    toast(`容器 ${name} 已执行 ${op}`);
    setTimeout(() => loadOverview(false), 1200);
  } catch (err) {
    toast(`容器操作失败：${err.message}`);
  } finally {
    if (btn) btn.disabled = false;
  }
}

async function restartDuaa(btn) {
  if (btn) btn.disabled = true;
  try {
    await apiJson('/api/duaa/restart', 'POST', {});
    toast('duaa 容器已重启');
    setTimeout(() => { loadOverview(false); loadLogs(false); }, 1500);
  } catch (err) {
    toast(`重启失败：${err.message}`);
  } finally {
    if (btn) btn.disabled = false;
  }
}

async function saveConfig() {
  syncDomToConfigState();
  $('save-config').disabled = true;
  try {
    const res = await apiJson('/api/config', 'PUT', state.config);
    await loadConfig(false);
    toast(res.restart_required
      ? '配置已保存！修改了全局参数，请点击“重启 duaa”生效。'
      : '配置已保存！学生配置将在下一次轮询自动热加载。');
  } catch (err) {
    $('config-error').textContent = `保存失败：${err.message}`;
    $('config-error').classList.remove('hidden');
    toast(`保存失败：${err.message}`);
  } finally {
    $('save-config').disabled = false;
  }
}

function addStudent() {
  syncDomToConfigState();
  state.config = {
    ...(state.config || { poll_interval_minutes: 5, auto_window_minutes: 15, log_file: 'logs/duaa.log', students: [] }),
    students: [
      ...((state.config && state.config.students) || []),
      {
        student_id: '',
        name: '',
        password: '',
        has_password: false,
        secret_url: '',
        course_ids: ['*'],
        exclude_courses: [],
      },
    ],
  };
  renderConfigTab();
}

/* ── Event Bindings ───────────────────────────────────────────────────────── */

document.querySelectorAll('.nav-item').forEach(btn => {
  btn.addEventListener('click', () => switchView(btn.dataset.view));
});
document.querySelectorAll('[data-jump]').forEach(btn => {
  btn.addEventListener('click', () => switchView(btn.dataset.jump));
});

$('refresh-all').addEventListener('click', () => { loadOverview(); loadLogs(false); loadConfig(false); });
$('refresh-logs').addEventListener('click', () => loadLogs());
$('quick-restart-duaa').addEventListener('click', (e) => restartDuaa(e.currentTarget));
$('restart-duaa-btn').addEventListener('click', (e) => restartDuaa(e.currentTarget));

$('create-account-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  const name = $('new-acc-name').value.trim().toLowerCase();
  const portRaw = $('new-acc-port').value.trim();
  $('create-acc-btn').disabled = true;
  try {
    const res = await apiJson('/api/accounts', 'POST', { name, http_port: portRaw ? Number(portRaw) : null });
    $('new-acc-name').value = '';
    $('new-acc-port').value = '';
    toast(`已创建并启动 ${res.name}（127.0.0.1:${res.http_port}），几秒后可点击画面扫码`);
    await loadOverview(false);
    renderConfigTab();
  } catch (err) {
    toast(`创建失败：${err.message}`);
  } finally {
    $('create-acc-btn').disabled = false;
  }
});

$('accounts-grid').addEventListener('click', (e) => {
  const ui = e.target.closest('[data-ui-action]');
  if (ui) return triggerUiAction(ui.dataset.acc, ui.dataset.uiAction, ui);
  const ts = e.target.closest('[data-test-secret]');
  if (ts) return testSecret(ts.dataset.testSecret, ts);
  const lc = e.target.closest('[data-lifecycle]');
  if (lc) return lifecycleOp(lc.dataset.acc, lc.dataset.lifecycle, lc);
  const del = e.target.closest('[data-delete-acc]');
  if (del) return openDeleteModal(del.dataset.deleteAcc);
  const ref = e.target.closest('[data-refresh-qr]');
  if (ref) return refreshQr(ref.dataset.refreshQr);
  const zm = e.target.closest('[data-zoom]');
  if (zm) return openZoom(zm.dataset.zoom);
});

$('compose-containers').addEventListener('click', (e) => {
  const rd = e.target.closest('[data-restart-duaa]');
  if (rd) return restartDuaa(rd);
  const lc = e.target.closest('[data-lifecycle]');
  if (lc) return lifecycleOp(lc.dataset.acc, lc.dataset.lifecycle, lc);
});

$('add-student').addEventListener('click', addStudent);
$('reset-config').addEventListener('click', () => loadConfig());
$('save-config').addEventListener('click', saveConfig);

$('config-students').addEventListener('click', (e) => {
  const rmBtn = e.target.closest('[data-remove-student]');
  if (rmBtn) {
    syncDomToConfigState();
    const idx = Number(rmBtn.dataset.removeStudent);
    state.config.students = state.config.students.filter((_, i) => i !== idx);
    renderConfigTab();
  }
});

$('config-students').addEventListener('change', (e) => {
  const sel = e.target.closest('[data-channel]');
  if (!sel) return;
  syncDomToConfigState();
  renderConfigTab();
});

document.querySelectorAll('[data-log-level]').forEach(chip => {
  chip.addEventListener('click', () => {
    state.logLevel = chip.dataset.logLevel;
    document.querySelectorAll('[data-log-level]').forEach(c => c.classList.toggle('active', c === chip));
    renderLogsTab();
  });
});
$('log-lines').addEventListener('change', () => loadLogs());
$('log-filter').addEventListener('input', renderLogsTab);

$('zoom-close').addEventListener('click', closeZoom);
$('zoom-refresh').addEventListener('click', refreshZoom);
$('zoom-qr').addEventListener('click', (e) => triggerUiAction(state.zoomAccount, 'qr', e.currentTarget));
$('zoom-login').addEventListener('click', (e) => triggerUiAction(state.zoomAccount, 'login', e.currentTarget));
$('zoom').addEventListener('click', (e) => { if (e.target === $('zoom')) closeZoom(); });

$('delete-cancel').addEventListener('click', closeDeleteModal);
$('delete-cancel-x').addEventListener('click', closeDeleteModal);
$('delete-modal').addEventListener('click', (e) => { if (e.target === $('delete-modal')) closeDeleteModal(); });
$('delete-confirm').addEventListener('click', async () => {
  const name = state.deleteTarget;
  if (!name) return;
  const removeVol = $('delete-remove-volume').checked;
  $('delete-confirm').disabled = true;
  try {
    await api(`/api/accounts/${encodeURIComponent(name)}?remove_volume=${removeVol ? '1' : '0'}`, { method: 'DELETE' });
    toast(`已删除账号容器 ${name}${removeVol ? ' 及数据卷' : ''}`);
    closeDeleteModal();
    await loadOverview(false);
    renderConfigTab();
  } catch (err) {
    toast(`删除失败：${err.message}`);
  } finally {
    $('delete-confirm').disabled = false;
  }
});

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') { closeZoom(); closeDeleteModal(); }
});

/* ── Boot & Polling ───────────────────────────────────────────────────────── */

Promise.all([loadOverview(false), loadConfig(false), loadLogs(false)]);

setInterval(() => {
  if (document.hidden) return;
  loadOverview(false);
}, 10000);

setInterval(() => {
  if (document.hidden) return;
  if (state.view === 'logs' || state.view === 'overview') loadLogs(false);
}, 5000);

setInterval(() => {
  if (document.hidden) return;
  if (state.view === 'accounts' || state.zoomAccount) {
    Object.keys(state.imageVersions).forEach(n => { state.imageVersions[n] = Date.now(); });
    if (state.view === 'accounts') renderAccountsTab();
    if (state.zoomAccount) refreshZoom();
  }
}, 15000);

$('ttl-extend').addEventListener('click', async () => {
  try {
    const res = await apiJson('/api/dashboard/extend', 'POST', { minutes: 10 });
    syncAutoShutdown(res.auto_shutdown);
    toast('已重置自动关闭倒计时为 10 分钟');
  } catch (err) {
    toast(`续期失败：${err.message}`);
  }
});
$('ttl-shutdown').addEventListener('click', async () => {
  try {
    await apiJson('/api/dashboard/shutdown', 'POST', {});
    state.ttlEnabled = false;
    $('ttl-countdown').textContent = '已关闭';
    $('global-status').className = 'pill neutral';
    $('global-status').textContent = '看板已关闭';
    toast('看板已关闭，duaa 后台签到服务继续运行');
  } catch (err) {
    toast(`关闭失败：${err.message}`);
  }
});
setInterval(renderTtlCountdown, 1000);

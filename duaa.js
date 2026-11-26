// ==UserScript==
// @name         不智慧教室
// @version      2.8.0
// @description  Bypass CORS to allow local in-campus query/checkin for the frontend
// @author       singledog
// @match        https://duaa.singledog233.top/*
// @grant        GM.setValue
// @grant        GM.getValue
// @grant        GM_xmlhttpRequest
// @grant        unsafeWindow
// @inject-into  content
// @noframes
// @connect      iclass.buaa.edu.cn
// @icon         https://www.google.com/s2/favicons?domain=www.singledog233.top
// @run-at       document-start
// @license MIT
// @namespace https://greasyfork.org/users/1226768
// ==/UserScript==

(function () {
    'use strict'

    if (window.top !== window.self) return

    // ── 端点常量 ──────────────────────────────────────────────────────────────────
    const LOGIN_BASE = 'https://iclass.buaa.edu.cn:8346'
    const BASE = 'https://iclass.buaa.edu.cn:8347'
    const SIGN_BASE = 'http://iclass.buaa.edu.cn:8081'
    const canonicalId = (sid) => String(sid).trim().toLowerCase()

    // ── GM_xmlhttpRequest 的 Promise 封装 ────────────────────────────────────────
    function gmReq(details) {
        return new Promise((resolve, reject) => {
            GM_xmlhttpRequest({
                timeout: 15000,
                ...details,
                onload: (r) => r.status < 400 ? resolve(r) : reject(new Error(`HTTP ${r.status}`)),
                onerror: (e) => reject(new Error((e && e.error) || 'Network error')),
                ontimeout: () => reject(new Error('Request timeout')),
            })
        })
    }

    function ssoRequiredError(message) {
        return {
            response: {
                data: {
                    error: {
                        code: 'login_sso_required',
                        message,
                    },
                },
            },
        }
    }

    // ── 解析 iclass 统一响应格式 ──────────────────────────────────────────────────
    // STATUS="0" 成功；STATUS="2" 无数据（课程为空）；其余视为错误
    function parseIclass(text) {
        const j = JSON.parse(text)
        if (j.STATUS === '2') return null   // 调用侧按需转为 [] 或抛错
        if (j.STATUS !== '0') throw new Error(j.ERRMSG || `iclass STATUS=${j.STATUS}`)
        return j.result
    }

    // ── 时间格式转换 ──────────────────────────────────────────────────────────────
    // "YYYY-MM-DD HH:MM:SS" / "YYYY-MM-DD HH:MM" → "YYYY-MM-DDTHH:MM:SS+08:00"
    function toIso(s) {
        if (!s) return ''
        const base = s.length === 16 ? s + ':00' : s
        return base.replace(' ', 'T') + '+08:00'
    }

    async function getServerTimestamp(studentId) {
        async function request(tk) {
            const res = await gmReq({
                method: 'POST',
                url: `${SIGN_BASE}/app/common/get_timestamp.action?id=${encodeURIComponent(tk.classId)}`,
                headers: { Sessionid: tk.loginName },
            })
            return JSON.parse(res.responseText)
        }
        let j = await request(await ensureToken(studentId))
        if (j.STATUS === '4001' || j.STATUS === '401') {
            await clearToken(studentId)
            j = await request(await login(studentId))
        }
        if (j.STATUS !== '0') throw new Error(j.ERRMSG || `iclass STATUS=${j.STATUS}`)
        const ts = j && j.timestamp
        if (ts === undefined || ts === null) throw new Error('timestamp missing')
        return String(ts)
    }

    function endpointResponds(url) {
        return new Promise((resolve) => GM_xmlhttpRequest({
            method: 'GET', url, timeout: 5000,
            onload: (r) => resolve(r.status > 0),
            onerror: () => resolve(false),
            ontimeout: () => resolve(false),
        }))
    }

    async function probeAvailability() {
        const [query, sign] = await Promise.all([
            endpointResponds(`${BASE}/`), endpointResponds(`${SIGN_BASE}/`),
        ])
        return query && sign
    }

    function matchSchedule(sched, targets) {
        return targets.some((entry) => {
            if (entry === sched.course_id || entry === sched.id || entry === sched.name) return true
            try {
                const target = JSON.parse(entry)
                return target.course_id === sched.course_id || target.name === sched.name
            } catch { return false }
        })
    }

    // ── 令牌缓存（以学号为 key） ──────────────────────────────────────────────────
    async function loadToken(sid) {
        const v = await GM.getValue(`tk:${sid}`, null)
        return v ? JSON.parse(v) : null
    }
    function saveToken(sid, tk) { return GM.setValue(`tk:${sid}`, JSON.stringify(tk)) }
    function clearToken(sid) { return GM.setValue(`tk:${sid}`, null) }
    async function loadLoginName(sid) { return (await GM.getValue(`login_name:${sid}`, '')) || '' }
    async function saveLoginName(sid, loginName) {
        await GM.setValue(`login_name:${sid}`, loginName || '')
        await clearToken(sid)
    }
    function clearLoginName(sid) { return GM.setValue(`login_name:${sid}`, '') }

    async function login(studentId) {
        const savedLoginName = await loadLoginName(studentId)
        const loginName = savedLoginName || studentId
        const qs = new URLSearchParams({
            phone: loginName,
            password: '',
            verificationType: '2',
            verificationUrl: '',
            userLevel: '1',
        })
        let result
        try {
            const res = await gmReq({ method: 'GET', url: `${LOGIN_BASE}/eschool/app/user/login_buaa.do?${qs}` })
            result = parseIclass(res.responseText)
        } catch (e) {
            if (savedLoginName) {
                await clearLoginName(studentId)
                await clearToken(studentId)
                throw ssoRequiredError('保存的 loginName 已失效，请重新完成一次 SSO 跳转')
            }
            const message = e instanceof Error ? e.message : '请先完成一次 SSO 跳转'
            throw ssoRequiredError(message)
        }
        if (!result || !result.id) throw new Error('登录失败：未获取到 class id')
        const tk = { classId: result.id, loginName, realName: result.realName || studentId }
        await saveToken(studentId, tk)
        return tk
    }

    // ── 确保令牌可用 ──────────────────────────────────────────────────────────────
    async function ensureToken(studentId) {
        return (await loadToken(studentId)) || await login(studentId)
    }

    // ── 通用 iclass 请求（带过期自动重登录）──────────────────────────────────────
    // iclass 使用 GET/POST 均可；此处与 Rust 后端保持一致，使用 POST
    async function iclassRequest(studentId, url, params) {
        async function doReq(tk) {
            const qs = new URLSearchParams({ id: tk.classId, ...params })
            return gmReq({
                method: 'POST',
                url: `${url}?${qs}`,
                headers: { Sessionid: tk.loginName },
            })
        }

        let tk = await ensureToken(studentId)
        let res = await doReq(tk)
        const j = JSON.parse(res.responseText)

        // SESSION 过期时重新登录并重试一次
        if (j.STATUS === '4001' || j.STATUS === '401') {
            await clearToken(studentId)
            tk = await login(studentId)
            res = await doReq(tk)
        }

        return parseIclass(res.responseText)
    }

    // ── Bridge：查询今日课程表 ────────────────────────────────────────────────────
    // 返回值与后端 /api/class/schedule 的 DayScheduleResponse 格式一致
    async function querySchedule(studentId, dateStr) {
        // ensureToken 以取得 realName（不依赖 iclassRequest 返回值）
        await ensureToken(studentId)

        const raw = await iclassRequest(
            studentId,
            `${BASE}/app/course/get_stu_course_sched.action`,
            { dateStr },
        )

        const items = Array.isArray(raw) ? raw : (raw === null ? [] : [raw])
        const schedules = items.map((s) => ({
            id: String(s.id),
            course_id: String(s.courseId ?? ''),
            name: s.courseName || '',
            teacher: s.teacherName || '',
            classroom_name: s.classroomName || '',
            time: toIso(s.classBeginTime),
            end_time: toIso(s.classEndTime),
            status: String(s.signStatus) === '1' ? 1 : 0,
        }))

        const cached = await loadToken(studentId)
        return {
            student_name: cached ? cached.realName : studentId,
            schedules,
        }
    }

    // ── Bridge：手动签到 ──────────────────────────────────────────────────────────
    async function checkin(studentId, scheduleId) {
        const ts = await getServerTimestamp(studentId)
        const result = await iclassRequest(
            studentId,
            `${SIGN_BASE}/eschool/app/course/stu_scan_sign.action`,
            { courseSchedId: scheduleId, timestamp: ts },
        )
        if (String(result?.stuSignStatus) !== '1') throw new Error('签到失败：未确认签到状态')
    }

    // Serialize storage and network operations: the website does not await saveLoginName.
    let queue = Promise.resolve()
    function enqueue(method, args) {
        const normalized = method === 'probeAvailability' ? [] : [canonicalId(args[0]), ...args.slice(1)]
        const result = queue.then(() => ({ querySchedule, checkin, saveLoginName, probeAvailability })[method](...normalized))
        queue = result.catch(() => {})
        return result
    }
    const bridge = {
        querySchedule: (...args) => enqueue('querySchedule', args),
        checkin: (...args) => enqueue('checkin', args),
        matchSchedule,
        probeAvailability,
        saveLoginName: (...args) => {
            const result = enqueue('saveLoginName', args)
            result.catch(e => console.error('[不智慧教室] 保存登录信息失败', e))
            return result
        },
    }

    // Tampermonkey: keep the original page bridge. Userscripts has no unsafeWindow.
    if (typeof unsafeWindow !== 'undefined') {
        unsafeWindow.__checkinBridge = bridge
        return
    }

    // Userscripts: GM APIs stay in the content world; inject only a small page proxy.
    const channel = `duaa-bridge-${crypto.randomUUID()}`
    const origin = location.origin
    let connected = false
    window.addEventListener('message', (event) => {
        if (event.source !== window || event.origin !== origin) return
        const m = event.data
        if (!m || m.channel !== channel) return
        if (m.type === 'ready') {
            connected = true
            console.info('[不智慧教室] 单脚本桥接已就绪')
            return
        }
        if (m.type !== 'request' || typeof m.id !== 'string' ||
            !['querySchedule', 'checkin', 'saveLoginName', 'probeAvailability'].includes(m.method) ||
            !Array.isArray(m.args) || m.args.length !== (m.method === 'probeAvailability' ? 0 : 2) ||
            !m.args.every(a => typeof a === 'string' || typeof a === 'number')) return
        enqueue(m.method, m.args).then(
            result => window.postMessage({ channel, type: 'response', id: m.id, result }, origin),
            e => window.postMessage({ channel, type: 'response', id: m.id,
                error: { message: e?.message || '请求失败', response: e?.response } }, origin),
        )
    })

    // This function must be self-contained: it runs in the page, without extension APIs.
    function installPageBridge(channel, origin) {
        const pending = new Map()
        let sequence = 0
        function call(method, args) {
            return new Promise((resolve, reject) => {
                const id = String(++sequence)
                const timer = setTimeout(() => {
                    pending.delete(id)
                    reject(new Error('脚本请求超时，请刷新页面后重试'))
                }, 120000)
                pending.set(id, { resolve, reject, timer })
                window.postMessage({ channel, type: 'request', id, method, args }, origin)
            })
        }
        window.addEventListener('message', (event) => {
            if (event.source !== window || event.origin !== origin) return
            const m = event.data
            if (!m || m.channel !== channel || m.type !== 'response') return
            const p = pending.get(m.id)
            if (!p) return
            clearTimeout(p.timer)
            pending.delete(m.id)
            if (m.error) {
                const error = new Error(m.error.message)
                if (m.error.response) error.response = m.error.response
                p.reject(error)
            } else p.resolve(m.result)
        })
        window.__checkinBridge = {
            querySchedule: (...args) => call('querySchedule', args),
            checkin: (...args) => call('checkin', args),
            matchSchedule: (sched, targets) => targets.some((entry) => {
                if (entry === sched.course_id || entry === sched.id || entry === sched.name) return true
                try {
                    const target = JSON.parse(entry)
                    return target.course_id === sched.course_id || target.name === sched.name
                } catch { return false }
            }),
            probeAvailability: () => call('probeAvailability', []),
            saveLoginName: (sid, name) => {
                const result = call('saveLoginName', [sid, name || ''])
                result.catch(e => console.error('[不智慧教室] 保存登录信息失败', e))
                return result
            },
        }
        window.postMessage({ channel, type: 'ready' }, origin)
    }

    function inject() {
        const script = document.createElement('script')
        script.textContent = `;(${installPageBridge.toString()})(${JSON.stringify(channel)}, ${JSON.stringify(origin)});`
        document.documentElement.appendChild(script)
        script.remove()
        setTimeout(() => {
            if (!connected) console.error('[不智慧教室] 页面桥接未启动，请检查页面 CSP 或脚本权限')
        }, 3000)
    }
    if (document.documentElement) inject()
    else {
        const observer = new MutationObserver(() => {
            if (document.documentElement) { observer.disconnect(); inject() }
        })
        observer.observe(document, { childList: true })
    }
})()

// ==UserScript==
// @name         不智慧教室
// @version      2.6
// @description  Bypass CORS to allow local sign
// @author       singledog
// @match        https://duaa.singledog233.top/*
// @grant        GM_setValue
// @grant        GM_getValue
// @grant        GM_xmlhttpRequest
// @grant        unsafeWindow
// @connect      iclass.buaa.edu.cn
// @icon         https://www.google.com/s2/favicons?domain=www.singledog233.top
// @run-at       document-start
// @license MIT
// @namespace https://greasyfork.org/users/1226768
// ==/UserScript==

(function () {
    'use strict'

    // ── 端点常量 ──────────────────────────────────────────────────────────────────
    const BASE = 'https://iclass.buaa.edu.cn:8347'
    const SIGN_BASE = 'http://iclass.buaa.edu.cn:8081'

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

    function matchSchedule(sched, targets) {
        const idMatch = targets.includes(sched.course_id) || targets.includes(sched.id)
        if (idMatch) return true
        return targets.includes(sched.name)
    }

    // ── 令牌缓存（以学号为 key） ──────────────────────────────────────────────────
    function loadToken(sid) {
        const v = GM_getValue(`tk:${sid}`, null)
        return v ? JSON.parse(v) : null
    }
    function saveToken(sid, tk) { GM_setValue(`tk:${sid}`, JSON.stringify(tk)) }
    function clearToken(sid) { GM_setValue(`tk:${sid}`, null) }

    // ── Passwordless iclass 登录（仅需学号，校内网有效）─────────────────────────
    async function login(studentId) {
        const qs = new URLSearchParams({
            phone: studentId,
            password: '',
            verificationType: '2',
            verificationUrl: '',
            userLevel: '1',
        })
        const res = await gmReq({ method: 'GET', url: `${BASE}/app/user/login.action?${qs}` })
        const result = parseIclass(res.responseText)
        if (!result || !result.id) throw new Error('登录失败：未获取到 token')
        const tk = { userId: result.id, sessionId: result.sessionId, realName: result.realName }
        saveToken(studentId, tk)
        return tk
    }

    // ── 确保令牌可用 ──────────────────────────────────────────────────────────────
    async function ensureToken(studentId) {
        return loadToken(studentId) || await login(studentId)
    }

    // ── 通用 iclass 请求（带过期自动重登录）──────────────────────────────────────
    // iclass 使用 GET/POST 均可；此处与 Rust 后端保持一致，使用 POST
    async function iclassRequest(studentId, url, params) {
        async function doReq(tk) {
            const qs = new URLSearchParams({ id: tk.userId, ...params })
            return gmReq({
                method: 'POST',
                url: `${url}?${qs}`,
                headers: { Sessionid: tk.sessionId },
            })
        }

        let tk = await ensureToken(studentId)
        let res = await doReq(tk)
        const j = JSON.parse(res.responseText)

        // SESSION 过期时重新登录并重试一次
        if (j.STATUS === '4001' || j.STATUS === '401') {
            clearToken(studentId)
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
            id: s.id,
            course_id: s.courseId,
            name: s.courseName,
            teacher: s.teacherName,
            classroom_name: s.classroomName || '',
            time: toIso(s.classBeginTime),
            end_time: toIso(s.classEndTime),
            status: s.signStatus === '1' ? 1 : 0,
        }))

        const cached = loadToken(studentId)
        return {
            student_name: cached ? cached.realName : studentId,
            schedules,
        }
    }

    // ── Bridge：手动签到 ──────────────────────────────────────────────────────────
    async function checkin(studentId, scheduleId) {
        await iclassRequest(
            studentId,
            `${SIGN_BASE}/app/course/stu_scan_sign.action`,
            { courseSchedId: scheduleId, timestamp: String(Date.now()) },
        )
    }

    // ── 暴露桥接对象到页面 window ─────────────────────────────────────────────────
    unsafeWindow.__checkinBridge = { querySchedule, checkin, matchSchedule }
})()

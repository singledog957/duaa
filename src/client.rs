use std::time::{Duration, Instant};

use dashmap::DashMap;
use rand::Rng;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::error::{AppError, AppResult};

// ── iclass response envelope ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct ClassRes<T> {
    #[serde(rename = "STATUS")]
    status: String,
    #[serde(rename = "ERRCODE")]
    errcode: Option<String>,
    #[serde(rename = "ERRMSG")]
    msg: Option<String>,
    result: Option<T>,
}

impl<T: for<'de> Deserialize<'de>> ClassRes<T> {
    fn take(self) -> AppResult<T> {
        if self.status != "0" {
            // ERRCODE 106 = user does not exist
            if self.errcode.as_deref() == Some("106") {
                return Err(AppError::not_found("用户不存在，请检查学号"));
            }
            return Err(AppError::remote(format!(
                "iclass status={} msg={:?}",
                self.status, self.msg
            )));
        }
        self.result
            .ok_or_else(|| AppError::remote("iclass returned no result"))
    }
}

// ── per-student session ───────────────────────────────────────────────────────

#[derive(Clone)]
struct Session {
    user_id: String,
    session_id: String,
    real_name: String,
}

// ── login response ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct LoginResult {
    id: String,
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "realName")]
    pub real_name: String,
}

// ── public data structures ────────────────────────────────────────────────────

/// One entry from the day-schedule endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct Schedule {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "courseId")]
    pub course_id: String,
    #[serde(rename = "courseName")]
    pub name: String,
    #[serde(rename = "teacherName")]
    pub teacher: String,
    /// Raw "YYYY-MM-DD HH:MM[:SS]" as returned by iclass.
    #[serde(rename = "classBeginTime")]
    pub time: String,
    #[serde(rename = "classEndTime")]
    pub end_time: Option<String>,
    /// "0" or "1" – kept as String to be deserialized leniently.
    #[serde(rename = "signStatus", default)]
    pub status_raw: String,
}

impl Schedule {
    pub fn status(&self) -> u8 {
        if self.status_raw == "1" { 1 } else { 0 }
    }
}

/// One entry from the course-schedule (all sessions of a course) endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct CourseSchedule {
    #[serde(rename = "courseSchedId")]
    pub id: String,
    #[serde(rename = "classBeginTime")]
    pub time: String,
    #[serde(rename = "signStatus", default)]
    pub status_raw: String,
}

impl CourseSchedule {
    pub fn status(&self) -> u8 {
        if self.status_raw == "1" { 1 } else { 0 }
    }
}

// ── ClassClient ───────────────────────────────────────────────────────────────

const SCHEDULE_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const CHECKIN_OFFSET_MIN: i64 = -15_000;
const CHECKIN_OFFSET_MAX: i64 = -1_000;
const GLOBAL_CHECKIN_OFFSET_CACHE_KEY: &str = "global";

pub struct ClassClient {
    http: Client,
    sessions: DashMap<String, Session>,
    schedule_cache: DashMap<(String, String), (Instant, Vec<Schedule>)>,
    /// Global cache for the last valid checkin offset.
    /// Shared across all students, stores (offset, last_update_time).
    checkin_offset_cache: DashMap<String, (i64, Instant)>,
}

const FALLBACK_MOBILE_UA: &str = "Mozilla/5.0 (Linux; Android 13; Pixel 7 Build/TQ3A.230901.001; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/116.0.0.0 Mobile Safari/537.36";
const CHECKIN_PY_LIKE_UA: &str = "Mozilla/5.0 (Linux; Android 13; M2012K11AC Build/TKQ1.220829.002; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/116.0.0.0 Mobile Safari/537.36 wxwork/4.1.22 MicroMessenger/7.0.1 NetType/WIFI Language/zh ColorScheme/Light";

const MOBILE_WECHAT_USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Linux; Android 9; COL-AL10 Build/HUAWEICOL-AL10; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/85.0.3527.52 MQQBrowser/6.2 TBS/044607 Mobile Safari/537.36 MMWEBID/7140 MicroMessenger/7.0.4.1420(0x27000437) Process/tools NetType/4G Language/zh_CN",
    "Mozilla/5.0 (Linux; Android 13; V2148A Build/TP1A.220624.014; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/116.0.0.0 Mobile Safari/537.36 XWEB/1160117 MMWEBSDK/20240404 MMWEBID/8833 MicroMessenger/8.0.49.2600(0x28003137) WeChat/arm64 Weixin NetType/WIFI Language/zh_CN ABI/arm64",
    "Mozilla/5.0 (Linux; Android 12; NOH-AL00 Build/HUAWEINOH-AL00; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/116.0.0.0 Mobile Safari/537.36 XWEB/1160117 MMWEBSDK/20240404 MMWEBID/6916 MicroMessenger/8.0.49.2600(0x28003136) WeChat/arm64 Weixin NetType/4G Language/zh_CN ABI/arm64",
    "Mozilla/5.0 (Linux; Android 14; V2307A Build/UP1A.231005.007; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/116.0.0.0 Mobile Safari/537.36 XWEB/1160117 MMWEBSDK/20240301 MMWEBID/4922 MicroMessenger/8.0.48.2580(0x28003052) WeChat/arm64 Weixin NetType/WIFI Language/zh_CN ABI/arm64",
    "Mozilla/5.0 (Linux; Android 13; 23049RAD8C Build/TKQ1.221114.001; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/116.0.0.0 Mobile Safari/537.36 XWEB/1160083 MMWEBSDK/20230303 MMWEBID/4466 MicroMessenger/8.0.34.2340(0x2800225F) WeChat/arm64 Weixin NetType/WIFI Language/zh_CN ABI/arm64",
    "Mozilla/5.0 (Linux; Android 10; PBEM00 Build/QKQ1.190918.001; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/116.0.0.0 Mobile Safari/537.36 XWEB/1160083 MMWEBSDK/20240301 MMWEBID/3124 MicroMessenger/8.0.48.2580(0x2800303F) WeChat/arm64 Weixin NetType/WIFI Language/zh_CN ABI/arm64",
    "Mozilla/5.0 (Linux; Android 13; V2024A Build/TP1A.220624.014; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/116.0.0.0 Mobile Safari/537.36 XWEB/1160117 MMWEBSDK/20240301 MMWEBID/2429 MicroMessenger/8.0.48.2580(0x28003050) WeChat/arm64 Weixin NetType/WIFI Language/zh_CN ABI/arm64",
    "Mozilla/5.0 (Linux; Android 13; V2304A Build/TP1A.220624.014; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/116.0.0.0 Mobile Safari/537.36 XWEB/1160083 MMWEBSDK/20240301 MMWEBID/195 MicroMessenger/8.0.48.2580(0x2800303F) WeChat/arm64 Weixin NetType/5G Language/zh_CN ABI/arm64",
    "Mozilla/5.0 (Linux; Android 9; COL-AL10 Build/HUAWEICOL-AL10; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/58.0.4467.59 MQQBrowser/6.2 TBS/044607 Mobile Safari/537.36 MMWEBID/7140 MicroMessenger/7.0.4.1420(0x27000437) Process/tools NetType/4G Language/zh_CN",
    "Mozilla/5.0 (Linux; Android 9; COL-AL10 Build/HUAWEICOL-AL10; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/56.0.3545.100 MQQBrowser/6.2 TBS/044607 Mobile Safari/537.36 MMWEBID/7140 MicroMessenger/7.0.4.1420(0x27000437) Process/tools NetType/4G Language/zh_CN",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.49(0x18003127) NetType/WIFI Language/zh_CN",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.49(0x18003127) NetType/WIFI Language/zh_CN",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.48(0x18003030) NetType/4G Language/zh_CN",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.42(0x18002a32) NetType/4G Language/zh_CN",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 16_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.48(0x1800302c) NetType/WIFI Language/zh_CN",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 16_6_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.49(0x18003129) NetType/4G Language/zh_HK",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.48(0x18003030) NetType/4G Language/zh_CN",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 14_8_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.48(0x18003030) NetType/WIFI Language/zh_CN",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 15_3_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.48(0x18003030) NetType/WIFI Language/zh_CN",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.49(0x1800312a) NetType/WIFI Language/zh_CN",
];

fn select_mobile_ua_from_pool<'a>(pool: &'a [&'a str]) -> Option<&'a str> {
    if pool.is_empty() {
        return None;
    }
    let idx = rand::rng().random_range(0..pool.len());
    pool.get(idx).copied()
}

fn mobile_user_agent() -> &'static str {
    select_mobile_ua_from_pool(MOBILE_WECHAT_USER_AGENTS).unwrap_or(FALLBACK_MOBILE_UA)
}

fn ua_kind(ua: &str) -> &'static str {
    if ua.contains("iPhone") {
        "iphone"
    } else {
        "android"
    }
}

impl ClassClient {
    pub fn new() -> Self {
        let http = Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(15))
            .build()
            .expect("build reqwest client");
        Self {
            http,
            sessions: DashMap::new(),
            schedule_cache: DashMap::new(),
            checkin_offset_cache: DashMap::new(),
        }
    }

    // ── auth ──────────────────────────────────────────────────────────────────

    /// Passwordless iclass login; returns the student's real name.
    pub async fn login(&self, student_id: &str) -> AppResult<String> {
        let url = "https://iclass.buaa.edu.cn:8347/app/user/login.action";
        let ua = mobile_user_agent();
        debug!(student_id, ua_kind = ua_kind(ua), "selected mobile UA for login request");
        let params = [
            ("phone", student_id),
            ("password", ""),
            ("verificationType", "2"),
            ("verificationUrl", ""),
            ("userLevel", "1"),
        ];
        let res = self
            .http
            .get(url)
            .header(reqwest::header::USER_AGENT, ua)
            .query(&params)
            .send()
            .await?
            .json::<ClassRes<LoginResult>>()
            .await?;
        let lr = res.take()?;
        let name = lr.real_name.clone();
        self.sessions.insert(
            student_id.to_owned(),
            Session {
                user_id: lr.id,
                session_id: lr.session_id,
                real_name: lr.real_name.clone(),
            },
        );
        info!(student_id, "iclass login ok");
        Ok(name)
    }

    /// Return the student's real name (logs in if session not cached).
    pub async fn student_name(&self, student_id: &str) -> AppResult<String> {
        Ok(self.ensure_session(student_id).await?.real_name)
    }

    /// Ensure session exists, re-logging in if necessary.
    async fn ensure_session(&self, student_id: &str) -> AppResult<Session> {
        if let Some(s) = self.sessions.get(student_id) {
            return Ok(s.clone());
        }
        self.login(student_id).await?;
        self.sessions
            .get(student_id)
            .map(|s| s.clone())
            .ok_or_else(|| AppError::internal("session missing after login"))
    }

    /// POST to iclass with automatic session re-login on auth failure.
    async fn iclass_post<T: for<'de> Deserialize<'de>>(
        &self,
        student_id: &str,
        url: &str,
        params: &[(&str, &str)],
    ) -> AppResult<T> {
        let sess = self.ensure_session(student_id).await?;
        let ua = mobile_user_agent();
        debug!(student_id, ua_kind = ua_kind(ua), "selected mobile UA for upstream request");
        let res = self
            .http
            .post(format!("{url}?id={}", sess.user_id))
            .header("Sessionid", &sess.session_id)
            .header(reqwest::header::USER_AGENT, ua)
            .query(params)
            .send()
            .await?
            .json::<ClassRes<T>>()
            .await;

        match res {
            Ok(r) => {
                if r.status == "4001" || r.status == "401" {
                    // Session expired – evict and retry once.
                    warn!(student_id, "session expired, re-logging in");
                    self.sessions.remove(student_id);
                    let sess2 = self.ensure_session(student_id).await?;
                    let ua_retry = mobile_user_agent();
                    debug!(student_id, ua_kind = ua_kind(ua_retry), "selected mobile UA for retry request");
                    return self
                        .http
                        .post(format!("{url}?id={}", sess2.user_id))
                        .header("Sessionid", &sess2.session_id)
                        .header(reqwest::header::USER_AGENT, ua_retry)
                        .query(params)
                        .send()
                        .await?
                        .json::<ClassRes<T>>()
                        .await?
                        .take();
                }
                r.take()
            }
            Err(e) => Err(e.into()),
        }
    }

    // ── public API methods ────────────────────────────────────────────────────

    /// Query all of a student's schedules for a given date (YYYYMMDD).
    /// iclass status=2 means no data for that day; treated as an empty list.
    /// Results are cached in memory for 24 hours per (student_id, date) pair.
    pub async fn query_schedule(
        &self,
        student_id: &str,
        date: &str,
    ) -> AppResult<Vec<Schedule>> {
        let key = (student_id.to_owned(), date.to_owned());
        if let Some(entry) = self.schedule_cache.get(&key) {
            let (stored_at, ref schedules) = *entry;
            if stored_at.elapsed() < SCHEDULE_CACHE_TTL {
                debug!(student = student_id, date, "schedule cache hit");
                return Ok(schedules.clone());
            }
        }
        let url = "https://iclass.buaa.edu.cn:8347/app/course/get_stu_course_sched.action";
        let result = match self.iclass_post(student_id, url, &[("dateStr", date)]).await {
            Ok(v) => v,
            Err(e) if e.code == "remote_error" && e.message.starts_with("iclass status=2") => {
                vec![]
            }
            Err(e) => return Err(e),
        };
        self.schedule_cache.insert(key, (Instant::now(), result.clone()));
        Ok(result)
    }

    /// Query all course-level schedules (all sessions) for a course ID.
    pub async fn query_course_schedule(
        &self,
        student_id: &str,
        course_id: &str,
    ) -> AppResult<Vec<CourseSchedule>> {
        let url =
            "https://iclass.buaa.edu.cn:8347/app/my/get_my_course_sign_detail.action";
        self.iclass_post(student_id, url, &[("courseId", course_id)])
            .await
    }

    /// Sign-in for `schedule_id` on behalf of `student_id`.
    pub async fn checkin(&self, student_id: &str, schedule_id: &str) -> AppResult<Value> {
        // Use a fixed base timestamp for this whole checkin flow.
        let base_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Try the global cached offset first (if available).
        if let Some(entry) = self
            .checkin_offset_cache
            .get(GLOBAL_CHECKIN_OFFSET_CACHE_KEY)
        {
            let (offset, _) = *entry;
            match self.do_checkin(student_id, schedule_id, offset, base_ts).await {
                Ok(result) => return Ok(result),
                Err(err) => {
                    // Check if this is an offset-related error
                    if self.is_offset_error(&err) {
                        debug!(student = student_id, "cached offset invalid, starting binary search");
                        // Fall through to binary search
                    } else {
                        // Other errors should not trigger offset search
                        return Err(err);
                    }
                }
            }
        }

        // Binary search for a valid offset
        info!(student = student_id, "binary searching for valid checkin offset");
        let (new_offset, result) = self
            .binary_search_offset(student_id, schedule_id, base_ts)
            .await?;
        info!(student = student_id, offset = new_offset, "found valid offset");

        // Cache the found offset globally.
        self.checkin_offset_cache.insert(
            GLOBAL_CHECKIN_OFFSET_CACHE_KEY.to_owned(),
            (new_offset, Instant::now()),
        );

        Ok(result)
    }

    /// Check if an error message suggests offset-related issues.
    fn is_offset_error(&self, err: &AppError) -> bool {
        let msg = &err.message;
        msg.contains("参数错误") || msg.contains("二维码已失效") || msg.contains("已失效")
    }

    /// Binary search for a valid offset in the range [CHECKIN_OFFSET_MIN, CHECKIN_OFFSET_MAX].
    async fn binary_search_offset(
        &self,
        student_id: &str,
        schedule_id: &str,
        base_ts: i64,
    ) -> AppResult<(i64, Value)> {
        let mut lo = CHECKIN_OFFSET_MIN;
        let mut lo_err = "二维码已失效".to_string(); // Assume lower bound needs larger offset
        let mut hi = CHECKIN_OFFSET_MAX;
        let mut hi_err = "参数错误".to_string(); // Assume upper bound needs smaller offset

        while lo < hi - 1 {
            let mid = (lo + hi) / 2;
            match self.do_checkin(student_id, schedule_id, mid, base_ts).await {
                Ok(result) => {
                    return Ok((mid, result));
                }
                Err(err) => {
                    let msg = err.message.as_str();
                    if msg.contains("参数错误") {
                        // Offset should be smaller (more negative), search lower half.
                        hi = mid;
                        hi_err = msg.to_string();
                    } else if msg.contains("二维码已失效") || msg.contains("已失效") {
                        // Offset should be larger (closer to zero), search upper half.
                        lo = mid;
                        lo_err = msg.to_string();
                    } else {
                        // Some other error, not offset-related
                        return Err(err);
                    }
                }
            }
        }

        // If we've exhausted the search, try the remaining candidates
        for offset in [lo, lo + 1, hi - 1, hi].iter().copied() {
            if offset < CHECKIN_OFFSET_MIN || offset > CHECKIN_OFFSET_MAX {
                continue;
            }
            if let Ok(result) = self
                .do_checkin(student_id, schedule_id, offset, base_ts)
                .await
            {
                return Ok((offset, result));
            }
        }

        Err(AppError::remote(format!(
            "binary search failed: lo={} ({}), hi={} ({})",
            lo, lo_err, hi, hi_err
        )))
    }

    /// Perform a single checkin request with the given offset.
    async fn do_checkin(
        &self,
        student_id: &str,
        schedule_id: &str,
        offset: i64,
        base_ts: i64,
    ) -> AppResult<Value> {
        let url = "http://iclass.buaa.edu.cn:8081/app/course/stu_scan_sign.action";
        let ts = (base_ts + offset).to_string();
        let sess = self.ensure_session(student_id).await?;
        let params = [
            ("id", sess.user_id.as_str()),
            ("courseSchedId", schedule_id),
            ("timestamp", ts.as_str()),
        ];
        let res = self
            .http
            .post(url)
            .header("Sessionid", &sess.session_id)
            .header(reqwest::header::USER_AGENT, CHECKIN_PY_LIKE_UA)
            .query(&params)
            .send()
            .await?
            .json::<ClassRes<Value>>()
            .await;

        let result = match res {
            Ok(r) => {
                if r.status == "4001" || r.status == "401" {
                    warn!(student_id, "session expired, re-logging in");
                    self.sessions.remove(student_id);
                    let sess2 = self.ensure_session(student_id).await?;
                    let retry_params = [
                        ("id", sess2.user_id.as_str()),
                        ("courseSchedId", schedule_id),
                        ("timestamp", ts.as_str()),
                    ];
                    self.http
                        .post(url)
                        .header("Sessionid", &sess2.session_id)
                        .header(reqwest::header::USER_AGENT, CHECKIN_PY_LIKE_UA)
                        .query(&retry_params)
                        .send()
                        .await?
                        .json::<ClassRes<Value>>()
                        .await?
                        .take()?
                } else {
                    r.take()?
                }
            }
            Err(e) => return Err(e.into()),
        };

        Ok(result)
    }
}

#[cfg(test)]
mod ua_tests {
    use super::{mobile_user_agent, select_mobile_ua_from_pool, MOBILE_WECHAT_USER_AGENTS};

    #[test]
    fn ua_pool_is_not_empty() {
        assert!(!MOBILE_WECHAT_USER_AGENTS.is_empty());
    }

    #[test]
    fn ua_pool_covers_android_and_iphone() {
        let has_android = MOBILE_WECHAT_USER_AGENTS.iter().any(|ua| ua.contains("Android"));
        let has_iphone = MOBILE_WECHAT_USER_AGENTS.iter().any(|ua| ua.contains("iPhone"));
        assert!(has_android, "UA pool should contain Android WeChat UA");
        assert!(has_iphone, "UA pool should contain iPhone WeChat UA");
    }

    #[test]
    fn empty_pool_returns_none_and_runtime_has_fallback() {
        assert_eq!(select_mobile_ua_from_pool(&[]), None);
        let ua = mobile_user_agent();
        assert!(!ua.is_empty());
        assert!(ua.contains("Mobile") || ua.contains("MicroMessenger"));
    }
}

use std::time::{Duration, Instant};

use dashmap::DashMap;
use hmac::{Hmac, Mac};
use rand::distr::{Alphanumeric, SampleString};
use reqwest::{redirect::Policy, Client};
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;
use tracing::{debug, info, warn};

use crate::{
    config::StudentEntry,
    error::{AppError, AppResult},
};

type HmacSha256 = Hmac<Sha256>;

const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ",
    "AppleWebKit/537.36 (KHTML, like Gecko) ",
    "Chrome/130.0.0.0 Safari/537.36"
);

const SCHEDULE_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// iclass reports "no data" as STATUS=2, which is indistinguishable from a
/// stale/expired session being silently rejected. On an empty answer we force
/// a re-login and retry once — but never more often than this, so genuinely
/// class-free days don't hammer SSO.
const EMPTY_SCHED_RELOGIN_MIN_INTERVAL: Duration = Duration::from_secs(30 * 60);
const SSO_LOGIN_URL: &str = "https://sso.buaa.edu.cn/login";
const JUMP_URL: &str = "https://iclass.buaa.edu.cn:8346/?type=jumpMyCenter";
const LOGIN_BUAA_URL: &str = "https://iclass.buaa.edu.cn:8346/eschool/app/user/login_buaa.do";
const SCHEDULE_URL: &str = "https://iclass.buaa.edu.cn:8347/app/course/get_stu_course_sched.action";
const COURSE_DETAIL_URL: &str =
    "https://iclass.buaa.edu.cn:8347/app/my/get_my_course_sign_detail.action";
const TIMESTAMP_URL_8081: &str = "http://iclass.buaa.edu.cn:8081/app/common/get_timestamp.action";
const TIMESTAMP_URL_8082: &str = "http://iclass.buaa.edu.cn:8082/app/common/get_timestamp.action";
const CHECKIN_URL: &str = "http://iclass.buaa.edu.cn:8082/eschool/app/course/stu_scan_sign.action";
const CHECKIN_URL_8081: &str =
    "http://iclass.buaa.edu.cn:8081/eschool/app/course/stu_scan_sign.action";
const SIGN_PAYLOAD_SUFFIX: &str = "stu_scan_sign";

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

impl<T> ClassRes<T> {
    fn check(self) -> AppResult<Self> {
        match self.status.as_str() {
            "0" => Ok(self),
            "2" => Err(AppError::remote(
                "iclass status=2 msg=Some(\"Empty data list\")",
            )),
            _ => {
                if self.errcode.as_deref() == Some("106") {
                    return Err(AppError::not_found("用户不存在，请检查学号"));
                }
                Err(AppError::remote(format!(
                    "iclass status={} msg={:?}",
                    self.status, self.msg
                )))
            }
        }
    }

    fn take(self) -> AppResult<T> {
        self.check()?
            .result
            .ok_or_else(|| AppError::remote("iclass returned no result"))
    }
}

#[cfg(test)]
mod tests {
    use super::sign_checkin;

    #[test]
    fn sign_checkin_matches_expected_hmac() {
        let sig = sign_checkin(
            "0123456789abcdef0123456789abcdef",
            "fedcba9876543210fedcba9876543210",
            "Ab12Cd34",
        );
        assert_eq!(
            sig,
            "5d4e39a5b17bdd59b3a880eb172f1410af77ca71f60a30df58f1922c7c55fbdb"
        );
    }
}

#[derive(Clone)]
struct Session {
    class_id: String,
    login_name: String,
    real_name: String,
}

#[derive(Clone)]
struct Account {
    student_id: String,
    sso_password: String,
    secret_url: Option<String>,
}

#[derive(Deserialize)]
struct SecretResponse {
    taskid: String,
    #[serde(rename = "signSecret")]
    sign_secret: String,
}

#[derive(Deserialize)]
struct LoginResult {
    id: String,
    #[serde(rename = "realName", default)]
    real_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Schedule {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "courseId")]
    pub course_id: String,
    #[serde(rename = "courseName")]
    pub name: String,
    /// Academic-affairs course code (e.g. D391041007); may lag behind GSMIS
    /// after course merges.
    #[serde(rename = "courseNum", default)]
    pub course_num: String,
    #[serde(rename = "teacherName")]
    pub teacher: String,
    #[serde(rename = "classBeginTime")]
    pub time: String,
    #[serde(rename = "classEndTime")]
    pub end_time: Option<String>,
    #[serde(rename = "signStatus", default)]
    pub status_raw: String,
}

impl Schedule {
    pub fn status(&self) -> u8 {
        if self.status_raw == "1" {
            1
        } else {
            0
        }
    }
}

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
        if self.status_raw == "1" {
            1
        } else {
            0
        }
    }
}

/// Per-student SSO state: one shared cookie jar with two clients on top —
/// a no-redirect one for login POST result checks (CAS signals success with a
/// 302), and a redirect-following one for the iclass jump chain that ends at
/// a URL carrying `loginName=...`.
#[derive(Clone)]
struct SsoSession {
    direct: Client,
    follow: Client,
}

impl SsoSession {
    fn new() -> Self {
        let jar = std::sync::Arc::new(reqwest::cookie::Jar::default());
        let base = || {
            Client::builder()
                .danger_accept_invalid_certs(true)
                .timeout(Duration::from_secs(20))
                .user_agent(USER_AGENT)
                .cookie_provider(jar.clone())
        };
        let direct = base()
            .redirect(Policy::none())
            .build()
            .expect("build sso client");
        let follow = base().build().expect("build sso follow client");
        Self { direct, follow }
    }
}

pub struct ClassClient {
    http: Client,
    /// Secret-provider calls may take ~20s (UI click + OAuth round-trip),
    /// so they get their own longer-timeout client.
    provider_http: Client,
    sso_sessions: DashMap<String, SsoSession>,
    sessions: DashMap<String, Session>,
    schedule_cache: DashMap<(String, String), (Instant, Vec<Schedule>)>,
    accounts: DashMap<String, Account>,
    /// Last time we force-relogged-in a student to double-check an empty
    /// (STATUS=2) schedule answer; used to rate-limit the retry.
    last_empty_relogin: DashMap<String, Instant>,
}

impl ClassClient {
    pub fn new(students: &[StudentEntry]) -> Self {
        let http = Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(15))
            .user_agent(USER_AGENT)
            .build()
            .expect("build reqwest client");

        let client = Self {
            http,
            provider_http: Client::builder()
                .danger_accept_invalid_certs(true)
                .timeout(Duration::from_secs(45))
                .build()
                .expect("build provider client"),
            sso_sessions: DashMap::new(),
            sessions: DashMap::new(),
            schedule_cache: DashMap::new(),
            accounts: DashMap::new(),
            last_empty_relogin: DashMap::new(),
        };
        for student in students {
            client.accounts.insert(
                student.student_id.clone(),
                Account {
                    student_id: student.student_id.clone(),
                    sso_password: student.sso_password.clone(),
                    secret_url: student.secret_url.clone(),
                },
            );
        }
        client
    }

    fn account(&self, student_id: &str) -> AppResult<Account> {
        self.accounts
            .get(student_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| AppError::not_found("配置中不存在该学号"))
    }

    /// Per-student SSO session: every student gets an isolated cookie jar so one
    /// student's SSO session (TGT) never leaks into another student's login flow.
    fn sso_session_for(&self, student_id: &str) -> SsoSession {
        if let Some(entry) = self.sso_sessions.get(student_id) {
            return entry.value().clone();
        }
        let session = SsoSession::new();
        self.sso_sessions
            .insert(student_id.to_owned(), session.clone());
        session
    }

    /// Hot-reload support: insert or update accounts from a freshly loaded config.
    pub fn sync_accounts(&self, students: &[StudentEntry]) {
        for student in students {
            self.accounts.insert(
                student.student_id.clone(),
                Account {
                    student_id: student.student_id.clone(),
                    sso_password: student.sso_password.clone(),
                    secret_url: student.secret_url.clone(),
                },
            );
        }
    }

    /// Hot-reload support: drop a student's account together with cached session state.
    pub fn remove_student(&self, student_id: &str) {
        self.clear_student_state(student_id);
        self.last_empty_relogin.remove(student_id);
        self.accounts.remove(student_id);
    }

    async fn sso_login(&self, student_id: &str, account: &Account) -> AppResult<()> {
        let sso = self.sso_session_for(student_id).direct;

        let res = sso.get(SSO_LOGIN_URL).send().await?;
        // A redirect here means this cookie jar already holds a valid SSO session.
        if res.status().is_redirection() {
            return Ok(());
        }

        let bytes = res.bytes().await?;
        let html = String::from_utf8_lossy(&bytes);
        let execution = extract_input_value(&html, "execution")
            .ok_or_else(|| AppError::remote("SSO 登录页缺少 execution"))?;

        let form = [
            ("username", account.student_id.as_str()),
            ("password", account.sso_password.as_str()),
            ("submit", "登录"),
            ("type", "username_password"),
            ("execution", execution.as_str()),
            ("_eventId", "submit"),
        ];
        let form_body = serde_urlencoded::to_string(form)
            .map_err(|e| AppError::internal(format!("encode sso form failed: {e}")))?;

        let res = sso
            .post(SSO_LOGIN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_body)
            .send()
            .await?;
        // BUAA SSO signals a successful login with a 302 redirect (ticket flow);
        // wrong credentials come back as 200 with the login page rendered again.
        if res.status().is_redirection() {
            return Ok(());
        }

        let bytes = res.bytes().await?;
        let html = String::from_utf8_lossy(&bytes);
        if html.contains("continueForm") {
            let execution = extract_input_value(&html, "execution")
                .ok_or_else(|| AppError::remote("SSO 风险继续页缺少 execution"))?;
            let form = [
                ("execution", execution.as_str()),
                ("_eventId", "ignoreAndContinue"),
            ];
            let form_body = serde_urlencoded::to_string(form)
                .map_err(|e| AppError::internal(format!("encode sso continue form failed: {e}")))?;
            let retry = sso
                .post(SSO_LOGIN_URL)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(form_body)
                .send()
                .await?;
            // The risk-confirm page only appears after credentials were accepted,
            // so both 2xx and 302 count as success here.
            if retry.status().is_success() || retry.status().is_redirection() {
                return Ok(());
            }
        }

        Err(AppError::remote("SSO 登录失败，请检查学号或密码"))
    }

    async fn fetch_login_name(&self, student_id: &str) -> AppResult<String> {
        // The jump chain (iclass → sso → ticket → iclass) must be followed to
        // the end: the final URL carries loginName=... after ticket validation.
        let res = self
            .sso_session_for(student_id)
            .follow
            .get(JUMP_URL)
            .send()
            .await?;
        let final_url = res.url().as_str().to_string();

        extract_login_name_from_url(&final_url).ok_or_else(|| {
            AppError::remote(format!(
                "未能为 {student_id} 解析 login_name（最终 URL: {final_url}）"
            ))
        })
    }

    async fn login_with_login_name(&self, login_name: &str) -> AppResult<LoginResult> {
        let params = [
            ("phone", login_name),
            ("password", ""),
            ("verificationType", "2"),
            ("verificationUrl", ""),
            ("userLevel", "1"),
        ];

        self.http
            .get(LOGIN_BUAA_URL)
            .query(&params)
            .send()
            .await?
            .json::<ClassRes<LoginResult>>()
            .await?
            .take()
    }

    pub async fn login(&self, student_id: &str) -> AppResult<String> {
        let account = self.account(student_id)?;
        if let Err(e) = self.sso_login(student_id, &account).await {
            // A stale SSO session in this jar can hide the login form (no
            // execution field); retry once with a freshly created jar.
            if e.message.contains("缺少 execution") {
                self.sso_sessions.remove(student_id);
                self.sso_login(student_id, &account).await?;
            } else {
                return Err(e);
            }
        }
        let login_name = self.fetch_login_name(student_id).await?;
        let login = self.login_with_login_name(&login_name).await?;
        let real_name = if login.real_name.trim().is_empty() {
            student_id.to_owned()
        } else {
            login.real_name.clone()
        };

        self.sessions.insert(
            student_id.to_owned(),
            Session {
                class_id: login.id,
                login_name,
                real_name: real_name.clone(),
            },
        );
        info!(student_id = %student_id, "iclass login ok via auto sso");
        Ok(real_name)
    }

    pub async fn student_name(&self, student_id: &str) -> AppResult<String> {
        Ok(self.ensure_session(student_id).await?.real_name)
    }

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

    fn clear_student_state(&self, student_id: &str) {
        self.sessions.remove(student_id);
        self.schedule_cache.retain(|(sid, _), _| sid != student_id);
        // Keep `last_empty_relogin`: it rate-limits the status=2 re-login
        // retry and must survive a forced session refresh.
    }

    async fn iclass_post<T: for<'de> Deserialize<'de>>(
        &self,
        student_id: &str,
        url: &str,
        params: &[(&str, &str)],
    ) -> AppResult<T> {
        let sess = self.ensure_session(student_id).await?;
        let res = self
            .http
            .post(format!("{url}?id={}", sess.class_id))
            .header("Sessionid", &sess.login_name)
            .query(params)
            .send()
            .await?
            .json::<ClassRes<T>>()
            .await;

        match res {
            Ok(r) => {
                if r.status == "4001" || r.status == "401" {
                    warn!(student_id = %student_id, "session expired, re-logging in");
                    self.clear_student_state(student_id);
                    let sess2 = self.ensure_session(student_id).await?;
                    return self
                        .http
                        .post(format!("{url}?id={}", sess2.class_id))
                        .header("Sessionid", &sess2.login_name)
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

    pub async fn query_schedule(&self, student_id: &str, date: &str) -> AppResult<Vec<Schedule>> {
        let key = (student_id.to_owned(), date.to_owned());
        if let Some(entry) = self.schedule_cache.get(&key) {
            let (stored_at, ref schedules) = *entry;
            if stored_at.elapsed() < SCHEDULE_CACHE_TTL {
                debug!(student = %student_id, date, "schedule cache hit");
                return Ok(schedules.clone());
            }
        }

        let result = self.fetch_schedule_tolerant(student_id, date).await?;
        // Never cache an empty answer: STATUS=2 may mean a stale session was
        // silently rejected, and a cached empty list would hide every class
        // of the day. An extra request per poll on class-free days is cheap.
        if !result.is_empty() {
            self.schedule_cache
                .insert(key, (Instant::now(), result.clone()));
        }
        Ok(result)
    }

    /// Cache-bypassing variant used by the `--status` one-shot mode: always
    /// hits iclass and refreshes the cached snapshot with the fresh answer.
    pub async fn query_schedule_force(
        &self,
        student_id: &str,
        date: &str,
    ) -> AppResult<Vec<Schedule>> {
        let result = self.fetch_schedule_tolerant(student_id, date).await?;
        if !result.is_empty() {
            self.schedule_cache.insert(
                (student_id.to_owned(), date.to_owned()),
                (Instant::now(), result.clone()),
            );
        }
        Ok(result)
    }

    /// Fetch one day's schedule, tolerating the ambiguous STATUS=2 ("empty
    /// data list") answer: iclass returns it both for genuinely class-free
    /// days and — as observed since 2026-11-18 — for requests made with a
    /// stale cached session, without the usual 4001/401 expiry signal.
    ///
    /// On STATUS=2 we force a re-login and retry once (rate-limited per
    /// student); if the fresh session still yields STATUS=2 we accept it as
    /// "no classes today".
    async fn fetch_schedule_tolerant(
        &self,
        student_id: &str,
        date: &str,
    ) -> AppResult<Vec<Schedule>> {
        let is_empty_err = |e: &crate::error::AppError| {
            e.code == "remote_error" && e.message.starts_with("iclass status=2")
        };

        match self
            .iclass_post(student_id, SCHEDULE_URL, &[("dateStr", date)])
            .await
        {
            Ok(v) => Ok(v),
            Err(e) if is_empty_err(&e) => {
                let relogin_ok = match self.last_empty_relogin.get(student_id) {
                    Some(at) => at.elapsed() >= EMPTY_SCHED_RELOGIN_MIN_INTERVAL,
                    None => true,
                };
                if !relogin_ok {
                    debug!(student = %student_id, date, "schedule empty (status=2), re-login recently tried");
                    return Ok(vec![]);
                }
                self.last_empty_relogin
                    .insert(student_id.to_owned(), Instant::now());
                warn!(
                    student = %student_id,
                    date,
                    "schedule empty (status=2); forcing re-login to rule out stale session"
                );
                self.clear_student_state(student_id);
                match self
                    .iclass_post(student_id, SCHEDULE_URL, &[("dateStr", date)])
                    .await
                {
                    Ok(v) => Ok(v),
                    Err(e2) if is_empty_err(&e2) => {
                        debug!(student = %student_id, date, "schedule still empty after re-login; assuming no classes today");
                        Ok(vec![])
                    }
                    Err(e2) => Err(e2),
                }
            }
            Err(e) => Err(e),
        }
    }

    pub async fn query_course_schedule(
        &self,
        student_id: &str,
        course_id: &str,
    ) -> AppResult<Vec<CourseSchedule>> {
        self.iclass_post(student_id, COURSE_DETAIL_URL, &[("courseId", course_id)])
            .await
    }

    pub async fn checkin(&self, student_id: &str, schedule_id: &str) -> AppResult<Value> {
        self.checkin_signed(student_id, &[schedule_id.to_owned()])
            .await
    }

    /// Try every member of a parallel-section group via the official 8081
    /// path (only the teacher's section has an open QR session). This never
    /// falls back to 8082; the worker owns that decision.
    pub async fn checkin_signed(
        &self,
        student_id: &str,
        schedule_ids: &[String],
    ) -> AppResult<Value> {
        if schedule_ids.is_empty() {
            return Err(AppError::not_found("empty checkin group"));
        };
        let Some(secret_url) = self.account(student_id).ok().and_then(|a| a.secret_url) else {
            return Err(AppError::remote("no 8081 secret provider configured"));
        };
        let mut last_err: Option<AppError> = None;
        for sid in schedule_ids {
            match self.do_signed_checkin(student_id, sid, &secret_url).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    warn!(
                        student = student_id,
                        sched = %sid,
                        group_len = schedule_ids.len(),
                        err = %e,
                        "8081 signed checkin failed for group member"
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| AppError::remote("8081 checkin failed")))
    }

    /// One unsigned 8082 attempt on the representative schedule id.
    pub async fn checkin_fallback(
        &self,
        student_id: &str,
        schedule_ids: &[String],
    ) -> AppResult<Value> {
        let Some(first) = schedule_ids.first() else {
            return Err(AppError::not_found("empty checkin group"));
        };
        let ts = self.get_server_timestamp(student_id).await?;
        self.do_checkin_with_ts(student_id, first, &ts).await
    }

    pub fn has_secret_provider(&self, student_id: &str) -> bool {
        self.account(student_id)
            .ok()
            .and_then(|a| a.secret_url)
            .is_some()
    }

    /// Official 8081 entry: ask the account's wwlocal container for a fresh
    /// (taskid, signSecret) pair — it clicks 扫一扫 and captures the
    /// get_sign_secret response. Fetch the timestamp only after that slow
    /// provider call: 8081 rejects timestamps that are older by only seconds.
    async fn do_signed_checkin(
        &self,
        student_id: &str,
        schedule_id: &str,
        secret_url: &str,
    ) -> AppResult<Value> {
        let secret: SecretResponse = self
            .provider_http
            .get(secret_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if secret.taskid.is_empty() || secret.sign_secret.is_empty() {
            return Err(AppError::remote(
                "secret provider returned empty credentials",
            ));
        }

        let ts = self.get_8081_timestamp().await?;
        let nonce = Alphanumeric.sample_string(&mut rand::rng(), 8);
        let signature = sign_checkin(&secret.taskid, &secret.sign_secret, &nonce);
        let sess = self.ensure_session(student_id).await?;
        let params = [
            ("courseSchedId", schedule_id),
            ("timestamp", ts.as_str()),
            ("taskid", secret.taskid.as_str()),
            ("nonce", nonce.as_str()),
            ("signature", signature.as_str()),
        ];
        let res = self
            .http
            .post(format!("{CHECKIN_URL_8081}?id={}", sess.class_id))
            .header("Sessionid", &sess.login_name)
            .form(&params)
            .send()
            .await?
            .json::<ClassRes<Value>>()
            .await?;
        let result = res.take()?;
        if result
            .get("stuSignStatus")
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            != "1"
        {
            return Err(AppError::remote("8081 checkin failed"));
        }
        info!(
            student = student_id,
            sched = schedule_id,
            stu_sign_id = %result.get("stuSignId").and_then(|v| v.as_str()).unwrap_or("?"),
            "checkin ok via 8081 (signed)"
        );
        Ok(result)
    }

    /// 8081's own timestamp endpoint, used only for the signed path. It does
    /// not need the business `id`/`Sessionid` headers.
    async fn get_8081_timestamp(&self) -> AppResult<String> {
        let res = self
            .http
            .get(TIMESTAMP_URL_8081)
            .send()
            .await?
            .json::<Value>()
            .await?;
        Self::parse_timestamp_response(&res)
    }

    async fn get_server_timestamp(&self, student_id: &str) -> AppResult<String> {
        let sess = self.ensure_session(student_id).await?;
        let res = self
            .http
            .post(format!("{TIMESTAMP_URL_8082}?id={}", sess.class_id))
            .header("Sessionid", &sess.login_name)
            .send()
            .await?
            .json::<Value>()
            .await?;

        match res.get("STATUS").and_then(|v| v.as_str()) {
            Some("0") => {}
            Some("4001" | "401") => {
                warn!(student_id = %student_id, "timestamp session expired, re-logging in");
                self.clear_student_state(student_id);
                let sess2 = self.ensure_session(student_id).await?;
                let retry = self
                    .http
                    .post(format!("{TIMESTAMP_URL_8082}?id={}", sess2.class_id))
                    .header("Sessionid", &sess2.login_name)
                    .send()
                    .await?
                    .json::<Value>()
                    .await?;
                return retry
                    .get("timestamp")
                    .map(timestamp_to_string)
                    .transpose()?
                    .ok_or_else(|| {
                        AppError::remote("Failed to parse timestamp from server response")
                    });
            }
            Some(status) => {
                return Err(AppError::remote(format!(
                    "iclass status={} msg={:?}",
                    status,
                    res.get("ERRMSG").and_then(|v| v.as_str())
                )));
            }
            None => {
                return Err(AppError::remote(
                    "Failed to parse timestamp from server response",
                ))
            }
        }

        res.get("timestamp")
            .map(timestamp_to_string)
            .transpose()?
            .ok_or_else(|| AppError::remote("Failed to parse timestamp from server response"))
    }

    fn parse_timestamp_response(res: &Value) -> AppResult<String> {
        match res.get("STATUS").and_then(|v| v.as_str()) {
            Some("0") => {}
            Some(status) => {
                return Err(AppError::remote(format!(
                    "iclass status={} msg={:?}",
                    status,
                    res.get("ERRMSG").and_then(|v| v.as_str())
                )));
            }
            None => {
                return Err(AppError::remote(
                    "Failed to parse timestamp from server response",
                ))
            }
        }

        res.get("timestamp")
            .map(timestamp_to_string)
            .transpose()?
            .ok_or_else(|| AppError::remote("Failed to parse timestamp from server response"))
    }

    async fn do_checkin_with_ts(
        &self,
        student_id: &str,
        schedule_id: &str,
        ts: &str,
    ) -> AppResult<Value> {
        let sess = self.ensure_session(student_id).await?;
        let params = [("courseSchedId", schedule_id), ("timestamp", ts)];
        let res = self
            .http
            .post(format!("{CHECKIN_URL}?id={}", sess.class_id))
            .header("Sessionid", &sess.login_name)
            .query(&params)
            .send()
            .await?
            .json::<ClassRes<Value>>()
            .await;

        let result = match res {
            Ok(r) => {
                if r.status == "4001" || r.status == "401" {
                    warn!(student_id = %student_id, "session expired, re-logging in");
                    self.clear_student_state(student_id);
                    let sess2 = self.ensure_session(student_id).await?;
                    self.http
                        .post(format!("{CHECKIN_URL}?id={}", sess2.class_id))
                        .header("Sessionid", &sess2.login_name)
                        .query(&params)
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

        if result
            .get("stuSignStatus")
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            != "1"
        {
            return Err(AppError::remote("Checkin failed"));
        }
        info!(
            student = student_id,
            sched = schedule_id,
            stu_sign_id = %result.get("stuSignId").and_then(|v| v.as_str()).unwrap_or("?"),
            "checkin ok via 8082 (fallback)"
        );

        Ok(result)
    }
}

fn timestamp_to_string(value: &Value) -> AppResult<String> {
    if let Some(s) = value.as_str() {
        return Ok(s.to_string());
    }
    if let Some(n) = value.as_i64() {
        return Ok(n.to_string());
    }
    if let Some(n) = value.as_u64() {
        return Ok(n.to_string());
    }
    Err(AppError::remote(
        "Failed to parse timestamp value from server",
    ))
}

/// HMAC-SHA256("JYDSIGN" + taskid + nonce + "stu_scan_sign", signSecret) as hex.
/// Verified against a live 8081 success on 2026-11-19.
fn sign_checkin(taskid: &str, secret: &str, nonce: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac accepts any key len");
    mac.update(format!("JYDSIGN{taskid}{nonce}{SIGN_PAYLOAD_SUFFIX}").as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn extract_input_value(html: &str, name: &str) -> Option<String> {
    let needle = format!("name=\"{name}\"");
    let pos = html.find(&needle)?;
    let tail = &html[pos..];
    let value_pos = tail.find("value=\"")?;
    let rest = &tail[value_pos + 7..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_login_name_from_url(url: &str) -> Option<String> {
    for part in url.split(['?', '#', '&']) {
        if let Some(value) = part.strip_prefix("loginName=") {
            return Some(percent_decode(value));
        }
    }
    None
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = from_hex(bytes[i + 1]);
                let lo = from_hex(bytes[i + 2]);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi << 4) | lo);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

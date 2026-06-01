use std::time::{Duration, Instant};

use dashmap::DashMap;
use reqwest::{header::LOCATION, redirect::Policy, Client};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    config::StudentEntry,
    error::{AppError, AppResult},
};

const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ",
    "AppleWebKit/537.36 (KHTML, like Gecko) ",
    "Chrome/130.0.0.0 Safari/537.36"
);

const SCHEDULE_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const SSO_LOGIN_URL: &str = "https://sso.buaa.edu.cn/login";
const SSO_VERIFY_URL: &str = "https://uc.buaa.edu.cn/";
const JUMP_URL: &str = "https://iclass.buaa.edu.cn:8346/?type=jumpMyCenter";
const LOGIN_BUAA_URL: &str = "https://iclass.buaa.edu.cn:8346/eschool/app/user/login_buaa.do";
const SCHEDULE_URL: &str = "https://iclass.buaa.edu.cn:8347/app/course/get_stu_course_sched.action";
const COURSE_DETAIL_URL: &str = "https://iclass.buaa.edu.cn:8347/app/my/get_my_course_sign_detail.action";
const TIMESTAMP_URL: &str = "http://iclass.buaa.edu.cn:8081/app/common/get_timestamp.action";
const CHECKIN_URL: &str = "http://iclass.buaa.edu.cn:8081/eschool/app/course/stu_scan_sign.action";

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
            "2" => Err(AppError::remote("iclass status=2 msg=Some(\"Empty data list\")")),
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
        if self.status_raw == "1" { 1 } else { 0 }
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
        if self.status_raw == "1" { 1 } else { 0 }
    }
}

pub struct ClassClient {
    http: Client,
    sso_http: Client,
    sessions: DashMap<String, Session>,
    schedule_cache: DashMap<(String, String), (Instant, Vec<Schedule>)>,
    accounts: DashMap<String, Account>,
}

impl ClassClient {
    pub fn new(students: &[StudentEntry]) -> Self {
        let http = Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(15))
            .user_agent(USER_AGENT)
            .build()
            .expect("build reqwest client");
        let sso_http = Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(20))
            .user_agent(USER_AGENT)
            .redirect(Policy::none())
            .cookie_store(true)
            .build()
            .expect("build sso client");

        let client = Self {
            http,
            sso_http,
            sessions: DashMap::new(),
            schedule_cache: DashMap::new(),
            accounts: DashMap::new(),
        };
        for student in students {
            client.accounts.insert(
                student.student_id.clone(),
                Account {
                    student_id: student.student_id.clone(),
                    sso_password: student.sso_password.clone(),
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

    async fn sso_login(&self, account: &Account) -> AppResult<()> {
        let res = self.sso_http.get(SSO_LOGIN_URL).send().await?;
        if res.url().as_str() == SSO_VERIFY_URL {
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

        let res = self
            .sso_http
            .post(SSO_LOGIN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_body)
            .send()
            .await?;
        if res.status().is_success() {
            return Ok(());
        }

        let bytes = res.bytes().await?;
        let html = String::from_utf8_lossy(&bytes);
        if html.contains("continueForm") {
            let execution = extract_input_value(&html, "execution").ok_or_else(|| {
                AppError::remote("SSO 风险继续页缺少 execution")
            })?;
            let form = [("execution", execution.as_str()), ("_eventId", "ignoreAndContinue")];
            let form_body = serde_urlencoded::to_string(form)
                .map_err(|e| AppError::internal(format!("encode sso continue form failed: {e}")))?;
            let retry = self
                .sso_http
                .post(SSO_LOGIN_URL)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(form_body)
                .send()
                .await?;
            if retry.status().is_success() {
                return Ok(());
            }
        }

        Err(AppError::remote("SSO 登录失败，请检查学号或密码"))
    }

    async fn fetch_login_name(&self, student_id: &str) -> AppResult<String> {
        let res = self.sso_http.get(JUMP_URL).send().await?;
        let final_url = if res.status().is_redirection() {
            res.headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| res.url().as_str().to_string())
        } else {
            res.url().as_str().to_string()
        };

        extract_login_name_from_url(&final_url)
            .ok_or_else(|| AppError::remote(format!("未能为 {student_id} 解析 login_name")))
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
        self.sso_login(&account).await?;
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

        let result = match self.iclass_post(student_id, SCHEDULE_URL, &[("dateStr", date)]).await {
            Ok(v) => v,
            Err(e) if e.code == "remote_error" && e.message.starts_with("iclass status=2") => vec![],
            Err(e) => return Err(e),
        };
        self.schedule_cache.insert(key, (Instant::now(), result.clone()));
        Ok(result)
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
        let ts = self.get_server_timestamp(student_id).await?;
        self.do_checkin_with_ts(student_id, schedule_id, &ts).await
    }

    async fn get_server_timestamp(&self, student_id: &str) -> AppResult<String> {
        let sess = self.ensure_session(student_id).await?;
        let res = self
            .http
            .post(format!("{TIMESTAMP_URL}?id={}", sess.class_id))
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
                    .post(format!("{TIMESTAMP_URL}?id={}", sess2.class_id))
                    .header("Sessionid", &sess2.login_name)
                    .send()
                    .await?
                    .json::<Value>()
                    .await?;
                return retry
                    .get("timestamp")
                    .map(timestamp_to_string)
                    .transpose()?
                    .ok_or_else(|| AppError::remote("Failed to parse timestamp from server response"));
            }
            Some(status) => {
                return Err(AppError::remote(format!(
                    "iclass status={} msg={:?}",
                    status,
                    res.get("ERRMSG").and_then(|v| v.as_str())
                )));
            }
            None => return Err(AppError::remote("Failed to parse timestamp from server response")),
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
    Err(AppError::remote("Failed to parse timestamp value from server"))
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

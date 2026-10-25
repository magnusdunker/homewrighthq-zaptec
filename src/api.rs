//! Zaptec's cloud API (api.zaptec.com).
//!
//! The user logs in with e-mail and password at `/oauth/token` (OAuth
//! password grant) and gets a bearer token that lasts an hour. The client
//! renews it a few minutes before it runs out, and on a 401 it logs in again
//! once and repeats the request.

use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Mutex;

pub const BASE_URL: &str = "https://api.zaptec.com";
/// Renew the token this long before it runs out.
const RENEW_BEFORE: Duration = Duration::from_secs(300);

/// Command ids for `sendCommand`.
pub const CMD_RESUME_CHARGING: u32 = 507;
pub const CMD_STOP_CHARGING_FINAL: u32 = 506;

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// Wrong e-mail or password.
    Auth,
    /// HTTP 429, with `Retry-After` when Zaptec sends it.
    RateLimited(Option<Duration>),
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Auth => write!(f, "Zaptec afviste e-mail eller adgangskode"),
            ApiError::RateLimited(_) => write!(f, "Zaptec beder om færre forespørgsler (HTTP 429)"),
            ApiError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        ApiError::Other(e.to_string())
    }
}

pub struct Client {
    http: reqwest::Client,
    base: String,
    username: String,
    password: String,
    token: Mutex<Option<(String, Instant)>>,
}

/// `application/x-www-form-urlencoded` encoding of one value.
fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Client {
    pub fn new(base: &str, username: &str, password: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("homewright-zaptec/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Self { http, base: base.trim_end_matches('/').to_string(), username: username.into(), password: password.into(), token: Mutex::new(None) }
    }

    /// Log in and keep the token.
    pub async fn login(&self) -> Result<String, ApiError> {
        let body = format!("grant_type=password&username={}&password={}&scope=openid", form_encode(&self.username), form_encode(&self.password));
        let res = self
            .http
            .post(format!("{}/oauth/token", self.base))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?;
        // a wrong password is 400 invalid_grant
        if res.status() == reqwest::StatusCode::BAD_REQUEST {
            return Err(ApiError::Auth);
        }
        let v = read(res).await?;
        let token = v["access_token"].as_str().filter(|t| !t.is_empty()).ok_or(ApiError::Auth)?.to_string();
        let lifetime = Duration::from_secs(v["expires_in"].as_u64().unwrap_or(3600));
        let renew_at = Instant::now() + lifetime.saturating_sub(RENEW_BEFORE);
        *self.token.lock().await = Some((token.clone(), renew_at));
        Ok(token)
    }

    async fn token(&self) -> Result<String, ApiError> {
        if let Some((t, renew_at)) = self.token.lock().await.clone() {
            if Instant::now() < renew_at {
                return Ok(t);
            }
        }
        self.login().await
    }

    /// GET with `body = None`, else POST. Logs in again once on 401.
    async fn send(&self, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let url = format!("{}{path}", self.base);
            let req = match body {
                // a command has no body
                Some(Value::Null) => self.http.post(url).body(""),
                Some(b) => self.http.post(url).json(b),
                None => self.http.get(url),
            };
            let res = req.bearer_auth(token).header("Accept", "application/json").send().await?;
            if res.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                *self.token.lock().await = None;
                continue;
            }
            return read(res).await;
        }
        Err(ApiError::Auth)
    }

    /// The chargers the user can see: `{ "Pages", "Data": [ ... ] }`.
    pub async fn chargers(&self) -> Result<Value, ApiError> {
        self.send("/api/chargers", None).await
    }

    /// The latest observations of a charger: `[ { "StateId", "ValueAsString" } ]`.
    pub async fn charger_state(&self, charger_id: &str) -> Result<Value, ApiError> {
        self.send(&format!("/api/chargers/{charger_id}/state"), None).await
    }

    /// An installation with its current limits.
    pub async fn installation(&self, installation_id: &str) -> Result<Value, ApiError> {
        self.send(&format!("/api/installation/{installation_id}"), None).await
    }

    /// Send a command such as [`CMD_RESUME_CHARGING`].
    pub async fn send_command(&self, charger_id: &str, command: u32) -> Result<Value, ApiError> {
        self.send(&format!("/api/chargers/{charger_id}/sendCommand/{command}"), Some(&Value::Null)).await
    }

    /// Change installation settings, such as the available current.
    pub async fn update_installation(&self, installation_id: &str, body: &Value) -> Result<Value, ApiError> {
        self.send(&format!("/api/installation/{installation_id}/update"), Some(body)).await
    }
}

async fn read(res: reqwest::Response) -> Result<Value, ApiError> {
    let status = res.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let after = res.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok()).map(Duration::from_secs);
        return Err(ApiError::RateLimited(after));
    }
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(ApiError::Auth);
    }
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(ApiError::Other(format!("HTTP {status}: {}", text.chars().take(200).collect::<String>())));
    }
    let text = res.text().await?;
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|e| ApiError::Other(format!("ugyldigt svar: {e}")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn form_encoding() {
        assert_eq!(super::form_encode("a+b@c.dk"), "a%2Bb%40c.dk");
        assert_eq!(super::form_encode("æ ø"), "%C3%A6+%C3%B8");
    }
}

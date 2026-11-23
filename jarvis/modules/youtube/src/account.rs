use crate::remote::{AccountProfile, Remote, TV_AGENT, WEB_VERSION};
use jarvis_home_common::{Host, now, random, save, text};
use reqwest::header::{AUTHORIZATION, COOKIE, HeaderMap, HeaderValue};
use serde_json::{Value, json};
use std::{
    fs,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};

const CLIENT: &str = "861556708454-d6dlm3lh05idd8npek18k6be8ba3oc68.apps.googleusercontent.com";
const KEY: &str = "SboVhoG9s0rNafixCSGGKXAT"; // gitleaks:allow -- public YouTube TV client identifier
pub const COOKIES: &str = "youtube_cookies.txt";

#[derive(Default)]
struct Login {
    code: String,
    url: String,
    error: String,
    task: Option<JoinHandle<()>>,
}

pub struct Account {
    host: Arc<Host>,
    flow: Mutex<Login>,
    token: AsyncMutex<Option<(Instant, String, String)>>,
    profile: AsyncMutex<Option<(Instant, AccountProfile)>>,
}

impl Account {
    pub fn new(host: Arc<Host>) -> Self {
        Self {
            host,
            flow: Mutex::default(),
            token: AsyncMutex::default(),
            profile: AsyncMutex::default(),
        }
    }

    pub async fn clear_profile_cache(&self) {
        *self.profile.lock().await = None;
    }

    pub async fn state(&self, remote: &Remote) -> Result<Value, String> {
        let auth = self.host.config().await?["auth"].clone();
        let refresh = text(&auth["refresh"]);
        let mut name = text(&auth["name"]);
        let mut handle = text(&auth["handle"]);
        let mut email = text(&auth["email"]);
        let signed_in = !refresh.is_empty();
        let needs_lookup =
            (name.is_empty() || name == "Аккаунт Google") || (handle.is_empty() && email.is_empty());
        if (signed_in && needs_lookup) || (!signed_in && self.has_cookies()) {
            let cached = {
                let guard = self.profile.lock().await;
                guard.as_ref().and_then(|(at, profile)| {
                    let ttl = if profile.is_empty() {
                        Duration::from_secs(60)
                    } else {
                        Duration::from_secs(1800)
                    };
                    (at.elapsed() < ttl).then(|| profile.clone())
                })
            };
            let resolved = match cached {
                Some(profile) => profile,
                None => {
                    let headers = if signed_in {
                        self.headers(false).await.unwrap_or_default()
                    } else {
                        self.cookie_headers()
                    };
                    let profile = if headers.is_empty() {
                        AccountProfile::default()
                    } else {
                        remote.account_profile(headers).await.unwrap_or_default()
                    };
                    *self.profile.lock().await = Some((Instant::now(), profile.clone()));
                    profile
                }
            };
            let mut improved = false;
            if !resolved.name.is_empty() && (name.is_empty() || name == "Аккаунт Google") {
                name = resolved.name;
                improved = true;
            }
            if handle.is_empty() && !resolved.handle.is_empty() {
                handle = resolved.handle;
                improved = true;
            }
            if email.is_empty() && !resolved.email.is_empty() {
                email = resolved.email;
                improved = true;
            }
            if signed_in && improved {
                let _ = self
                    .host
                    .set(
                        auth,
                        json!({"refresh": refresh, "name": name, "handle": handle, "email": email}),
                        true,
                    )
                    .await;
            }
        }
        let flow = self.flow.lock().unwrap();
        Ok(json!({
            "signed_in": signed_in,
            "name": name,
            "handle": handle,
            "email": email,
            "code": flow.code,
            "url": flow.url,
            "error": flow.error
        }))
    }

    pub async fn begin(self: &Arc<Self>, remote: &Arc<Remote>) -> Result<Value, String> {
        let mut token = self.token.lock().await;
        let device = hex::encode(random::<16>()?);
        let reply = self
            .form(
                "device/code",
                &[
                    (
                        "scope",
                        "http://gdata.youtube.com https://www.googleapis.com/auth/youtube",
                    ),
                    ("device_id", &device),
                    ("device_model", "ytlr::"),
                ],
            )
            .await?;
        let code = text(&reply["user_code"]);
        let device_code = text(&reply["device_code"]);
        if code.is_empty() || device_code.is_empty() {
            return Err(format!("Google не выдал код {}", text(&reply["error"])));
        }
        let interval = reply["interval"].as_u64().unwrap_or(5).clamp(5, 60);
        let ttl = reply["expires_in"].as_u64().unwrap_or(1800).clamp(5, 1800);
        *token = None;
        {
            let mut flow = self.flow.lock().unwrap();
            if let Some(task) = flow.task.take() {
                task.abort();
            }
            flow.code = code;
            flow.url = ["verification_url", "verification_uri"]
                .into_iter()
                .map(|key| text(&reply[key]))
                .find(|url| {
                    reqwest::Url::parse(url).ok().is_some_and(|u| {
                        u.scheme() == "https"
                            && matches!(
                                u.host_str(),
                                Some(
                                    "google.com"
                                        | "www.google.com"
                                        | "youtube.com"
                                        | "www.youtube.com"
                                )
                            )
                    })
                })
                .unwrap_or_else(|| "https://www.google.com/device".into());
            flow.error.clear();
            let account = self.clone();
            let remote_bg = Arc::clone(remote);
            flow.task = Some(tokio::spawn(async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(ttl),
                    account.poll(&remote_bg, &device_code, interval),
                )
                .await
                .unwrap_or_else(|_| Err("Код истёк".into()));
                let mut flow = account.flow.lock().unwrap();
                flow.code.clear();
                flow.url.clear();
                flow.error = result.err().unwrap_or_default();
                flow.task.take();
            }));
        }
        drop(token);
        self.state(remote).await
    }

    async fn poll(&self, remote: &Remote, code: &str, mut interval: u64) -> Result<(), String> {
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let Ok(reply) = self
                .form(
                    "token",
                    &[
                        ("client_secret", KEY),
                        ("code", code),
                        ("grant_type", "http://oauth.net/grant_type/device/1.0"),
                    ],
                )
                .await
            else {
                continue;
            };
            match reply["error"].as_str().unwrap_or_default() {
                "" => {}
                "authorization_pending" => continue,
                "slow_down" => {
                    interval += 5;
                    continue;
                }
                error => return Err(format!("Вход в YouTube: {error}")),
            }
            let refresh = text(&reply["refresh_token"]);
            let access = text(&reply["access_token"]);
            if refresh.is_empty() || access.is_empty() {
                return Err("Google не выдал токен YouTube".into());
            }
            let profile = if let Ok(mut value) = HeaderValue::from_str(&format!("Bearer {access}")) {
                value.set_sensitive(true);
                let mut headers = HeaderMap::new();
                headers.insert(AUTHORIZATION, value);
                remote.account_profile(headers).await.unwrap_or_default()
            } else {
                AccountProfile::default()
            };
            let name = if profile.name.is_empty() {
                "Аккаунт Google".to_string()
            } else {
                profile.name.clone()
            };
            let mut token = self.token.lock().await;
            let before = self.host.config().await?["auth"].clone();
            self.host
                .set(
                    before,
                    json!({
                        "refresh": refresh,
                        "name": name,
                        "handle": profile.handle,
                        "email": profile.email
                    }),
                    true,
                )
                .await?;
            *token = Some((expiry(&reply), refresh, access));
            *self.profile.lock().await = Some((Instant::now(), profile));
            return Ok(());
        }
    }

    pub async fn logout(&self) -> Result<(), String> {
        let mut token = self.token.lock().await;
        self.close();
        *token = None;
        *self.profile.lock().await = None;
        let before = self.host.config().await?["auth"].clone();
        self.host
            .set(before, json!({"refresh":"","name":"","handle":"","email":""}), false)
            .await
    }

    pub fn close(&self) {
        let old = std::mem::take(&mut *self.flow.lock().unwrap());
        if let Some(task) = old.task {
            task.abort();
        }
    }

    async fn form(&self, path: &str, values: &[(&str, &str)]) -> Result<Value, String> {
        let mut fields = values.to_vec();
        fields.push(("client_id", CLIENT));
        let response = self
            .host
            .client()
            .await?
            .post(format!("https://www.youtube.com/o/oauth2/{path}"))
            .header("User-Agent", TV_AGENT)
            .form(&fields)
            .send()
            .await
            .map_err(|_| "Нет соединения с Google")?;
        let status = response.status();
        // OAuth errors return HTTP 400 with a machine-readable reason.
        let mut data = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|_| "Обрыв ответа Google")?
        {
            if data.len() + chunk.len() > 1 << 20 {
                return Err("Слишком большой ответ Google".into());
            }
            data.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&data).map_err(|_| format!("Google ответил {status}"))
    }

    pub async fn headers(&self, cookies_first: bool) -> Result<HeaderMap, String> {
        let cookies = self.cookie_headers();
        if cookies_first && !cookies.is_empty() {
            return Ok(cookies);
        }
        let mut token = self.token.lock().await;
        let auth = self.host.config().await?["auth"].clone();
        let refresh = text(&auth["refresh"]);
        if refresh.is_empty() {
            return Ok(cookies);
        }
        let access = if let Some((exp, _, access)) = token
            .as_ref()
            .filter(|(exp, key, _)| *exp > Instant::now() && key == &refresh)
        {
            let _ = exp;
            access.clone()
        } else {
            let reply = self
                .form(
                    "token",
                    &[
                        ("client_secret", KEY),
                        ("refresh_token", &refresh),
                        ("grant_type", "refresh_token"),
                    ],
                )
                .await?;
            let access = text(&reply["access_token"]);
            if access.is_empty() {
                if reply["error"] == "invalid_grant" {
                    self.host
                        .set(auth, json!({"refresh":"","name":"","handle":"","email":""}), false)
                        .await?;
                    *token = None;
                    *self.profile.lock().await = None;
                    return Ok(cookies);
                }
                return Err("Google не обновил токен YouTube".into());
            }
            *token = Some((expiry(&reply), refresh, access.clone()));
            access
        };
        let mut headers = HeaderMap::new();
        let mut value = HeaderValue::from_str(&format!("Bearer {access}"))
            .map_err(|_| "Неверный токен YouTube")?;
        value.set_sensitive(true);
        headers.insert(AUTHORIZATION, value);
        Ok(headers)
    }

    pub fn has_cookies(&self) -> bool {
        self.host.data.join(COOKIES).is_file()
    }

    pub async fn is_signed_in(&self) -> bool {
        if self.has_cookies() {
            return true;
        }
        let Ok(config) = self.host.config().await else {
            return false;
        };
        !text(&config["auth"]["refresh"]).is_empty()
    }

    pub fn web_headers(&self) -> HeaderMap {
        self.cookie_headers()
    }

    fn cookie_headers(&self) -> HeaderMap {
        fs::read_to_string(self.host.data.join(COOKIES))
            .ok()
            .and_then(|data| parse_cookies(&data).ok())
            .unwrap_or_default()
    }

    pub async fn set_cookies(&self, remote: &Remote, data: &str) -> Result<(), String> {
        if data.len() > 200 << 10 {
            return Err("Файл cookies.txt слишком большой".into());
        }
        remote.check_cookies(parse_cookies(data)?).await?;
        save(&self.host.data.join(COOKIES), data.as_bytes())
    }

    pub fn clear_cookies(&self) -> Result<(), String> {
        match fs::remove_file(self.host.data.join(COOKIES)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

fn expiry(reply: &Value) -> Instant {
    Instant::now()
        + Duration::from_secs(
            reply["expires_in"]
                .as_u64()
                .unwrap_or(3600)
                .clamp(61, 86400)
                - 60,
        )
}

fn parse_cookies(data: &str) -> Result<HeaderMap, String> {
    let mut cookies = std::collections::BTreeMap::new();
    let timestamp = now() / 1000;
    for line in data.lines() {
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.starts_with('#') {
            continue;
        }
        let parts: Vec<_> = line.split('\t').collect();
        let [domain, _, path, _, expiration, name, value] = parts.as_slice() else {
            continue;
        };
        let domain = domain.trim_start_matches('.');
        if !matches!(domain, "youtube.com" | "www.youtube.com")
            || !"/youtubei/v1/player".starts_with(path)
        {
            continue;
        }
        if !expiration
            .parse::<i64>()
            .is_ok_and(|v| v == 0 || v > timestamp)
        {
            continue;
        }
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            || value.is_empty()
            || !value
                .bytes()
                .all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
        {
            continue;
        }
        cookies.insert(*name, *value);
    }
    let sid = cookies
        .get("SAPISID")
        .or_else(|| cookies.get("__Secure-3PAPISID"))
        .filter(|_| {
            cookies.contains_key("SID")
                || cookies.contains_key("__Secure-1PSID")
                || cookies.contains_key("__Secure-3PSID")
        })
        .ok_or("В cookies.txt нет действующего входа в YouTube")?;
    let origin = "https://www.youtube.com";
    let hash = |value: &str| {
        let input = format!("{timestamp} {value} {origin}");
        let digest =
            ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, input.as_bytes());
        format!("{timestamp}_{}", hex::encode(digest.as_ref()))
    };
    let mut auth = format!("SAPISIDHASH {}", hash(sid));
    for (name, prefix) in [
        ("__Secure-1PAPISID", "SAPISID1PHASH"),
        ("__Secure-3PAPISID", "SAPISID3PHASH"),
    ] {
        if let Some(value) = cookies.get(name) {
            auth += &format!(" {prefix} {}", hash(value));
        }
    }
    let header = cookies
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    let mut out = HeaderMap::new();
    for (name, value) in [(AUTHORIZATION, auth), (COOKIE, header)] {
        let mut value = HeaderValue::from_str(&value).map_err(|_| "Неверный формат кук YouTube")?;
        value.set_sensitive(true);
        out.insert(name, value);
    }
    out.insert("X-Origin", HeaderValue::from_static(origin));
    out.insert("Origin", HeaderValue::from_static(origin));
    out.insert("X-Goog-AuthUser", HeaderValue::from_static("0"));
    out.insert("X-YouTube-Client-Name", HeaderValue::from_static("1"));
    out.insert(
        "X-YouTube-Client-Version",
        HeaderValue::from_static(WEB_VERSION),
    );
    Ok(out)
}

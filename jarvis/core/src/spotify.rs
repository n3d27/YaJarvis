#![allow(dead_code)] // подключается к music.rs в следующих задачах
//! Spotify как источник музыки: конфиг ключей, состояние Soloist, поиск Web API и сверка результата с запросом.
//! Ключи и токены нигде не логируются.

use serde::Serialize;
use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CONF_PATH: &str = "/data/opt/soloist/spotify_api.conf";
const CONTROL_SH: &str = "/data/opt/soloist/scripts/control.sh";
const SOLOIST_BIN: &str = "/data/opt/soloist/bin/soloist";
const SOLOIST_LIB: &str = "/data/opt/soloist/lib64";
const ACCEPT_SCORE: f32 = 0.6;

#[derive(Clone, Default)]
pub struct SpotifyConf {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    pub liked_uri: String,
}

impl std::fmt::Debug for SpotifyConf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SpotifyConf(..)")
    }
}

impl SpotifyConf {
    pub fn parse(text: &str) -> SpotifyConf {
        let mut c = SpotifyConf::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            let v = if v.len() >= 2
                && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
            {
                &v[1..v.len() - 1]
            } else {
                v
            };
            match k.trim() {
                "SPOTIFY_CLIENT_ID" => c.client_id = v.to_string(),
                "SPOTIFY_CLIENT_SECRET" => c.client_secret = v.to_string(),
                "SPOTIFY_REFRESH_TOKEN" => c.refresh_token = v.to_string(),
                "SPOTIFY_LIKED_URI" => c.liked_uri = v.to_string(),
                _ => {}
            }
        }
        c
    }

    /// Читает конфиг колонки; при любой ошибке — пустой (поиск недоступен).
    pub fn load() -> SpotifyConf {
        std::fs::read_to_string(CONF_PATH).map(|t| SpotifyConf::parse(&t)).unwrap_or_default()
    }

    pub fn can_search(&self) -> bool {
        !self.client_id.is_empty() && !self.client_secret.is_empty()
    }
}

#[derive(Clone, Default, Serialize)]
pub struct SpotState {
    pub status: String,
    pub uri: String,
    pub title: String,
    pub artists: String,
    pub album: String,
    pub cover: String,
    pub pos_ms: u64,
    pub dur_ms: u64,
    pub context: String,
    pub shuffle: bool,
}

/// Разбор `soloist ctl now --json`. Мусор и состояние без трека — `None`.
pub fn parse_now(json: &str) -> Option<SpotState> {
    let v: Value = serde_json::from_str(json.trim()).ok()?;
    let item = v.get("item")?;
    let uri = item.get("uri")?.as_str()?.to_string();
    if uri.is_empty() {
        return None;
    }
    let deco = item.get("decorations");
    let name_of = |n: Option<&Value>| -> String {
        n.and_then(|d| d.pointer("/identity/name")).and_then(Value::as_str).unwrap_or("").to_string()
    };
    let artists = deco
        .and_then(|d| d.get("creators"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|c| name_of(c.pointer("/entity/decorations")))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let covers = deco.and_then(|d| d.pointer("/visual_identity/cover")).and_then(Value::as_array);
    let cover = covers
        .and_then(|a| {
            a.iter()
                .find(|c| c.get("size").and_then(Value::as_str) == Some("large"))
                .or_else(|| a.last())
        })
        .and_then(|c| c.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let status = match v.get("status").and_then(Value::as_str) {
        Some(s @ ("playing" | "paused")) => s.to_string(),
        _ => String::new(),
    };
    Some(SpotState {
        status,
        uri,
        title: name_of(deco),
        artists,
        album: name_of(deco.and_then(|d| d.pointer("/parent/entity/decorations"))),
        cover,
        pos_ms: v.pointer("/position/position_ms").and_then(Value::as_u64).unwrap_or(0),
        dur_ms: deco.and_then(|d| d.pointer("/playback/duration_ms")).and_then(Value::as_u64).unwrap_or(0),
        context: v.pointer("/context/uri").and_then(Value::as_str).unwrap_or("").to_string(),
        shuffle: v.pointer("/options/shuffle").and_then(Value::as_bool).unwrap_or(false),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Any,
    Track,
    Artist,
    Album,
    Playlist,
    Podcast,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub uri: String,
    pub label: String,
}

const STOP_WORDS: &[&str] = &[
    "песню", "песня", "трек", "альбом", "группы", "группу", "исполнителя", "плейлист", "подкаст", "выпуск", "эпизод", "the",
];

fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars().flat_map(|c| c.to_lowercase()) {
        let ch = if ch == 'ё' { 'е' } else { ch };
        out.push(if ch.is_alphanumeric() { ch } else { ' ' });
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Кириллица → латиница: «Кино» в запросе должно совпасть с артистом Kino.
fn translit(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let t = match ch {
            'а' => "a", 'б' => "b", 'в' => "v", 'г' => "g", 'д' => "d", 'е' => "e", 'ж' => "zh", 'з' => "z",
            'и' => "i", 'й' => "y", 'к' => "k", 'л' => "l", 'м' => "m", 'н' => "n", 'о' => "o", 'п' => "p",
            'р' => "r", 'с' => "s", 'т' => "t", 'у' => "u", 'ф' => "f", 'х' => "h", 'ц' => "ts", 'ч' => "ch",
            'ш' => "sh", 'щ' => "sch", 'ъ' | 'ь' => "", 'ы' => "y", 'э' => "e", 'ю' => "yu", 'я' => "ya",
            _ => {
                out.push(ch);
                continue;
            }
        };
        out.push_str(t);
    }
    out
}

fn query_words(query: &str) -> Vec<String> {
    norm(query).split(' ').filter(|w| !w.is_empty() && !STOP_WORDS.contains(w)).map(str::to_string).collect()
}

fn word_hit(word: &str, hay: &str) -> bool {
    if word.chars().count() >= 3 {
        hay.contains(word)
    } else {
        hay.split(' ').any(|t| t == word)
    }
}

fn score(words: &[String], hay: &str) -> f32 {
    if words.is_empty() {
        return 0.0;
    }
    let plain = norm(hay);
    let plain_hits = words.iter().filter(|w| word_hit(w, &plain)).count();
    let tr_hay = translit(&plain);
    let tr_hits = words.iter().filter(|w| word_hit(&translit(w), &tr_hay)).count();
    plain_hits.max(tr_hits) as f32 / words.len() as f32
}

fn s<'a>(v: &'a Value, path: &str) -> &'a str {
    v.pointer(path).and_then(Value::as_str).unwrap_or("")
}

fn artist_names(v: &Value) -> String {
    v.get("artists")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("name").and_then(Value::as_str)).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

struct Cand {
    uri: String,
    label: String,
    hay: String,
    is_artist: bool,
    name: String,
}

fn candidates(kind: Kind, search: &Value) -> Vec<Cand> {
    let sections: &[&str] = match kind {
        Kind::Any => &["tracks", "artists", "albums", "playlists"],
        Kind::Track => &["tracks"],
        Kind::Artist => &["artists"],
        Kind::Album => &["albums"],
        Kind::Playlist => &["playlists"],
        Kind::Podcast => &["shows", "episodes"],
    };
    let mut out = Vec::new();
    for sec in sections {
        let Some(items) = search.pointer(&format!("/{sec}/items")).and_then(Value::as_array) else { continue };
        for it in items {
            if it.is_null() {
                continue;
            }
            let uri = s(it, "/uri");
            let name = s(it, "/name");
            if uri.is_empty() || name.is_empty() {
                continue;
            }
            let (label, hay, is_artist) = match *sec {
                "tracks" | "albums" => {
                    let a = artist_names(it);
                    let label = if a.is_empty() { name.to_string() } else { format!("{a} — {name}") };
                    (label, format!("{name} {a}"), false)
                }
                "artists" => (name.to_string(), name.to_string(), true),
                "playlists" => (name.to_string(), format!("{name} {}", s(it, "/owner/display_name")), false),
                "shows" => (name.to_string(), format!("{name} {}", s(it, "/publisher")), false),
                _ => (name.to_string(), format!("{name} {} {}", s(it, "/show/name"), s(it, "/show/publisher")), false),
            };
            out.push(Cand { uri: uri.to_string(), label, hay, is_artist, name: name.to_string() });
        }
    }
    out
}

/// Сверка: берём первый результат Spotify, в котором узнаётся запрос (≥ 0.6 слов).
pub fn pick(query: &str, kind: Kind, search: &Value) -> Option<Pick> {
    let words = query_words(query);
    if words.is_empty() {
        return None;
    }
    let cands = candidates(kind, search);
    let q_full = words.join(" ");
    if kind == Kind::Any {
        let q_tr = translit(&q_full);
        let exact = cands.iter().find(|c| {
            c.is_artist && {
                let n = norm(&c.name);
                n == q_full || translit(&n) == q_tr
            }
        });
        if let Some(c) = exact {
            return Some(Pick { uri: c.uri.clone(), label: c.label.clone() });
        }
    }
    cands
        .iter()
        .find(|c| score(&words, &c.hay) >= ACCEPT_SCORE)
        .map(|c| Pick { uri: c.uri.clone(), label: c.label.clone() })
}

static TOKEN: Mutex<Option<(String, Instant)>> = Mutex::new(None);

async fn token(conf: &SpotifyConf) -> Result<String, String> {
    if let Some((t, exp)) = TOKEN.lock().unwrap().as_ref() {
        if Instant::now() < *exp {
            return Ok(t.clone());
        }
    }
    let r = crate::net::direct()
        .post("https://accounts.spotify.com/api/token")
        .basic_auth(&conf.client_id, Some(&conf.client_secret))
        .form(&[("grant_type", "client_credentials")])
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("spotify token: {}", crate::net::short(&e)))?;
    if !r.status().is_success() {
        return Err(format!("spotify token: HTTP {}", r.status().as_u16()));
    }
    let v: Value = r.json().await.map_err(|_| "spotify token: bad response".to_string())?;
    let t = v.get("access_token").and_then(Value::as_str).ok_or("spotify token: no access_token")?.to_string();
    let ttl = v.get("expires_in").and_then(Value::as_u64).unwrap_or(3600).saturating_sub(60).max(30);
    *TOKEN.lock().unwrap() = Some((t.clone(), Instant::now() + Duration::from_secs(ttl)));
    Ok(t)
}

fn search_types(kind: Kind) -> &'static str {
    match kind {
        Kind::Any => "track,artist,album,playlist",
        Kind::Track => "track",
        Kind::Artist => "artist",
        Kind::Album => "album",
        Kind::Playlist => "playlist",
        Kind::Podcast => "show,episode",
    }
}

/// Поиск в Spotify Web API (client credentials) и сверка. `Ok(None)` — ничего подходящего.
pub async fn search(conf: &SpotifyConf, query: &str, kind: Kind) -> Result<Option<Pick>, String> {
    if !conf.can_search() {
        return Err("spotify: ключи не заданы".into());
    }
    let tok = token(conf).await?;
    let r = crate::net::direct()
        .get("https://api.spotify.com/v1/search")
        .bearer_auth(&tok)
        .query(&[("q", query), ("type", search_types(kind)), ("limit", "5")])
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("spotify search: {}", crate::net::short(&e)))?;
    if r.status().as_u16() == 401 {
        *TOKEN.lock().unwrap() = None;
    }
    if !r.status().is_success() {
        return Err(format!("spotify search: HTTP {}", r.status().as_u16()));
    }
    let v: Value = r.json().await.map_err(|_| "spotify search: bad response".to_string())?;
    Ok(pick(query, kind, &v))
}

/// Текущее состояние Soloist; не запущен, завис или мусор — `None`.
pub async fn now() -> Option<SpotState> {
    let mut cmd = tokio::process::Command::new(SOLOIST_BIN);
    cmd.args(["ctl", "-w", "127.0.0.1:9090", "now", "--json"])
        .env("LD_LIBRARY_PATH", SOLOIST_LIB)
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    // при таймауте future отбрасывается, kill_on_drop убивает процесс
    let out = tokio::time::timeout(Duration::from_secs(4), cmd.output()).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    parse_now(&String::from_utf8_lossy(&out.stdout))
}

/// `sh control.sh <args…>`: 15 с для `search`, 8 с для остальных.
pub async fn ctl(args: &[&str]) -> Result<(), String> {
    let secs = if args.first() == Some(&"search") { 15 } else { 8 };
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg(CONTROL_SH)
        .args(args)
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(secs), cmd.output())
        .await
        .map_err(|_| format!("control.sh {}: таймаут {secs} с", args.first().unwrap_or(&"")))?
        .map_err(|e| format!("control.sh: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim().chars().take(200).collect::<String>();
        Err(format!("control.sh {}: код {}{}", args.first().unwrap_or(&""), out.status.code().unwrap_or(-1), if err.is_empty() { String::new() } else { format!(": {err}") }))
    }
}

#[cfg(test)]
fn fixture(name: &str) -> serde_json::Value {
    let text = match name {
        "search_kino.json" => include_str!("spotify/fixtures/search_kino.json"),
        "search_obzor.json" => include_str!("spotify/fixtures/search_obzor.json"),
        "search_podcast.json" => include_str!("spotify/fixtures/search_podcast.json"),
        _ => panic!("unknown fixture {name}"),
    };
    serde_json::from_str(text).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn conf_parses_quoted_and_plain() {
        let c = SpotifyConf::parse("SPOTIFY_CLIENT_ID=abc\nSPOTIFY_CLIENT_SECRET=\"s e\"\n# x\nSPOTIFY_LIKED_URI='spotify:user:u:collection'\n");
        assert_eq!(c.client_id, "abc"); assert_eq!(c.client_secret, "s e");
        assert_eq!(c.liked_uri, "spotify:user:u:collection"); assert!(c.refresh_token.is_empty());
        assert!(c.can_search());
        assert!(!SpotifyConf::parse("").can_search());
    }
    #[test] fn parse_now_reads_track() {
        let s = parse_now(include_str!("spotify/fixtures/now_playing.json")).unwrap();
        assert_eq!(s.status, "paused"); assert_eq!(s.title, "ONLY POSITIVE");
        assert_eq!(s.artists, "Endspiel, TumaniYO"); assert_eq!(s.album, "CHAMPION SOUND");
        assert_eq!(s.cover, "https://i.scdn.co/image/large");
        assert_eq!(s.pos_ms, 62762); assert_eq!(s.dur_ms, 178614); assert!(s.shuffle);
    }
    #[test] fn parse_now_rejects_garbage() { assert!(parse_now("").is_none()); assert!(parse_now("error: connection refused").is_none()); assert!(parse_now("{\"type\":\"playback_state\"}").map_or(true, |s| s.uri.is_empty())); }
    #[test] fn pick_artist_for_band_name() { let v = fixture("search_kino.json"); assert!(pick("кино", Kind::Any, &v).unwrap().uri.starts_with("spotify:artist:")); }
    #[test] fn pick_rejects_video_like_query() { let v = fixture("search_obzor.json"); assert!(pick("обзор айфона 17", Kind::Any, &v).is_none()); }
    #[test] fn pick_podcast_show() { let v = fixture("search_podcast.json"); assert!(pick("Lex Fridman Podcast", Kind::Podcast, &v).unwrap().uri.starts_with("spotify:show:")); }
    #[test] fn pick_skips_nulls() { let v = serde_json::json!({"tracks":{"items":[null]}}); assert!(pick("что угодно", Kind::Any, &v).is_none()); }
}

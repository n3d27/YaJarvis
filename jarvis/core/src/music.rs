//! Общий интерфейс музыки. Поиск, аккаунт и декодирование принадлежат модулю-провайдеру.
use crate::{
    alice_music::AliceMusic,
    fastcmd,
    plugins::Host,
    spotify::{self, SpotState},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Remote {
    pub playing: bool,
    pub active: bool,
    pub paused: bool,
    pub proposal: bool,
    pub personal_search: bool,
    pub repeat: bool,
    pub cur: Value,
    pub pos: f64,
    pub status: String,
}

/// Как долго пауза источника ещё считается «последней».
const PAUSE_FRESH: Duration = Duration::from_secs(30 * 60);
/// Сколько подряд неудачных опросов Soloist нужно, чтобы забыть состояние Spotify.
const SPOT_MAX_FAILS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Youtube,
    Alice,
    Spotify,
}

/// Кто сейчас владеет управлением: играющий источник, иначе последний на паузе (не старше 30 мин), иначе YouTube.
pub fn active_source(yt: &Remote, alice_active: bool, alice_playing: bool, sp: &SpotState, sp_paused_at: Option<Instant>) -> Source {
    if sp.status == "playing" {
        return Source::Spotify;
    }
    if yt.playing {
        return Source::Youtube;
    }
    if alice_active && alice_playing {
        return Source::Alice;
    }
    if sp.status == "paused" && sp_paused_at.is_some_and(|t| t.elapsed() < PAUSE_FRESH) {
        return Source::Spotify;
    }
    if alice_active && !yt.active {
        return Source::Alice;
    }
    Source::Youtube
}

/// Последнее известное состояние Spotify (по опросу Soloist) и служебные отметки времени.
#[derive(Default)]
pub struct SpotTrack {
    pub state: SpotState,
    pub paused_at: Option<Instant>,
    pub last_playing: Option<Instant>,
    pub updated: Option<Instant>,
    fails: u8,
}

pub struct Music {
    state: Mutex<Remote>,
    pub alice: Mutex<AliceMusic>,
    pub spotify: Mutex<SpotTrack>,
    button: Mutex<Button>,
    cancel: tokio::sync::watch::Sender<u64>,
    playing_tx: tokio::sync::watch::Sender<bool>,
}

#[derive(Default)]
struct Button {
    youtube_pause: bool,
    at: Option<Instant>,
}
impl Default for Music {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            alice: Mutex::default(),
            spotify: Mutex::default(),
            button: Mutex::default(),
            cancel: tokio::sync::watch::channel(0).0,
            playing_tx: tokio::sync::watch::channel(false).0,
        }
    }
}

impl Music {
    fn set_remote(&self, next: Remote) {
        let playing = next.playing;
        *self.state.lock().unwrap() = next;
        if playing {
            // YouTube заиграл позже — пауза Spotify больше не «последняя»
            self.spotify.lock().unwrap().paused_at = None;
        }
        let _ = self.playing_tx.send_if_modified(|cur| {
            if *cur == playing {
                false
            } else {
                *cur = playing;
                true
            }
        });
    }

    pub fn state(&self) -> Remote {
        self.state.lock().unwrap().clone()
    }

    pub fn playing(&self) -> bool {
        self.state.lock().unwrap().playing || self.alice.lock().unwrap().playing() || self.spotify.lock().unwrap().state.status == "playing"
    }

    pub fn source(&self) -> Source {
        let youtube = self.state.lock().unwrap();
        let alice = self.alice.lock().unwrap();
        let sp = self.spotify.lock().unwrap();
        active_source(&youtube, alice.active(), alice.playing(), &sp.state, sp.paused_at)
    }

    fn use_alice(&self) -> bool {
        self.source() == Source::Alice
    }

    /// Состояние Spotify с позицией, досчитанной до текущего момента.
    fn spotify_current(&self) -> SpotState {
        let sp = self.spotify.lock().unwrap();
        let mut st = sp.state.clone();
        if st.status == "playing" && let Some(at) = sp.updated {
            st.pos_ms += at.elapsed().as_millis() as u64;
            if st.dur_ms > 0 { st.pos_ms = st.pos_ms.min(st.dur_ms); }
        }
        st
    }

    pub fn spotify_snapshot(st: &SpotState) -> Value {
        let mut parts = st.uri.split(':');
        let link = match (parts.next(), parts.next(), parts.next()) {
            (Some("spotify"), Some(kind), Some(id)) if !id.is_empty() => format!("https://open.spotify.com/{kind}/{id}"),
            _ => String::new(),
        };
        json!({"source":"spotify", "playing":st.status == "playing", "paused":st.status != "playing",
            "track":{"ID":st.uri,"Title":st.title,"Author":st.artists},
            "pos":st.pos_ms as f64 / 1000.0, "dur":st.dur_ms as f64 / 1000.0, "cover":st.cover,
            "album":st.album, "link":link, "shuffle":st.shuffle})
    }

    /// Применяет результат опроса Soloist. `true` — Spotify только что перешёл в `playing`.
    fn set_spotify(&self, next: Option<SpotState>) -> bool {
        let mut sp = self.spotify.lock().unwrap();
        let next = next.filter(|n| !n.status.is_empty());
        let Some(next) = next else {
            sp.fails = sp.fails.saturating_add(1);
            if sp.fails >= SPOT_MAX_FAILS && !sp.state.status.is_empty() {
                sp.state = SpotState::default();
                sp.paused_at = None;
            }
            return false;
        };
        sp.fails = 0;
        let was = sp.state.status.clone();
        let now = Instant::now();
        let started = next.status == "playing" && was != "playing";
        if next.status == "playing" {
            sp.last_playing = Some(now);
            sp.paused_at = None;
        } else if was == "playing" {
            sp.paused_at = Some(now);
        }
        sp.state = next;
        sp.updated = Some(now);
        started
    }

    /// Оптимистичная отметка после команды, пока опрос не подтвердил.
    fn mark_spotify(&self, status: &str) {
        let mut sp = self.spotify.lock().unwrap();
        if sp.state.uri.is_empty() { return; }
        let was = std::mem::replace(&mut sp.state.status, status.to_string());
        sp.updated = Some(Instant::now());
        if status == "playing" {
            sp.paused_at = None;
            sp.last_playing = sp.updated;
        } else if was == "playing" {
            sp.paused_at = sp.updated;
        }
    }

    fn spotify_playing(&self) -> bool {
        self.spotify.lock().unwrap().state.status == "playing"
    }

    /// Другой источник собирается играть — Spotify уступает.
    async fn yield_spotify(&self) {
        if self.spotify_playing() {
            let _ = spotify::ctl(&["pause"]).await;
            self.mark_spotify("paused");
        }
    }

    /// Spotify заиграл (по опросу): YouTube и Алиса молчат.
    pub async fn on_spotify_started(&self, host: &Host) {
        if self.state().playing {
            let _ = self.call(host, "stop", Value::Null).await;
        }
        if self.alice.lock().unwrap().playing() {
            let _ = self.alice_control("pause", &Value::Null).await;
        }
    }

    /// Опрос Soloist: раз в секунду, пока Spotify играл в последние 30 минут, иначе раз в 5 секунд.
    pub async fn spotify_poll_loop(self: Arc<Self>, host: Arc<Host>) {
        loop {
            let st = spotify::now().await;
            let before = self.spotify.lock().unwrap().state.status.clone();
            let started = self.set_spotify(st);
            let after = self.spotify.lock().unwrap().state.status.clone();
            if before != after {
                log::info!("spotify: {} -> {}", if before.is_empty() { "idle" } else { &before }, if after.is_empty() { "idle" } else { &after });
            }
            if started {
                self.on_spotify_started(&host).await;
            }
            let hot = {
                let sp = self.spotify.lock().unwrap();
                sp.state.status == "playing" || sp.last_playing.is_some_and(|t| t.elapsed() < PAUSE_FRESH)
            };
            tokio::time::sleep(Duration::from_secs(if hot { 1 } else { 5 })).await;
        }
    }

    async fn spotify_control(&self, op: &str, args: &Value) -> Result<(), String> {
        match op {
            "pause" | "stop" => {
                spotify::ctl(&["pause"]).await?;
                self.mark_spotify("paused");
            }
            "resume" => {
                spotify::ctl(&["play"]).await?;
                self.mark_spotify("playing");
            }
            "next" | "related" => spotify::ctl(&["next"]).await?,
            "prev" => spotify::ctl(&["prev"]).await?,
            "like" => spotify::ctl(&["like"]).await?,
            "unlike" => spotify::ctl(&["unlike"]).await?,
            "shuffle" => spotify::ctl(&["shuffle", if args["on"].as_bool().unwrap_or(false) { "on" } else { "off" }]).await?,
            "repeat" => spotify::ctl(&["repeat", if args["on"].as_bool().unwrap_or(false) { "track" } else { "off" }]).await?,
            "seek" => {
                let cur = self.spotify_current();
                let ms = seek_target_ms(cur.pos_ms, cur.dur_ms, args["delta"].as_f64().unwrap_or(0.0), args["abs"].as_bool().unwrap_or(false));
                spotify::seek(ms).await?;
            }
            _ => return Err("эта команда не поддерживается для Spotify".into()),
        }
        Ok(())
    }

    fn spotify_description(&self) -> String {
        let st = self.spotify_current();
        let action = if st.status == "playing" { "Играет" } else { "На паузе" };
        if st.artists.is_empty() {
            format!("{action} «{}».", st.title)
        } else {
            format!("{action} «{}» — {}.", st.title, st.artists)
        }
    }

    pub async fn alice_control(&self, op: &str, args: &Value) -> Result<(), String> {
        let cmd = self.alice.lock().unwrap().command(op, args)?;
        if cmd.is_empty() { return Ok(()); }
        tokio::task::spawn_blocking(move || crate::alice::text(&cmd, true)).await.map_err(|e| e.to_string())?
    }

    pub async fn transport(&self, host: &Host, op: &str, args: Value) -> Result<(), String> {
        let source = self.source();
        if source == Source::Spotify && matches!(op, "pause" | "resume" | "next" | "prev" | "stop" | "seek" | "related" | "like" | "unlike" | "shuffle" | "repeat") {
            return self.spotify_control(op, &args).await;
        }
        if matches!(op, "like" | "unlike" | "shuffle") {
            return Err("Это работает только в Spotify.".into());
        }
        if matches!(op, "play" | "play_id") {
            self.yield_spotify().await;
        }
        if self.use_alice() && matches!(op, "pause" | "resume" | "next" | "prev" | "stop" | "seek" | "related") {
            self.alice_control(op, &args).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            return Ok(());
        }
        self.call(host, op, args).await.map(|_| ())
    }

    pub async fn stop_all(&self, host: &Host) {
        let stop_alice = self.alice.lock().unwrap().active();
        let _ = tokio::join!(self.call(host, "stop", Value::Null), async {
            if stop_alice { let _ = self.alice_control("stop", &Value::Null).await; }
        }, self.yield_spotify());
    }

    pub async fn cancel_loading(&self, host: &Host) {
        self.cancel.send_modify(|n| *n = n.wrapping_add(1));
        let should_stop = {
            let s = self.state.lock().unwrap();
            !s.playing && !s.paused
        };
        if should_stop {
            let _ = self.call(host, "stop", Value::Null).await;
        }
    }

    pub async fn alice_started(&self, host: &Host) {
        let from_button = {
            let mut button = self.button.lock().unwrap();
            let from_button = button.at.is_some_and(|at| at.elapsed() < Duration::from_secs(4));
            if !from_button { button.youtube_pause = false; }
            from_button
        };
        if from_button {
            let playing = self.alice.lock().unwrap().playing();
            if playing { let _ = self.alice_control("pause", &Value::Null).await; }
        } else if self.state().playing {
            let _ = self.call(host, "pause", Value::Null).await;
        }
        self.yield_spotify().await;
    }

    pub async fn pause_button(&self, host: &Host) {
        let next = {
            let youtube = self.state.lock().unwrap();
            let alice = self.alice.lock().unwrap();
            let mut button = self.button.lock().unwrap();
            if youtube.playing {
                button.at = Some(Instant::now());
                button.youtube_pause = true;
                "pause"
            } else if youtube.paused && !alice.playing() && (button.youtube_pause || alice.paused_since().is_none()) {
                button.at = Some(Instant::now());
                button.youtube_pause = false;
                "resume"
            } else { return; }
        };
        let _ = self.call(host, next, Value::Null).await;
    }

    pub fn event(&self, name: &str, data: Value) {
        match name {
            "music.state" => {
                if let Ok(st) = serde_json::from_value(data) {
                    self.set_remote(st);
                }
            }
            "exit" => {
                self.cancel.send_modify(|n| *n = n.wrapping_add(1));
                self.set_remote(Remote::default());
            }
            _ => {}
        }
    }

    pub async fn call(&self, host: &Host, op: &str, args: Value) -> Result<Value, String> {
        if op == "stop" {
            self.cancel.send_modify(|n| *n = n.wrapping_add(1));
        }
        let mut cancel = self.cancel.subscribe();
        let provider = host.provider("youtube").map_err(|_| {
            "Установите и включите модуль музыки YouTube на странице «Плагины».".to_string()
        })?;
        let timeout = if [
            "play",
            "play_id",
            "enqueue",
            "propose",
            "related",
            "resume_saved",
            "respond",
            "next",
        ]
        .contains(&op)
        {
            70
        } else if matches!(op, "cookies_set" | "account_begin") {
            30
        } else {
            10
        };
        let request = provider.request(
            "music",
            json!({"op": op, "args": args}),
            Duration::from_secs(timeout),
        );
        let cancellable = [
            "play",
            "play_id",
            "enqueue",
            "propose",
            "related",
            "resume_saved",
            "respond",
            "next",
        ]
        .contains(&op);
        let out = tokio::select! {
            biased;
            _ = cancel.changed(), if cancellable => return Err("операция отменена".into()),
            result = request => result?,
        };
        if let Some(st) = out.get("state").filter(|v| !v.is_null()) {
            let st =
                serde_json::from_value(st.clone()).map_err(|e| format!("состояние музыки: {e}"))?;
            self.set_remote(st);
        }
        if let Some(err) = out
            .get("error")
            .and_then(Value::as_str)
            .filter(|e| !e.is_empty())
        {
            return Err(err.into());
        }
        Ok(out.get("value").cloned().unwrap_or(Value::Null))
    }

    pub async fn snapshot(&self, host: &Host) -> Value {
        let youtube = match self.call(host, "snapshot", Value::Null).await {
            Ok(v) => v,
            Err(_) => {
                let s = self.state();
                json!({"source":"youtube", "playing":s.playing,"paused":s.paused,"repeat":s.repeat,"track":s.cur,"pos":s.pos})
            }
        };
        match self.source() {
            Source::Alice => {
                let mut alice = self.alice.lock().unwrap().snapshot();
                alice["history"] = youtube["history"].clone();
                alice
            }
            Source::Spotify => {
                let mut sp = Self::spotify_snapshot(&self.spotify_current());
                sp["history"] = youtube["history"].clone();
                sp
            }
            Source::Youtube => youtube,
        }
    }

    pub async fn play(&self, host: &Host, query: &str, queue: bool) -> String {
        let propose = recommendation(query);
        if !queue && !propose {
            self.yield_spotify().await;
        }
        let result = self
            .call(
                host,
                if queue {
                    "enqueue"
                } else if propose {
                    "propose"
                } else {
                    "play"
                },
                json!({"query":query}),
            )
            .await;
        match result {
            Ok(v) if queue => format!("В очереди: {}.", title(&v)),
            Ok(Value::String(s)) if propose => format!(
                "{s} Ролик ещё не включён. Дождись ответа человека и передай его в music_control."
            ),
            Ok(v) => format!("Включил с YouTube: {}.", title(&v)),
            Err(e) => format!("Не получилось: {e}"),
        }
    }

    pub async fn control(&self, host: &Host, text: &str) -> Option<String> {
        let command = command(text);
        if self.source() == Source::Spotify && let Some((op, args, answer)) = &command {
            if *op == "status" { return Some(self.spotify_description()); }
            return Some(match self.spotify_control(op, args).await {
                Ok(()) if *op == "related" => "Переключаю трек.".into(),
                Ok(()) => (*answer).into(),
                Err(e) => format!("Не получилось: {e}"),
            });
        }
        if self.use_alice() && let Some((op, args, answer)) = &command {
            if *op == "status" { return Some(self.alice.lock().unwrap().description()); }
            return Some(match self.alice_control(op, args).await {
                Ok(()) if *op == "related" => "Переключаю трек.".into(),
                Ok(()) => (*answer).into(),
                Err(e) => format!("Не получилось: {e}"),
            });
        }
        if self.state().proposal {
            match self.call(host, "respond", json!({"text":text})).await {
                Ok(v) if v["ok"].as_bool() == Some(true) => {
                    return Some(v["answer"].as_str().unwrap_or("").into());
                }
                Err(e) => return Some(format!("Не получилось: {e}")),
                _ => {}
            }
        }
        if !self.state().active {
            return None;
        }
        let (op, args, answer) = command?;
        if op == "status" {
            return Some(self.state().status);
        }
        if matches!(op, "like" | "unlike" | "shuffle") {
            return Some("Это работает только в Spotify.".into());
        }
        Some(match self.call(host, op, args).await {
            Err(e) => format!("Не получилось: {e}"),
            Ok(Value::String(s)) if op == "related" => s,
            Ok(_) => answer.into(),
        })
    }
}

fn title(v: &Value) -> &str {
    v.get("Title")
        .or_else(|| v.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("трек")
}

fn recommendation(text: &str) -> bool {
    let t = fastcmd::strip_lead(&fastcmd::norm_text(text));
    if [
        "рекомендац",
        "рекомендован",
        "что нибудь ",
        "подбери ",
        "выбери ",
        "предложи ",
    ]
    .iter()
    .any(|p| t.starts_with(p))
    {
        return true;
    }
    if ["мои рекомендации", "что то интересное"].contains(&t.as_str())
    {
        return true;
    }
    ["включи ", "поставь ", "запусти ", "давай ", "найди "]
        .iter()
        .any(|p| t.starts_with(p))
        && [
            "мои рекомендации",
            "моих рекомендац",
            "из рекомендац",
            "что нибудь",
            "что то интересное",
            "послушать",
            "чтобы болтало на фоне",
            "сам ",
            "самому",
            "на свой вкус",
        ]
        .iter()
        .any(|p| t.contains(p))
}

pub fn command(text: &str) -> Option<(&'static str, Value, &'static str)> {
    let lowered = text
        .to_lowercase()
        .replace('ё', "е")
        .replace([',', '.', '!', '?'], " ");
    let t = fastcmd::strip_lead(&lowered)
        .trim_matches(|c: char| c.is_whitespace() || ",.!?".contains(c))
        .trim_start_matches("пожалуйста ")
        .to_string();
    let starts = |s: &[&str]| s.iter().any(|p| t.starts_with(p));
    let simple = |op, answer| Some((op, Value::Null, answer));
    if starts(&[
        "что играет",
        "что сейчас играет",
        "что за пес",
        "что за трек",
        "кто поет",
        "как называется пес",
    ]) {
        return simple("status", "");
    }
    if starts(&["дизлайк", "не нравится", "убери из любимых", "убери лайк", "удали из любимых"]) {
        return simple("unlike", "Убрал из любимых.");
    }
    if starts(&["лайк", "нравится", "добавь в любимые", "поставь лайк"]) {
        return simple("like", "Добавил в любимые.");
    }
    if starts(&["не перемешивай", "по порядку", "выключи перемешивание", "отключи перемешивание"]) {
        return Some(("shuffle", json!({"on": false}), "Играю по порядку."));
    }
    if starts(&["перемешай", "вперемешку", "включи перемешивание"]) {
        return Some(("shuffle", json!({"on": true}), "Перемешиваю."));
    }
    if starts(&["сначала", "с начала", "в начало", "включи сначала"])
    {
        return Some(("seek", json!({"delta":0,"abs":true}), "С начала."));
    }
    if starts(&[
        "перемот",
        "промот",
        "мотни",
        "вперед на",
        "назад на",
        "отмотай",
    ]) {
        let (delta, abs) = seek(&t);
        return Some(("seek", json!({"delta":delta,"abs":abs}), "Перемотал."));
    }
    if starts(&["продолж", "сними с пауз", "сними пауз", "играй дальше"])
    {
        return simple("resume", "Продолжаю.");
    }
    if starts(&[
        "пауза",
        "на паузу",
        "поставь на паузу",
        "останови музыку",
        "приостанов",
    ]) {
        return simple("pause", "Пауза.");
    }
    if starts(&["выключи музыку", "выключи песню", "выруби музыку", "стоп"])
    {
        return simple("stop", "Выключил.");
    }
    if starts(&[
        "на повтор",
        "на репит",
        "включи повтор",
        "поставь на повтор",
        "поставь на репит",
        "включи репит",
        "повтор трека",
        "повторяй",
    ]) {
        return Some(("repeat", json!({"on": true}), "Повтор включён."));
    }
    if starts(&[
        "выключи повтор",
        "убери повтор",
        "выключи репит",
        "убери репит",
        "без повтора",
        "отключи повтор",
        "не повторяй",
    ]) {
        return Some(("repeat", json!({"on": false}), "Повтор выключен."));
    }
    if starts(&[
        "следующ",
        "включи следующ",
        "поставь следующ",
        "другую песню",
        "другой трек",
        "переключи",
        "дальше",
        "включи похож",
        "поставь похож",
    ]) {
        return Some(("related", json!({"expected":""}), ""));
    }
    None
}

/// Новая позиция Spotify в мс: абсолютная или относительная, в пределах трека.
fn seek_target_ms(pos_ms: u64, dur_ms: u64, delta: f64, abs: bool) -> u64 {
    let base = if abs { 0.0 } else { pos_ms as f64 };
    let mut ms = (base + delta * 1000.0).max(0.0) as u64;
    if dur_ms > 0 { ms = ms.min(dur_ms.saturating_sub(500)); }
    ms
}

fn seek(t: &str) -> (f64, bool) {
    for word in t.split_whitespace() {
        if let Some((m, s)) = word.split_once(':') {
            if let (Ok(m), Ok(s)) = (m.parse::<u32>(), s.parse::<u32>()) {
                if s < 60 {
                    return ((m as f64) * 60.0 + s as f64, true);
                }
            }
        }
    }
    let words: Vec<_> = t.split_whitespace().collect();
    let mut total = 0.0;
    for (i, w) in words.iter().enumerate() {
        let n = match *w {
            "полминуты" => {
                total += 30.0;
                continue;
            }
            "минуту" if i == 0 || amount(words[i - 1]).is_none() => {
                total += 60.0;
                continue;
            }
            _ => amount(w),
        };
        if let Some(n) = n {
            let unit = words.get(i + 1).copied().unwrap_or("");
            total += n * if unit.starts_with("мин")
                || *w == "полторы"
                || (*w == "пару" && unit.is_empty())
            {
                60.0
            } else {
                1.0
            };
        }
    }
    if total == 0.0 {
        total = 30.0;
    }
    if t.contains("назад") || t.contains("отмотай") {
        total = -total;
    }
    (total, t.contains("на отметку") || t.contains("на позицию"))
}
fn amount(w: &str) -> Option<f64> {
    Some(match w {
        "одну" | "один" => 1.0,
        "два" | "две" | "пару" => 2.0,
        "три" => 3.0,
        "четыре" => 4.0,
        "пять" => 5.0,
        "десять" => 10.0,
        "пятнадцать" => 15.0,
        "двадцать" => 20.0,
        "тридцать" => 30.0,
        "сорок" => 40.0,
        "пятьдесят" => 50.0,
        "полторы" => 1.5,
        _ => w.parse().ok()?,
    })
}

pub fn volume_loop(music: Arc<Music>, host: Arc<Host>, duck: impl Fn() -> bool + Send + 'static) {
    tokio::spawn(async move {
        let mut rx = music.playing_tx.subscribe();
        let mut last = -1.0;
        loop {
            while !*rx.borrow_and_update() {
                last = -1.0;
                if rx.changed().await.is_err() {
                    return;
                }
            }
            let v = if duck() { 0.16 } else { 0.8 };
            if v != last
                && let Ok(n) = host.provider("youtube")
                && n.notify("volume", json!({"v": v})).await.is_ok()
            {
                last = v;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
}

pub fn played_music_title(tool_name: &str, result: &str) -> Option<String> {
    match tool_name {
        "play_music" => result
            .strip_prefix("Включил с YouTube:")
            .map(|s| s.trim().trim_end_matches('.').to_string()),
        "music_control" => result
            .strip_prefix("Включил «")
            .and_then(|s| s.strip_suffix("».").or_else(|| s.strip_suffix('»')))
            .map(|s| s.trim().to_string()),
        _ => None,
    }
}

pub fn wants_keep_talking(text: &str) -> bool {
    let lower = text.to_lowercase().replace('ё', "е");
    lower.contains('?')
        || [
            "не отключай",
            "не выключай",
            "не закрывай",
            "останься",
            "поболта",
            "поговори",
            "расскажи",
            "объясни",
            "подскажи",
            "какая погода",
            "сколько врем",
            "что такое",
            "кто такой",
            "кто такая",
            "почему ",
            "зачем ",
            "как сдела",
        ]
        .iter()
        .any(|p| lower.contains(p))
}

pub fn should_auto_close_after_music(
    played_title: Option<&str>,
    other_tools: bool,
    keep_talking: bool,
    answer: &str,
) -> bool {
    let Some(title) = played_title else {
        return false;
    };
    !other_tools
        && !keep_talking
        && !answer.contains('?')
        && answer.chars().count() <= title.chars().count() + 100
}

#[derive(Default)]
pub struct MusicTurnTracker {
    played_title: Option<String>,
    other_tools: bool,
    keep_talking: bool,
}

impl MusicTurnTracker {
    pub fn note_other(&mut self) {
        self.other_tools = true;
    }

    pub fn observe_tool(&mut self, name: &str, args: &str, result: &str) {
        if name == "play_music" || name == "music_control" {
            if let Ok(v) = serde_json::from_str::<Value>(args)
                && v.get("keep_talking").and_then(Value::as_bool) == Some(true)
            {
                self.keep_talking = true;
            }
            if let Some(title) = played_music_title(name, result) {
                if !self.other_tools {
                    self.played_title = Some(title);
                }
            } else {
                self.other_tools = true;
            }
        } else {
            self.other_tools = true;
        }
    }

    pub fn should_close(&self, answer: &str) -> bool {
        should_auto_close_after_music(
            self.played_title.as_deref(),
            self.other_tools,
            self.keep_talking,
            answer,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_controls_without_catching_questions() {
        assert!(command("расскажи как перемотать фильм").is_none());
        assert_eq!(
            command("Джарвис, перемотай назад на 2 минуты 15 секунд")
                .unwrap()
                .1,
            json!({"delta":-135.0,"abs":false})
        );
        assert_eq!(
            command("перемотай на 2:05").unwrap().1,
            json!({"delta":125.0,"abs":true})
        );
        assert_eq!(command("перемотай на полминуты").unwrap().1["delta"], 30.0);
        assert_eq!(command("пауза").unwrap().0, "pause");
    }
    #[test]
    fn resets_state_on_exit() {
        let m = Music::default();
        m.event("music.state", json!({"active":true,"playing":true}));
        assert!(m.state().playing);
        m.event("exit", Value::Null);
        assert!(!m.state().active);
    }

    #[test]
    fn foreground_player_owns_controls() {
        let m = Music::default();
        m.alice.lock().unwrap().main_log(r#"event=track_start, payload={"id":"42"}"#);
        assert!(m.use_alice());
        m.event("music.state", json!({"playing":true,"active":true}));
        assert!(!m.use_alice());
        m.event("music.state", json!({"paused":true,"active":true}));
        assert!(m.use_alice());
        m.alice.lock().unwrap().media_log("[audio stream=42] pause (position 2000)");
        assert!(!m.use_alice());
    }

    #[test]
    fn repeat_voice_commands_toggle_on_and_off() {
        assert_eq!(command("поставь на повтор").unwrap().1, json!({"on": true}));
        assert_eq!(command("на репит").unwrap().1, json!({"on": true}));
        assert_eq!(command("включи повтор").unwrap().0, "repeat");
        assert_eq!(command("выключи повтор").unwrap().1, json!({"on": false}));
        assert_eq!(command("не повторяй").unwrap().1, json!({"on": false}));
        assert!(command("повтори что ты сказал").is_none());
    }

    #[test]
    fn auto_close_after_music_only_triggers_for_single_intent_play() {
        assert_eq!(
            played_music_title("play_music", "Включил с YouTube: Сало.").as_deref(),
            Some("Сало")
        );
        assert_eq!(
            played_music_title("music_control", "Включил «Сало».").as_deref(),
            Some("Сало")
        );
        assert!(played_music_title("play_music", "В очереди: Сало.").is_none());
        assert!(
            played_music_title(
                "play_music",
                "Нашёл «Сало». Послушаем? Ролик ещё не включён."
            )
            .is_none()
        );
        assert!(played_music_title("play_music", "Не получилось: ошибка").is_none());
        assert!(played_music_title("music_control", "Пауза.").is_none());

        assert!(wants_keep_talking("Включи Сало и расскажи новости"));
        assert!(wants_keep_talking("Включи музыку?"));
        assert!(!wants_keep_talking("Включи на ютубе Сало"));

        let mut tr = MusicTurnTracker::default();
        tr.observe_tool("play_music", r#"{"query":"Сало"}"#, "Включил с YouTube: Сало.");
        assert!(tr.should_close("Включил трек «Сало» с YouTube."));
        assert!(!tr.should_close("Включил трек «Сало». Что ещё сделать?"));

        let mut tr_keep = MusicTurnTracker::default();
        tr_keep.observe_tool(
            "play_music",
            r#"{"query":"Сало","keep_talking":true}"#,
            "Включил с YouTube: Сало.",
        );
        assert!(!tr_keep.should_close("Включил Сало."));

        let mut tr_multi = MusicTurnTracker::default();
        tr_multi.observe_tool("play_music", r#"{"query":"Сало"}"#, "Включил с YouTube: Сало.");
        tr_multi.observe_tool("smart_home", r#"{"on":false}"#, "Готово.");
        assert!(!tr_multi.should_close("Включил Сало и выключил свет."));
    }

    fn sp(status: &str) -> SpotState { SpotState { status: status.into(), uri: "spotify:track:X".into(), ..Default::default() } }

    #[test] fn spotify_playing_wins() { let sp = SpotState{status:"playing".into(), ..Default::default()}; assert!(matches!(active_source(&Remote::default(), false, false, &sp, None), Source::Spotify)); }
    #[test] fn youtube_playing_beats_paused_spotify() { let yt = Remote{playing:true, active:true, ..Default::default()}; let sp = SpotState{status:"paused".into(), ..Default::default()}; assert!(matches!(active_source(&yt, false, false, &sp, Some(Instant::now())), Source::Youtube)); }
    #[test] fn stale_spotify_pause_is_ignored() { let sp = SpotState{status:"paused".into(), ..Default::default()}; let old = Instant::now() - Duration::from_secs(31*60); assert!(matches!(active_source(&Remote::default(), false, false, &sp, Some(old)), Source::Youtube)); }
    #[test] fn like_phrases_parse() { assert_eq!(command("лайк").unwrap().0, "like"); assert_eq!(command("убери из любимых").unwrap().0, "unlike"); assert!(command("что такое лайк в тиктоке").is_none()); }
    #[test] fn spotify_snapshot_shape() { let s = SpotState{status:"playing".into(), uri:"spotify:track:X".into(), title:"T".into(), artists:"A".into(), pos_ms:1500, dur_ms:3000, ..Default::default()}; let v = Music::spotify_snapshot(&s); assert_eq!(v["source"],"spotify"); assert_eq!(v["pos"],1.5); assert_eq!(v["link"],"https://open.spotify.com/track/X"); }

    #[test]
    fn fresh_spotify_pause_owns_controls_until_youtube_plays() {
        let m = Music::default();
        assert!(m.set_spotify(Some(sp("playing"))));
        assert!(m.playing());
        assert_eq!(m.source(), Source::Spotify);
        assert!(!m.set_spotify(Some(sp("paused"))));
        assert!(!m.playing());
        assert_eq!(m.source(), Source::Spotify);
        m.event("music.state", json!({"playing":true,"active":true}));
        assert_eq!(m.source(), Source::Youtube);
        m.event("music.state", json!({"paused":true,"active":true}));
        assert_eq!(m.source(), Source::Youtube);
    }

    #[test]
    fn spotify_start_is_reported_once_and_failures_clear_state() {
        let m = Music::default();
        assert!(m.set_spotify(Some(sp("playing"))));
        assert!(!m.set_spotify(Some(sp("playing"))));
        assert!(!m.set_spotify(None));
        assert!(!m.set_spotify(None));
        assert_eq!(m.source(), Source::Spotify);
        m.set_spotify(None);
        assert_eq!(m.source(), Source::Youtube);
        assert!(!m.playing());
    }

    #[test]
    fn spotify_commands_and_seek_math() {
        assert_eq!(command("перемешай").unwrap().1, json!({"on": true}));
        assert_eq!(command("не перемешивай").unwrap().1, json!({"on": false}));
        assert_eq!(command("по порядку").unwrap().0, "shuffle");
        assert_eq!(command("не нравится").unwrap().0, "unlike");
        assert_eq!(command("добавь в любимые").unwrap().0, "like");
        assert_eq!(seek_target_ms(10_000, 200_000, -30.0, false), 0);
        assert_eq!(seek_target_ms(10_000, 200_000, 20.0, false), 30_000);
        assert_eq!(seek_target_ms(10_000, 200_000, 125.0, true), 125_000);
        assert_eq!(seek_target_ms(10_000, 200_000, 999.0, true), 199_500);
    }
}

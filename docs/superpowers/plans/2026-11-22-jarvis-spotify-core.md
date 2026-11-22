# Джарвис + Spotify в ядре — план работ

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Spotify (через Soloist на колонке) становится третьим музыкальным источником ядра YaJarvis: умный поиск Spotify → YouTube, карточка в панели, пауза при разговоре; Алиса и yapanel больше не участвуют.

**Architecture:** Локальный форк YaJarvis `v0.10.16`, ветка `spotify`. Новый модуль ядра `core/src/spotify.rs` (конфиг, разбор состояния Soloist, поиск через Spotify Web API, вызовы скриптов `control.sh`) встраивается в `music.rs` рядом с `AliceMusic`. Панель (Svelte) учит источник `spotify`. Сборка — `build.sh` под `aarch64-unknown-linux-musl` (на колонке ядро arm64: ELF class 2, machine 0xb7).

**Tech Stack:** Rust 2024 (tokio, axum, reqwest 0.12, serde_json — уже в зависимостях `core`), Svelte 5 + Vite, zig как C-компилятор, sh-скрипты Soloist.

**Spec:** `docs/superpowers/plans/2026-11-22-jarvis-spotify-core-design.md`

## Global Constraints

- Форк: `~/Desktop/Yandex Alice/yajarvis`, от тега `v0.10.16`, ветка `spotify`. Коммиты только туда; мат и секреты в коммиты не попадают.
- Версия сборки: `JARVIS_VERSION=0.10.16+spotify`.
- Только `STATION_ARCHES=arm64` (`aarch64-unknown-linux-musl`).
- Колонка: `ssh -p 2222 -i <ssh-key> root@<speaker-ip>`. Ядро: `/data/opt/jarvisd/jarvisd`, перезапуск `/data/opt/jarvisd/jarvisd.sh stop && /data/opt/jarvisd/jarvisd.sh start`, журнал `/tmp/jarvisd/jarvisd.log`.
- Spotify-управление — только через `sh /data/opt/soloist/scripts/control.sh <cmd> [arg]` (команды: `play [uri]`, `pause`, `next`, `prev`, `toggle`, `shuffle on|off`, `repeat …`, `like`, `unlike`, `search "<фраза>"`), запуск с `cwd=/`. Состояние — `LD_LIBRARY_PATH=/data/opt/soloist/lib64 /data/opt/soloist/bin/soloist ctl -w 127.0.0.1:9090 now --json`.
- Ключи: `/data/opt/soloist/spotify_api.conf`, строки `KEY=value` (ключи `SPOTIFY_CLIENT_ID`, `SPOTIFY_CLIENT_SECRET`, `SPOTIFY_REFRESH_TOKEN`, `SPOTIFY_LIKED_URI`; значения могут быть в кавычках). Никогда не логировать и не печатать значения.
- Поиск Spotify: `POST https://accounts.spotify.com/api/token` (`grant_type=client_credentials`, Basic auth), `GET https://api.spotify.com/v1/search?q=…&type=…&limit=5` (без `market=from_token`: с client credentials он не работает).
- Тексты ответов Джарвиса — по-русски, формат: «Включил в Spotify: <Артист> — <Название>.», «В Spotify не нашёл, включил с YouTube: <название>.»
- Таймаут вызова скрипта: 15 с (`search`), 8 с (остальные); `now --json` — 4 с.
- Ожидание паузы Spotify перед первым звуком ответа — не дольше 1500 мс.
- `soloist-bridge` не трогаем.

## Review Focus

1. **Soloist не запущен / `now --json` падает или висит** — ядро считает Spotify неактивным, Джарвис и YouTube работают как раньше, панель не зависает. Тест: `spotify::parse_now("")` и мусор → `None`; `poll` с таймаутом (Task 3).
2. **Spotify включён с телефона, пока играет YouTube** — YouTube останавливается, карточка переключается на Spotify. Тест: `active_source` + `on_spotify_started` (Task 3).
3. **«Пауза наша» против ручной паузы**: хозяин сам поставил паузу во время разговора или включил другое — после разговора Spotify не возобновляется. Тест: `TalkPause` (Task 5).
4. **Запрос без совпадения в Spotify** («обзор айфона», опечатки, латиница/кириллица) — уходит в YouTube, а не играет случайный трек. Тест: `pick` на фикстурах (Task 2).
5. **Ключи отсутствуют или refresh-токен протух** — поиск Spotify тихо падает в YouTube с пояснением, лайк отвечает «Не получилось: …». Тест: `SpotifyConf::parse` без ключей → поиск недоступен (Task 2), `play_smart` фолбэк (Task 4).

---

### Task 1: Форк, тулчейн, сборка и блокировка обновлений

**Files:**
- Create: `~/Desktop/Yandex Alice/yajarvis/` (git clone `https://github.com/newfpv/YaJarvis`, `git checkout -b spotify v0.10.16`)
- Create: `~/Desktop/Yandex Alice/build-jarvis.sh`
- Modify: `yajarvis/jarvis/core/src/web/update.rs` (`automatic`, `install`, `upload`)
- Modify: `yajarvis/jarvis/core/src/updates.rs` (добавить `pub fn locked`)

**Interfaces:**
- Produces: `updates::locked(version: &str) -> bool` — `true`, если версия содержит `+spotify`. `build-jarvis.sh build|deploy|rollback`.

- [ ] **Step 1:** `brew install rustup zig cmake ninja node`; `rustup-init -y`; `rustup target add aarch64-unknown-linux-musl`. Проверка: `zig version`, `cargo --version` печатают версии.
- [ ] **Step 2:** Клонировать форк, `npm ci` в корне. Baseline: `cd jarvis && cargo test --locked -p jarvis 2>&1 | tail -5`. Записать в `yajarvis/BASELINE.md` число прошедших и список падающих на macOS тестов (если есть) — дальше «зелёный» значит «не хуже baseline».
- [ ] **Step 3: Тест** в `updates.rs` (`#[cfg(test)]`):
```rust
#[test]
fn custom_build_is_locked() {
    assert!(locked("0.10.16+spotify"));
    assert!(!locked("0.10.16"));
    assert!(!locked("dev"));
}
```
- [ ] **Step 4:** Запустить — FAIL (`locked` не определена).
- [ ] **Step 5:** Реализовать `locked`. В `web/update.rs`: `automatic` сразу `return`, если `locked(crate::version())`; `install` и `upload` возвращают `bad("Сборка со Spotify: обновлять через build-jarvis.sh")`.
- [ ] **Step 6:** `cargo test --locked -p jarvis updates` — PASS.
- [ ] **Step 7:** `build-jarvis.sh`:
  - `build`: `cd yajarvis && STATION_ARCHES=arm64 VERSION=0.10.16+spotify JARVIS_VERSION=0.10.16+spotify sh build.sh`, затем находит собранный `jarvisd` для arm64 (в `$tools/arm64/jarvisd`, путь вывести из `build.sh`) и копирует в `yajarvis/out/jarvisd`.
  - `deploy`: если на колонке нет `/data/opt/jarvisd/jarvisd.orig` — `cp jarvisd jarvisd.orig`; `jarvisd.sh stop`; залить `out/jarvisd` через `scp -P 2222` во временный файл и `mv`; `jarvisd.sh start`; ждать до 30 с строку `jarvis 0.10.16+spotify` в журнале.
  - `rollback`: stop, `cp jarvisd.orig jarvisd`, start.
- [ ] **Step 8:** `./build-jarvis.sh build && ./build-jarvis.sh deploy`. Проверка: журнал содержит `jarvis 0.10.16+spotify`; «Джарвис, сколько времени?» — отвечает; `curl -s http://<speaker-ip>:8080/api/update` показывает `"version":"0.10.16+spotify"`.
- [ ] **Step 9:** Commit: `git add -A && git commit -m "build: custom +spotify build, lock self-update"`.

### Task 2: Модуль `spotify.rs` — конфиг, состояние, поиск, подбор

**Files:**
- Create: `yajarvis/jarvis/core/src/spotify.rs`
- Create: `yajarvis/jarvis/core/src/spotify/fixtures/` (`now_playing.json` — пример `now --json` ниже; `search_kino.json`, `search_obzor.json`, `search_podcast.json` — реальные ответы `/v1/search`, снять curl-ом с Мака с теми же ключами, без токенов в файлах)
- Modify: `yajarvis/jarvis/core/src/main.rs` (`mod spotify;`)

**Interfaces:**
- Produces:
  - `pub struct SpotifyConf { pub client_id: String, pub client_secret: String, pub refresh_token: String, pub liked_uri: String }`, `SpotifyConf::parse(text: &str) -> SpotifyConf`, `SpotifyConf::load() -> SpotifyConf` (читает `/data/opt/soloist/spotify_api.conf`, при ошибке — пустой), `fn can_search(&self) -> bool`.
  - `#[derive(Clone, Default, Serialize)] pub struct SpotState { pub status: String /* "playing"|"paused"|"" */, pub uri: String, pub title: String, pub artists: String, pub album: String, pub cover: String /* size "large" */, pub pos_ms: u64, pub dur_ms: u64, pub context: String, pub shuffle: bool }`, `pub fn parse_now(json: &str) -> Option<SpotState>`.
  - `pub enum Kind { Any, Track, Artist, Album, Playlist, Podcast }` и `pub struct Pick { pub uri: String, pub label: String /* "Артист — Название" или название шоу/плейлиста */ }`.
  - `pub fn pick(query: &str, kind: Kind, search: &serde_json::Value) -> Option<Pick>` — сверка.
  - `pub async fn search(conf: &SpotifyConf, query: &str, kind: Kind) -> Result<Option<Pick>, String>` (кеш client-credentials токена в `static Mutex<Option<(String, Instant)>>`).
  - `pub async fn now() -> Option<SpotState>`, `pub async fn ctl(args: &[&str]) -> Result<(), String>` (spawn `sh control.sh …` с `current_dir("/")`, таймауты из Global Constraints).

Пример `now --json` (сокращён) для фикстуры:
```json
{"type":"playback_state","status":"paused","item":{"uri":"spotify:track:5cEsFmTLxsQfKm9OOMws28","entity_type":"track","decorations":{"identity":{"name":"ONLY POSITIVE"},"visual_identity":{"cover":[{"url":"https://i.scdn.co/image/small","size":"small"},{"url":"https://i.scdn.co/image/large","size":"large"}]},"parent":{"entity":{"uri":"spotify:album:76ThthchnyANx9FJUhFkPX","entity_type":"album","decorations":{"identity":{"name":"CHAMPION SOUND"}}}},"creators":[{"entity":{"decorations":{"identity":{"name":"Endspiel"}}}},{"entity":{"decorations":{"identity":{"name":"TumaniYO"}}}}],"playback":{"duration_ms":178614}}},"context":{"uri":"spotify:playlist:37i9dQZF1F5p3rmiWPIYgZ"},"position":{"position_ms":62762,"timestamp_ms":1795371557263,"speed":0},"options":{"shuffle":true}}
```

**Правило сверки `pick`** (из спеки, решение «модель + сверка»):
- Нормализация: нижний регистр, `ё→е`, только буквы/цифры/пробелы; выкинуть стоп-слова `песню песня трек альбом группы группу исполнителя плейлист подкаст выпуск эпизод the`.
- Кандидаты: для `Kind::Any` — треки, артисты, альбомы, плейлисты; `Podcast` — `shows`, затем `episodes`; остальные — свой тип. Пропускать `null`-элементы (Spotify их возвращает).
- Счёт кандидата = доля слов запроса, найденных (как подстрока, ≥3 символов или точное совпадение коротких) в `name` + имена артистов/владельца/издателя.
- Принять первый кандидат по порядку Spotify со счётом ≥ 0.6. Для `Any` предпочитать артиста, если его имя совпало полностью (запрос «Кино» → артист Кино, а не трек).

- [ ] **Step 1: Тесты** в `spotify.rs`:
```rust
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
#[test] fn pick_podcast_show() { let v = fixture("search_podcast.json"); assert!(pick("<название реального подкаста из фикстуры>", Kind::Podcast, &v).unwrap().uri.starts_with("spotify:show:")); }
#[test] fn pick_skips_nulls() { let v = serde_json::json!({"tracks":{"items":[null]}}); assert!(pick("что угодно", Kind::Any, &v).is_none()); }
```
- [ ] **Step 2:** `cargo test -p jarvis spotify` — FAIL (модуля нет).
- [ ] **Step 3:** Реализовать всё из Interfaces. `now()`/`ctl()` через `tokio::process::Command` + `tokio::time::timeout`; при таймауте процесс `kill`.
- [ ] **Step 4:** `cargo test -p jarvis spotify` — PASS; полный `cargo test -p jarvis` — не хуже baseline.
- [ ] **Step 5:** Commit `feat(spotify): config, Soloist state, search and match`.

### Task 3: Spotify как источник в музыкальном движке

**Files:**
- Modify: `yajarvis/jarvis/core/src/music.rs`
- Modify: `yajarvis/jarvis/core/src/main.rs` (запуск опроса)

**Interfaces:**
- Consumes: `spotify::{now, ctl, SpotState, SpotifyConf}`.
- Produces:
  - `pub enum Source { Youtube, Alice, Spotify }` и `pub fn active_source(yt: &Remote, alice_active: bool, alice_playing: bool, sp: &SpotState, sp_paused_at: Option<Instant>) -> Source`: играет Spotify → `Spotify`; играет YouTube → `Youtube`; играет Алиса → `Alice`; иначе последний на паузе не старше 30 мин (Spotify — по `sp_paused_at`) → он; иначе `Youtube`.
  - В `Music`: поле `pub spotify: Mutex<SpotTrack>` (`SpotTrack { state: SpotState, paused_at: Option<Instant>, liked_flash: … }` — по усмотрению), `pub fn spotify_snapshot(st: &SpotState) -> Value`, `pub async fn spotify_poll_loop(self: Arc<Self>, host: Arc<Host>)` (1 с, пока Spotify играл в последние 30 мин, иначе 5 с).
  - `Music::playing()` учитывает Spotify; `transport()`/`control()`/`snapshot()` маршрутизируют по `active_source`.
  - Snapshot Spotify: `{"source":"spotify","playing","paused","track":{"ID":uri,"Title","Author"},"pos","dur","cover","album","link":"https://open.spotify.com/<type>/<id>","shuffle"}` (`pos`/`dur` в секундах, как у `AliceMusic`).
  - Команды `control()` для Spotify: `pause→pause`, `resume→play`, `next→next`, `prev→prev`, `stop→pause`, `status→«Играет/На паузе «Название» — Артист.»`, `related→next`, `seek` → `soloist ctl -w 127.0.0.1:9090 seek <ms>` напрямую (Soloist объявляет `available_actions.seek`; точный синтаксис сначала проверить `soloist ctl --help` на колонке и записать в `BASELINE.md`; если команды нет — ответ «Перемотка в Spotify не поддерживается.» и кнопка перемотки в панели для Spotify скрыта), плюс новые фразы: «лайк|нравится|добавь в любимые» → `ctl(["like"])`, «дизлайк|не нравится|убери из любимых» → `ctl(["unlike"])`, «перемешай|вперемешку» → `shuffle on`, «по порядку|не перемешивай» → `shuffle off`. Распознавание — расширить `command()` (не ломая существующие тесты).
  - Эксклюзивность: `on_spotify_started` — при переходе статуса Spotify в `playing` (опрос) вызвать `self.call(host,"stop",Null)`, если YouTube играет.

- [ ] **Step 1: Тесты** (`music.rs` tests):
```rust
#[test] fn spotify_playing_wins() { let sp = SpotState{status:"playing".into(), ..Default::default()}; assert!(matches!(active_source(&Remote::default(), false, false, &sp, None), Source::Spotify)); }
#[test] fn youtube_playing_beats_paused_spotify() { let yt = Remote{playing:true, active:true, ..Default::default()}; let sp = SpotState{status:"paused".into(), ..Default::default()}; assert!(matches!(active_source(&yt, false, false, &sp, Some(Instant::now())), Source::Youtube)); }
#[test] fn stale_spotify_pause_is_ignored() { let sp = SpotState{status:"paused".into(), ..Default::default()}; let old = Instant::now() - Duration::from_secs(31*60); assert!(matches!(active_source(&Remote::default(), false, false, &sp, Some(old)), Source::Youtube)); }
#[test] fn like_phrases_parse() { assert_eq!(command("лайк").unwrap().0, "like"); assert_eq!(command("убери из любимых").unwrap().0, "unlike"); assert!(command("что такое лайк в тиктоке").is_none()); }
#[test] fn spotify_snapshot_shape() { let s = SpotState{status:"playing".into(), uri:"spotify:track:X".into(), title:"T".into(), artists:"A".into(), pos_ms:1500, dur_ms:3000, ..Default::default()}; let v = Music::spotify_snapshot(&s); assert_eq!(v["source"],"spotify"); assert_eq!(v["pos"],1.5); assert_eq!(v["link"],"https://open.spotify.com/track/X"); }
```
- [ ] **Step 2:** Тесты — FAIL.
- [ ] **Step 3:** Реализовать. Запуск `spotify_poll_loop` в `main.rs` рядом с `volume_loop`. `/api/music` (`web/music.rs`) менять не нужно — он уже идёт через `transport`/`snapshot`; добавить действия `like`/`unlike`/`shuffle` в `post` (`"like" | "unlike"` → `transport(op)`, `"shuffle"` с `on: bool`).
- [ ] **Step 4:** Тесты — PASS, полный набор не хуже baseline.
- [ ] **Step 5:** Сборка и деплой (`build-jarvis.sh build && deploy`). Проверка: включить Spotify с телефона → `curl -s http://<speaker-ip>:8080/api/music` отдаёт `"source":"spotify"` с треком; `curl -s -X POST -H 'Content-Type: application/json' -d '{"action":"pause"}' …/api/music` ставит на паузу. Если API требует авторизацию панели — проверять через панель в браузере.
- [ ] **Step 6:** Commit `feat(music): Spotify source in engine`.

### Task 4: Умный `play_music` и запрет музыки для Алисы

**Files:**
- Modify: `yajarvis/jarvis/core/src/tools.rs` (схема и вызов `play_music`, `alice_command`)
- Modify: `yajarvis/jarvis/core/src/music.rs` (`play_smart`)
- Modify: `yajarvis/jarvis/core/src/main.rs` (замыкания `tools.music`/`music_queue` передают JSON-аргументы)
- Modify: `yajarvis/jarvis/core/src/fastcmd.rs` (`alice_skill_for`)

**Interfaces:**
- Consumes: `spotify::{search, pick, ctl, SpotifyConf, Kind}`, `Music::play` (YouTube).
- Produces:
  - Схема `play_music`: `query` (string), `source` (`"auto"|"spotify"|"youtube"`, default `auto`), `kind` (`"any"|"track"|"artist"|"album"|"playlist"|"podcast"|"liked"|"liked_shuffle"|"wave"`, default `any`), `queue`, `keep_talking`. Описание: «Включить музыку, подкаст или видео на колонке. По умолчанию ищет в Spotify, при неудаче — на YouTube. source=youtube — для видео: обзоры, ролики, концерты, каверы, «на ютубе». kind=liked/liked_shuffle/wave — «моё любимое», «любимое вперемешку», «рекомендации/моя волна».»
  - `pub async fn play_smart(&self, host: &Host, query: &str, source: &str, kind: &str, queue: bool) -> String`:
    - `liked` → `ctl(["search","моё любимое"])`; `liked_shuffle` → `ctl(["search","любимое вперемешку"])`; `wave` → `ctl(["search","рекомендации"])`; ответы «Включил любимое в Spotify.» / «…вперемешку.» / «Включил рекомендации в Spotify.»
    - `source=youtube` или `queue=true` → прежний `self.play(host, query, queue)` (очередь есть только у YouTube) + перед этим `ctl(["pause"])`, если Spotify играет.
    - иначе `spotify::search` → `Some(pick)` → остановить YouTube (`call stop`, если играет) → `ctl(["play", &pick.uri])` → «Включил в Spotify: {label}.»; `None`/`Err` → `self.play(...)` и ответ с префиксом «В Spotify не нашёл, » + прежний ответ с маленькой буквы.
  - `alice_skill_for(text, yandex_music)`: при `!yandex_music` — никогда не возвращать музыкальные/подкастовые «включи …» (уже фильтрует `alice_music_request`; добавить подкасты и «включи <что угодно>» без слов умного дома). В `tools.rs` `alice_command`: если `fastcmd::alice_music_request(&cmd)` — ответ «Для музыки вызови play_music.».

- [ ] **Step 1: Тесты:**
```rust
// tools.rs / music.rs
#[test] fn play_music_schema_has_source_and_kind() { /* defs() из Tools с фейковым provider: найти play_music, проверить enum source == ["auto","spotify","youtube"] и наличие "liked_shuffle" в kind */ }
// fastcmd.rs
#[test] fn music_never_goes_to_alice_without_yandex_music() {
    for t in ["включи кино", "включи подкаст про космос", "поставь мою волну", "включи что-нибудь спокойное"] {
        assert!(alice_skill_for(t, false).is_none(), "{t}");
    }
    assert!(alice_skill_for("поставь таймер на 5 минут", false).is_some());
    assert!(alice_skill_for("сделай громче", false).is_some());
}
```
`play_smart` маршрутизацию вынести в чистую `fn plan(source:&str, kind:&str, queue:bool, found: Option<&Pick>) -> PlayPlan` и покрыть: `liked` → Spotify без поиска; `youtube` → YouTube; `auto` + `None` → YouTube с префиксом; `queue` → YouTube.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Реализовать; `tools.call` читает `source`/`kind` из `a`. Сигнатуры замыканий `MusicFn` поменять на приём `serde_json::Value` аргументов — обновить все места создания (`main.rs`) и типы в `tools.rs`.
- [ ] **Step 4:** PASS, полный набор не хуже baseline.
- [ ] **Step 5:** Деплой; в панели Джарвиса выключить канал «Яндекс Музыка» (`yandexmusic`). Голосом: «включи Кино» → Spotify; «включи на ютубе обзор айфона» → YouTube, Spotify на паузе; «включи обзор айфона 17» → YouTube с «В Spotify не нашёл»; «моё любимое», «любимое вперемешку», «рекомендации» → Spotify, первый трек не пропущен.
- [ ] **Step 6:** Commit `feat(tools): smart play_music Spotify→YouTube`.

### Task 5: Пауза Spotify на время разговора

**Files:**
- Modify: `yajarvis/jarvis/core/src/music.rs` (`TalkPause`)
- Modify: `yajarvis/jarvis/core/src/engine.rs` (`trigger`, конец разговора в `ConverseGuard`/после `converse`, перед первым `player` звуком)
- Modify (колонка/пакет): `soloist-station-lite/install.sh`, `scripts/stop.sh`, `/data/opt/soloist/start.sh` — убрать `jarvis-watch.sh`; удалить `scripts/jarvis-watch.sh`.

**Interfaces:**
- Produces:
  - `#[derive(Default)] pub struct TalkPause { ours: Option<String /* uri в момент паузы */> }` с чистыми методами `fn on_wake(&mut self, sp: &SpotState) -> bool /* true = надо слать pause */` и `fn on_end(&mut self, sp: &SpotState) -> bool /* true = надо слать play */`: `on_end` → `true` только если `ours == Some(sp.uri)` и `sp.status == "paused"`; всегда сбрасывает `ours`.
  - `Music::talk_started(&self)` (неблокирующий: spawn pause, сохраняет `tokio::sync::Notify`/watch «пауза подтверждена»), `Music::wait_quiet(&self, max: Duration)` (ждёт подтверждения, что `now` = paused, не дольше `max`), `Music::talk_ended(&self)`.
  - Вызовы: `talk_started` — в `Engine::trigger` рядом с `log::info!("Джарвис: активация …")`; `wait_quiet(Duration::from_millis(1500))` — перед первым воспроизведением ответа/отклика (найти первый вызов `self.player`/`earcon` в `converse`); `talk_ended` — там, где сейчас разговор завершается (после `e.converse(...)` в потоке `converse` и в `ConverseGuard::drop`).

- [ ] **Step 1: Тесты:**
```rust
#[test] fn resumes_only_our_pause() {
    let mut t = TalkPause::default();
    let playing = SpotState{status:"playing".into(), uri:"u1".into(), ..Default::default()};
    assert!(t.on_wake(&playing));
    let paused = SpotState{status:"paused".into(), uri:"u1".into(), ..Default::default()};
    assert!(t.on_end(&paused));
}
#[test] fn no_resume_if_something_else_started() {
    let mut t = TalkPause::default();
    t.on_wake(&SpotState{status:"playing".into(), uri:"u1".into(), ..Default::default()});
    assert!(!t.on_end(&SpotState{status:"playing".into(), uri:"u2".into(), ..Default::default()}));
}
#[test] fn no_pause_when_not_playing() { let mut t = TalkPause::default(); assert!(!t.on_wake(&SpotState{status:"paused".into(), ..Default::default()})); assert!(!t.on_end(&SpotState{status:"paused".into(), ..Default::default()})); }
#[test] fn second_wake_keeps_first_pause() { let mut t = TalkPause::default(); t.on_wake(&SpotState{status:"playing".into(), uri:"u1".into(), ..Default::default()}); assert!(!t.on_wake(&SpotState{status:"paused".into(), uri:"u1".into(), ..Default::default()})); assert!(t.on_end(&SpotState{status:"paused".into(), uri:"u1".into(), ..Default::default()})); }
```
- [ ] **Step 2:** FAIL. **Step 3:** реализовать. **Step 4:** PASS, полный набор не хуже baseline.
- [ ] **Step 5:** Убрать `jarvis-watch.sh` из пакета и с колонки (`killall jarvis-watch.sh`, удалить строку запуска из `/data/opt/soloist/start.sh`, файл из `/data/opt/soloist/scripts/`). Деплой ядра.
- [ ] **Step 6:** Голосом при играющем Spotify: «Джарвис, какая погода?» → музыка встаёт, ответ слышен, музыка продолжается. «Джарвис, включи на ютубе …» → после разговора Spotify не возобновляется. Время от «Джарвис» до отклика — на слух не хуже, чем было.
- [ ] **Step 7:** Commit (форк) `feat(engine): pause Spotify while talking`; изменения пакета soloist — в `soloist-station-lite` (не git — просто сохранить файлы).

### Task 6: Карточка Spotify в панели

**Files:**
- Modify: `yajarvis/jarvisd/ui/src/state/player.js` (строки ~84 и ~254)
- Modify: `yajarvis/jarvisd/ui/src/routes/components/HomePlayer/HomePlayer.svelte`
- Modify: `yajarvis/jarvisd/ui/src/routes/player/components/NowPlaying/NowPlaying.svelte`

**Interfaces:**
- Consumes: snapshot `source:"spotify"` (Task 3), `POST /api/music` с `action` `like`/`unlike`/`shuffle` (Task 3).
- Produces: в UI заменить бинарный `yandex` на `src = state?.source` с подписями `{yandex:"Яндекс Музыка", spotify:"Spotify", youtube:"YouTube"}`; «внешний» источник (`yandex` или `spotify`) — обложка из `state.cover`, `prev`/`next` всегда доступны, повтор/радио/очередь скрыты; чип «через Спотифай» вместо «через Джарвиса»; ссылка «Открыть в Spotify»; кнопка лайка (иконка сердца из существующего набора `icon()`, если есть, иначе «♥») только для Spotify → `music({action:"like"})` и тост «Добавил в любимые»; текст пустой карточки: «Скажите «Джарвис, включи …»».

- [ ] **Step 1:** Правки. **Step 2:** `npm run build:panel` — без ошибок.
- [ ] **Step 3:** Сборка ядра + деплой. В браузере `http://<speaker-ip>:8080`: при Spotify — обложка, название, артист, прогресс, бейдж «Spotify», пауза/вперёд/назад/лайк работают; при YouTube — карточка как была. Проверить на узком экране (телефон).
- [ ] **Step 4:** Commit `feat(panel): Spotify in player card`.

### Task 7: Финальная настройка и уборка

**Files:**
- Колонка: `/data/opt/jarvisd/settings.json` (через панель: «Яндекс Музыка» выкл., автообновление выкл.)
- Mac: `~/Desktop/Yandex Alice/yapanel-web/`, `patches/`, `start-panel.sh`, `launcher/`, `yapanel.log`, `~/Desktop/Моя колонка.app`
- Create: `~/Desktop/Джарвис.webloc` (URL `http://<speaker-ip>:8080`)
- Memory/vault: `yandex-mini2-setup.md` (memory), заметка в the owner's notes

- [ ] **Step 1:** Остановить сервер панели (`start-panel.sh stop`), выйти из «Моя колонка.app». **Перед удалением спросить хозяина**, перечислив пути; удалять в Корзину (`mv … ~/.Trash/`), не `rm -rf`.
- [ ] **Step 2:** Создать `.webloc`, проверить, что открывает панель.
- [ ] **Step 3:** Попросить хозяина сменить фразу активации Алисы на «Яндекс» в «Доме с Алисой».
- [ ] **Step 4:** Приёмка — все 11 пунктов раздела «Проверка на колонке» спеки, плюс перезагрузка колонки (`reboot`) и повтор пунктов 1, 5, 9.
- [ ] **Step 5:** Обновить memory-файл и записать заметку в vault (`DD-MM-YY-HHMMSS-джарвис-spotify-ядро.md`, frontmatter `created`, `source: claude`, `tags: []`): форк, `build-jarvis.sh`, `jarvisd.orig`, как обновлять на новый релиз (rebase ветки `spotify` на новый тег, build, deploy).

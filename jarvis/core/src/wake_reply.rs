use super::{Engine, INTERRUPTED, Mode, Profile, Settings, voice_preview};
use std::{
    fs,
    path::Path,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

const PHRASES: &[&str] = &[
    "М?",
    "Слушаю.",
    "Тут.",
    "Ну?",
    "Да?",
    "Я тут.",
    "Угу.",
    "Ага.",
    "Говори.",
    "На связи.",
    "Да, слушаю.",
    "Хм?",
];
pub(super) const FOLLOW_ON_GAP: Duration = Duration::from_millis(70);
const MAX_REPLY: Duration = Duration::from_millis(900);
const CACHE_BUDGET: u64 = 2 << 20;

fn identity(profile: &Profile) -> (&str, &str, &str, &str) {
    let engine = if profile.is_classic() {
        profile.effective_tts_engine()
    } else {
        profile.engine.as_str()
    };
    let voice = if (profile.is_classic() || profile.engine == "eleven_realtime")
        && !profile.tts_voice.is_empty()
    {
        &profile.tts_voice
    } else {
        &profile.voice
    };
    (
        engine,
        voice,
        &profile.live_model,
        &profile.tts_model,
    )
}

fn reply_name(profile: &Profile, phrase: &str) -> String {
    let mut voice = profile.clone();
    voice.voice = identity(profile).1.into();
    voice_preview::sample_name(&voice, &format!("wake-reply/v1/{phrase}"))
}

impl Engine {
    pub(super) fn start_wake_reply_cache(self: &Arc<Self>) {
        let engine = Arc::downgrade(self);
        self.rt.spawn(async move {
            tokio::time::sleep(Duration::from_secs(12)).await;
            let mut attempt = 0;
            while let Some(engine) = engine.upgrade() {
                let _ = tokio::task::spawn_blocking(move || engine.prepare_wake_replies(attempt))
                    .await;
                attempt = attempt.wrapping_add(1);
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    fn prepare_wake_replies(&self, attempt: usize) {
        let settings = self.store.get();
        let profile = settings.active_profile();
        if !settings.wake_enabled
            || !settings.wake_reply_enabled
            || self.talking()
            || self.player.playing()
            || !crate::engine::tts_missing(&self.host, &settings, &profile).is_empty()
        {
            return;
        }
        let directory = self.data.join("wake-replies");
        let mut pending: Vec<_> = PHRASES
            .iter()
            .filter_map(|&phrase| {
                let path = directory.join(reply_name(&profile, phrase));
                (!path.is_file()).then_some((phrase, path))
            })
            .collect();
        if pending.is_empty() {
            return;
        }
        // A timed-out session is closed, so a late reply can't be saved under another phrase.
        let start = attempt % pending.len();
        pending.rotate_left(start);
        let cancelled = || {
            self.talking()
                || self.player.playing()
                || !self
                    .store
                    .with(|s| s.wake_enabled && s.wake_reply_enabled && identity(&s.active_profile()) == identity(&profile))
        };

        match self.record_wake_replies(settings, &profile, &directory, &pending, &cancelled) {
            Ok(recorded) if recorded > 0 => {
                log::info!("Отклики на активацию: добавлено {recorded} в кеш")
            }
            Err(error) if error != INTERRUPTED => log::info!("Отклики на активацию: {error}"),
            _ => {}
        }
    }

    fn record_wake_replies(
        &self,
        settings: Settings,
        profile: &Profile,
        directory: &Path,
        pending: &[(&str, std::path::PathBuf)],
        cancelled: &impl Fn() -> bool,
    ) -> Result<usize, String> {
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        if cancelled() {
            return Err(INTERRUPTED.into());
        }
        let session = self.preview_session(settings, profile.clone());
        let result = (|| {
            let ready = session.ready(Duration::from_secs(10));
            if cancelled() {
                return Err(INTERRUPTED.into());
            }
            ready?;
            let output = session
                .take_audio()
                .ok_or("Голосовой движок не открыл аудио")?;
            let mut recorded = 0;
            for (phrase, path) in pending {
                let sample = voice_preview::capture_phrase(
                    &*session,
                    &output,
                    phrase,
                    Duration::from_secs(10),
                    cancelled,
                    false,
                )?;
                let bytes = if short_enough(&sample) {
                    recorded += 1;
                    sample.encode()
                } else {
                    // Avoid repeatedly asking the provider to generate an overlong reply.
                    Vec::new()
                };
                crate::settings::write_atomic(path, &bytes, 0o600)
                    .map_err(|error| error.to_string())?;
                voice_preview::prune(directory, path, CACHE_BUDGET);
            }
            Ok(recorded)
        })();
        session.close();
        result
    }

    pub(super) fn cached_wake_reply(&self, profile: &Profile) -> Option<voice_preview::Sample> {
        let directory = self.data.join("wake-replies");
        let start = self.wake_reply_sequence.fetch_add(1, Ordering::Relaxed) as usize;
        (0..PHRASES.len()).find_map(|offset| {
            let path = directory.join(reply_name(
                profile,
                PHRASES[start.wrapping_add(offset) % PHRASES.len()],
            ));
            voice_preview::Sample::read(&path).filter(short_enough)
        })
    }

    pub(super) fn play_wake_reply(&self, sample: &voice_preview::Sample, engine: &str, opening_at: Instant) {
        if self.player.start(sample.rate).is_err() {
            return;
        }
        self.player.set_boost(sample.boost * 0.65);
        self.leds.set(Mode::Listening);
        if self.player.write(&sample.pcm).is_ok() {
            log::info!("Отклик на активацию из кеша ({engine}) за {:.2} с", opening_at.elapsed().as_secs_f64());
            self.player.finish(MAX_REPLY + Duration::from_millis(200));
        } else {
            self.player.stop(true);
        }
    }
}

fn short_enough(sample: &voice_preview::Sample) -> bool {
    sample.pcm.len() as u128 * 1000 <= sample.rate as u128 * MAX_REPLY.as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_replies_are_separate_for_each_voice_and_model() {
        assert!((10..=15).contains(&PHRASES.len()));
        let mut profile = Profile {
            engine: "gemini".into(),
            voice: "Kore".into(),
            live_model: "gemini-3.8-live".into(),
            ..Default::default()
        };
        let original = reply_name(&profile, PHRASES[0]);
        assert_ne!(reply_name(&profile, PHRASES[1]), original);
        profile.voice = "Fenrir".into();
        assert_ne!(reply_name(&profile, PHRASES[0]), original);
        profile.voice = "Kore".into();
        profile.live_model = "gemini-3.8-live-extended-thinking".into();
        assert_ne!(reply_name(&profile, PHRASES[0]), original);
    }
}

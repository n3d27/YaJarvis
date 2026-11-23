//! Догнать микрофон после подключения разговора, не трогая речь.
//!
//! Пока звонок поднимается (1–1,5 с после слова), фраза копится в очереди. Раньше очередь
//! догонялась сжатием времени — речь уходила в модель в 1,5–1,9 раза быстрее, и первую фразу
//! после слова модель не разбирала. Теперь выбрасывается только тишина: длинные паузы
//! укорачиваются до KEEP_PAUSE, а сама речь идёт кадр за кадром как есть. Темп RTP не меняется.

const FRAME: usize = 320;
const BLOCK: usize = 80;
/// Догоняем, только если в очереди больше этого (60 мс): живой звук не трогаем.
const CATCH_UP_ABOVE: usize = 960;
/// Сколько тишины оставить перед речью и в паузе между словами (100 мс).
const KEEP_PAUSE: usize = 1_600;
/// Блок тише этого — тишина (выход Yandex VQE уже без шума и эха).
const QUIET_AMP: u16 = 450;
const OVERLAP: usize = 80;

pub struct MicClock {
    continuation: Option<[i16; OVERLAP]>,
}

impl Default for MicClock {
    fn default() -> Self {
        Self::new()
    }
}

fn quiet_lead(queue: &[i16]) -> usize {
    queue
        .chunks_exact(BLOCK)
        .take_while(|b| b.iter().all(|v| v.unsigned_abs() <= QUIET_AMP))
        .count()
        * BLOCK
}

impl MicClock {
    pub fn new() -> Self {
        Self { continuation: None }
    }

    pub fn take(&mut self, queue: &mut Vec<i16>) -> [i16; FRAME] {
        let mut frame = [0; FRAME];
        let mut skip = 0;
        if queue.len() > CATCH_UP_ABOVE {
            skip = quiet_lead(queue).saturating_sub(KEEP_PAUSE);
        }
        let count = (queue.len() - skip).min(FRAME);
        frame[..count].copy_from_slice(&queue[skip..skip + count]);
        // Стык двух кусков тишины сглаживаем, чтобы не было щелчка.
        if skip > 0 && let Some(previous) = self.continuation.as_ref() {
            for i in 0..OVERLAP.min(count) {
                let n = i as i32;
                frame[i] = ((i32::from(previous[i]) * (OVERLAP as i32 - n) + i32::from(frame[i]) * n) / OVERLAP as i32) as i16;
            }
        }
        let consumed = skip + count;
        self.continuation = queue.get(consumed..consumed + OVERLAP).and_then(|next| next.try_into().ok());
        queue.drain(..consumed);
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(n: usize) -> Vec<i16> {
        (0..n).map(|i| ((i as f64 * std::f64::consts::TAU / 80.0).sin() * 5000.0) as i16).collect()
    }

    #[test]
    fn buffered_speech_is_sent_unchanged() {
        let speech = voice(32_000);
        let mut queue = speech.clone();
        let mut mic = MicClock::new();
        for i in 0..100 {
            let frame = mic.take(&mut queue);
            assert_eq!(frame[..], speech[i * FRAME..(i + 1) * FRAME], "кадр {i}");
        }
        assert!(queue.is_empty());
    }

    #[test]
    fn leading_silence_shrinks_to_a_short_pause() {
        let mut queue = vec![120i16; 16_000];
        queue.extend(voice(3_200));
        let mut mic = MicClock::new();
        let mut out = vec![];
        while !queue.is_empty() {
            out.extend(mic.take(&mut queue));
        }
        let first_loud = out.iter().position(|v| v.unsigned_abs() > QUIET_AMP).unwrap();
        assert!(first_loud <= KEEP_PAUSE + BLOCK, "тишины перед речью осталось {first_loud}");
        let speech = voice(3_200);
        let start = out.len() - 3_200;
        assert_eq!(out[start..], speech[..], "речь не тронута");
    }

    #[test]
    fn long_pause_between_words_shrinks_short_pause_stays() {
        let mut queue = voice(3_200);
        queue.extend(vec![0i16; 9_600]); // 600 мс
        queue.extend(voice(3_200));
        queue.extend(vec![0i16; 800]); // 50 мс — не трогаем
        queue.extend(voice(1_600));
        let mut mic = MicClock::new();
        let mut out = vec![];
        while !queue.is_empty() {
            out.extend(mic.take(&mut queue));
        }
        // Последний кадр добит нулями до 20 мс.
        assert_eq!(out.len(), (3_200 + KEEP_PAUSE + 3_200 + 800 + 1_600_usize).div_ceil(FRAME) * FRAME);
    }

    #[test]
    fn live_audio_is_never_skipped() {
        let mut mic = MicClock::new();
        let mut live = vec![0i16; 320];
        assert_eq!(mic.take(&mut live), [0; FRAME]);
        assert!(live.is_empty());
        let mut small = vec![0i16; CATCH_UP_ABOVE];
        mic.take(&mut small);
        assert_eq!(small.len(), CATCH_UP_ABOVE - FRAME);
    }
}

//! Where the speech is in a recording: Silero VAD (bundled, run by sherpa-onnx), or a loudness
//! threshold when its model can't be loaded.

use crate::audio::SAMPLE_RATE;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::OnceLock;

const RATE: usize = SAMPLE_RATE as usize;
/// Shorter pauses stay inside one speech segment.
const MIN_PAUSE_SECS: f32 = 0.25;
/// Shorter sounds aren't speech: a click, a key or a tap on the desk.
const MIN_SPEECH_SECS: f32 = 0.15;
/// Silence after which Silero stops reporting speech; pauses are merged by `MIN_PAUSE_SECS`.
const SILERO_SILENCE_SECS: f32 = 0.1;

static MODEL: OnceLock<PathBuf> = OnceLock::new();

/// The bundled `silero_vad.onnx`, once the app knows its resource folder.
pub fn set_model(path: PathBuf) {
    let _ = MODEL.set(path);
}

fn model() -> PathBuf {
    MODEL.get().cloned().unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/silero_vad.onnx"))
}

fn silero() -> Option<sherpa_onnx::VoiceActivityDetector> {
    let path = model();
    if !path.exists() {
        return None;
    }
    let config = sherpa_onnx::VadModelConfig {
        silero_vad: sherpa_onnx::SileroVadModelConfig {
            model: Some(path.to_string_lossy().into_owned()),
            threshold: 0.5,
            min_silence_duration: SILERO_SILENCE_SECS,
            min_speech_duration: MIN_SPEECH_SECS,
            window_size: 512,
            // Speech is tracked from `detected`; this only bounds what the detector buffers.
            max_speech_duration: 30.0,
        },
        sample_rate: SAMPLE_RATE as i32,
        num_threads: 1,
        provider: Some("cpu".into()),
        ..Default::default()
    };
    sherpa_onnx::VoiceActivityDetector::create(&config, 40.0)
}

/// Silero's window, 32 ms.
const WINDOW: usize = 512;
/// Silero reports speech this long after it starts (`min_speech_duration`, plus a window)...
const ONSET: usize = (MIN_SPEECH_SECS * RATE as f32) as usize + WINDOW;
/// ...and stops reporting it this long after it ends.
const RELEASE: usize = (SILERO_SILENCE_SECS * RATE as f32) as usize;

/// Speech segments of a recording fed to it as it grows, in samples from its start. A segment
/// closes a pause's length after its speech ends, so pauses are known as soon as they are long
/// enough (sherpa-onnx's own segments arrive seconds later).
pub struct Vad {
    silero: Option<sherpa_onnx::VoiceActivityDetector>,
    fed: usize,
    segments: Vec<Range<usize>>,
    /// The speech in progress: its start and where it last ended.
    run: Option<(usize, usize)>,
    /// Where the last speech heard ended, and whether the last frame was speech.
    heard_until: usize,
    voiced: bool,
}

impl Vad {
    pub fn new() -> Self {
        let silero = silero();
        if silero.is_none() {
            log::warn!("Silero VAD unavailable ({}); detecting speech by loudness", model().display());
        }
        Self { silero, fed: 0, segments: Vec::new(), run: None, heard_until: 0, voiced: false }
    }

    /// The loudness fallback alone, for synthetic test audio that isn't speech.
    #[cfg(test)]
    pub fn loudness() -> Self {
        Self { silero: None, fed: 0, segments: Vec::new(), run: None, heard_until: 0, voiced: false }
    }

    /// Processes what `audio` (the whole recording so far) has beyond what was fed before.
    pub fn feed(&mut self, audio: &[i16]) {
        let frame = if self.silero.is_some() { WINDOW } else { RATE / 20 };
        while self.fed + frame <= audio.len() {
            let (start, end) = (self.fed, self.fed + frame);
            self.fed = end;
            let (voiced, onset, release) = match &self.silero {
                Some(vad) => {
                    let samples: Vec<f32> = audio[start..end].iter().map(|s| *s as f32 / 32768.0).collect();
                    vad.accept_waveform(&samples);
                    // Its own segments aren't used; they would only pile up.
                    while !vad.is_empty() {
                        vad.pop();
                    }
                    (vad.detected(), ONSET, RELEASE)
                }
                None => (crate::audio::loud(&audio[start..end]), 0, 0),
            };
            self.voiced = voiced;
            if voiced {
                let speech_end = end.saturating_sub(release);
                self.heard_until = speech_end;
                self.run = Some(match self.run {
                    Some((s, _)) => (s, speech_end),
                    None => (start.saturating_sub(onset), speech_end),
                });
            } else {
                self.close_run(end);
            }
        }
    }

    /// Feeds the rest of `audio` and closes the speech in progress.
    pub fn finish(&mut self, audio: &[i16]) {
        self.feed(audio);
        self.fed = self.fed.max(audio.len());
        self.close_run(usize::MAX);
    }

    /// Speech segments closed so far, in order.
    pub fn segments(&self) -> &[Range<usize>] {
        &self.segments
    }

    /// Samples fed so far.
    pub fn fed(&self) -> usize {
        self.fed
    }

    /// How long it has been quiet since the last speech, before that pause is long enough to end
    /// a segment. 0 while speech is heard.
    pub fn quiet_for(&self) -> usize {
        if self.voiced { 0 } else { self.fed.saturating_sub(self.heard_until) }
    }

    /// Where the last speech heard ended.
    pub fn heard_until(&self) -> usize {
        self.heard_until
    }

    /// Whether speech that hasn't been closed into a segment yet is going on.
    pub fn speaking(&self) -> bool {
        self.run.is_some()
    }

    /// Where that speech started.
    pub fn speech_start(&self) -> Option<usize> {
        self.run.map(|(start, _)| start)
    }

    /// Whether any closed segment overlaps `range`.
    pub fn speech_in(&self, range: Range<usize>) -> bool {
        self.segments.iter().any(|s| s.start < range.end && range.start < s.end)
    }

    /// Ends the speech in progress if `now` is a pause's length past where it ended, plus the
    /// time Silero takes to report speech that started within that pause.
    fn close_run(&mut self, now: usize) {
        let Some((start, end)) = self.run else { return };
        let onset = if self.silero.is_some() { ONSET } else { 0 };
        if now.saturating_sub(end) < (MIN_PAUSE_SECS * RATE as f32) as usize + onset {
            return;
        }
        self.run = None;
        if end.saturating_sub(start) >= (MIN_SPEECH_SECS * RATE as f32) as usize {
            self.segments.push(start..end);
        }
    }
}

/// Whether a whole recording has any speech in it.
pub fn has_speech(audio: &[i16]) -> bool {
    let mut vad = Vad::new();
    vad.finish(audio);
    !vad.segments().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f64, amp: f64) -> Vec<i16> {
        (0..(secs * RATE as f64) as usize).map(|i| ((i as f64 * 0.05).sin() * amp) as i16).collect()
    }

    #[test]
    fn loudness_fallback_finds_segments_and_ignores_clicks() {
        let mut v = Vad::loudness();
        let mut audio = [vec![0; RATE], tone(1.0, 3000.0), vec![0; RATE / 2], tone(0.05, 3000.0), vec![0; RATE]].concat();
        audio.extend(tone(0.8, 3000.0));
        v.finish(&audio);
        assert_eq!(v.segments().len(), 2, "{:?}", v.segments());
        assert!((RATE - RATE / 20..RATE + RATE / 20).contains(&v.segments()[0].start));
        assert!(v.speech_in(RATE..RATE * 2) && !v.speech_in(RATE * 2 + RATE / 4..RATE * 3));
    }

    #[test]
    fn silero_finds_speech_in_the_bundled_sample_and_none_in_silence() {
        assert!(model().exists(), "resources/silero_vad.onnx");
        let sample = crate::audio::read_wav(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/sample.wav")).unwrap();
        assert!(has_speech(&sample));
        // Quiet speech (-24 dB) is still speech; silence and a tone are not.
        let quiet: Vec<i16> = sample.iter().map(|s| s / 16).collect();
        assert!(has_speech(&quiet));
        assert!(!has_speech(&vec![0; RATE * 3]));
        assert!(!has_speech(&tone(2.0, 3000.0)));
    }
}

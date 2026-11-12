//! Microphone capture. The input stream is opened only while recording. Audio is downmixed to mono,
//! resampled to 16 kHz PCM16 (what every STT provider accepts), kept in a buffer for batch/fallback
//! transcription and forwarded in ~100 ms chunks to an optional streaming session.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

pub const SAMPLE_RATE: u32 = 16_000;
const CHUNK: usize = SAMPLE_RATE as usize / 10;

pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| it.filter_map(|d| name_of(&d)).collect())
        .unwrap_or_default()
}

fn name_of(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|desc| desc.name().to_string())
}

pub struct Recording {
    stop: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<i16>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Recording {
    /// Starts capturing. Fails fast (before returning) if the device can't be opened.
    pub fn start(
        device_name: Option<String>,
        chunks: Option<UnboundedSender<Vec<i16>>>,
        on_level: impl Fn(f32) + Send + 'static,
    ) -> Result<Recording> {
        let (rec, ready) = Recording::open(device_name, chunks, on_level)?;
        Recording::wait_ready(&rec, &ready)?;
        Ok(rec)
    }

    /// Starts capturing without waiting for the device (opening can take ~1 s, e.g. the first
    /// PulseAudio connection or Bluetooth). Pass the receiver to `wait_ready` to learn the outcome.
    pub fn open(
        device_name: Option<String>,
        chunks: Option<UnboundedSender<Vec<i16>>>,
        on_level: impl Fn(f32) + Send + 'static,
    ) -> Result<(Recording, mpsc::Receiver<Result<()>>)> {
        let stop = Arc::new(AtomicBool::new(false));
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(SAMPLE_RATE as usize * 30)));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let (stop2, buffer2) = (stop.clone(), buffer.clone());
        // cpal streams are !Send on some platforms, so the stream lives on its own thread.
        let thread = std::thread::Builder::new().name("opstrellis-audio".into()).spawn(move || {
            if let Err(e) = capture(device_name, stop2, buffer2, chunks, on_level, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;
        Ok((Recording { stop, buffer, thread: Some(thread) }, ready_rx))
    }

    /// Blocks until the device opened (Ok) or failed / timed out (Err; capture is stopped).
    pub fn wait_ready(rec: &Recording, ready: &mpsc::Receiver<Result<()>>) -> Result<()> {
        Recording::wait_ready_with(&rec.stop, ready)
    }

    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    pub fn wait_ready_with(stop: &AtomicBool, ready: &mpsc::Receiver<Result<()>>) -> Result<()> {
        let t0 = Instant::now();
        // Ready means the first samples arrived, not just that the device opened: a Bluetooth
        // headset can take seconds to switch to its microphone profile and seconds more to send
        // audio. Until then the pill shows "Opening mic…" and Esc cancels.
        let r = ready.recv_timeout(MIC_READY_TIMEOUT);
        log::debug!("microphone opened in {:?}", t0.elapsed());
        match r {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                Err(anyhow!("Timed out opening the microphone"))
            }
        }
    }

    /// The most recent `max` samples (for live partial transcripts).
    pub fn buffer(&self) -> Arc<Mutex<Vec<i16>>> {
        self.buffer.clone()
    }

    pub fn stop(mut self) -> Vec<i16> {
        self.finish();
        std::mem::take(&mut *self.buffer.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.finish();
    }
}

fn capture(
    device_name: Option<String>,
    stop: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<i16>>>,
    chunks: Option<UnboundedSender<Vec<i16>>>,
    on_level: impl Fn(f32),
    ready: &mpsc::Sender<Result<()>>,
) -> Result<()> {
    let host = cpal::default_host();
    let device = match &device_name {
        Some(name) => host
            .input_devices()?
            .find(|d| name_of(d).as_ref() == Some(name))
            .or_else(|| host.default_input_device()),
        None => host.default_input_device(),
    }
    .context("No microphone found")?;
    let config = device.default_input_config().context("Microphone has no usable input format")?;
    let channels = config.channels() as usize;
    let rate = config.sample_rate();
    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    let err_fn = |e| log::error!("audio stream error: {e}");
    let mut stream_config: cpal::StreamConfig = config.into();
    let build_stream = |cfg: &cpal::StreamConfig, tx: mpsc::Sender<Vec<f32>>| match config.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, cfg, channels, tx, err_fn),
        cpal::SampleFormat::I16 => build::<i16>(&device, cfg, channels, tx, err_fn),
        cpal::SampleFormat::U16 => build::<u16>(&device, cfg, channels, tx, err_fn),
        cpal::SampleFormat::I32 => build::<i32>(&device, cfg, channels, tx, err_fn),
        f => Err(anyhow!("Unsupported sample format {f:?}")),
    };
    // Prefer a small buffer: the default can be huge (ALSA→PulseAudio uses 2 s), which delays
    // partials and can drop the first words. Not every backend accepts every size, so try a few.
    let mut stream = None;
    if let cpal::SupportedBufferSize::Range { min, max } = config.buffer_size() {
        for ms in [20, 50, 100, 200] {
            stream_config.buffer_size = cpal::BufferSize::Fixed((rate * ms / 1000).clamp(*min, *max));
            if let Ok(s) = build_stream(&stream_config, tx.clone()) {
                log::debug!("audio buffer {ms} ms");
                stream = Some(s);
                break;
            }
        }
    }
    let stream = match stream {
        Some(s) => s,
        None => {
            stream_config.buffer_size = cpal::BufferSize::Default;
            build_stream(&stream_config, tx)?
        }
    };
    stream.play().context("Could not start the microphone")?;
    log::info!("recording from {:?} at {rate} Hz x{channels}", name_of(&device));

    let mut ready = Some(ready);
    let mut resampler = Resampler::new(rate, SAMPLE_RATE);
    let mut pending: Vec<i16> = Vec::with_capacity(CHUNK * 2);
    let mut last_level = Instant::now();
    let mut level_acc = (0f64, 0usize);
    loop {
        let stopping = stop.load(Ordering::SeqCst);
        // Drain whatever the callback produced; after stop, take what's left and exit.
        while let Ok(frame) = rx.recv_timeout(Duration::from_millis(if stopping { 0 } else { 20 })) {
            let out = resampler.process(&frame);
            for s in &out {
                level_acc.0 += (*s as f64) * (*s as f64);
                level_acc.1 += 1;
            }
            let pcm: Vec<i16> = out.iter().map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).collect();
            buffer.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(&pcm);
            // `play()` can return before a Bluetooth device starts delivering audio. The cue
            // means it is safe to speak, so announce readiness only after the first samples.
            if !pcm.is_empty() {
                if let Some(ready) = ready.take() {
                    let _ = ready.send(Ok(()));
                }
            }
            pending.extend_from_slice(&pcm);
            if pending.len() >= CHUNK {
                if let Some(c) = &chunks {
                    let _ = c.send(std::mem::take(&mut pending));
                } else {
                    pending.clear();
                }
            }
            if stopping {
                continue;
            }
            // Frames arrive about as often as the timeout, so the drain may never time out on its own.
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if last_level.elapsed() >= Duration::from_millis(50) && level_acc.1 > 0 {
                on_level(level_from_rms((level_acc.0 / level_acc.1 as f64).sqrt() as f32));
                level_acc = (0.0, 0);
                last_level = Instant::now();
            }
        }
        if stopping {
            break;
        }
    }
    drop(stream);
    if let (Some(c), false) = (&chunks, pending.is_empty()) {
        let _ = c.send(pending);
    }
    Ok(())
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    tx: mpsc::Sender<Vec<f32>>,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    Ok(device.build_input_stream(
        *config,
        move |data: &[T], _| {
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|f| f.iter().map(|s| f32::from_sample(*s)).sum::<f32>() / channels as f32)
                .collect();
            let _ = tx.send(mono);
        },
        err_fn,
        None,
    )?)
}

fn level_from_rms(rms: f32) -> f32 {
    let db = 20.0 * rms.max(1e-6).log10();
    ((db + 55.0) / 50.0).clamp(0.0, 1.0)
}

/// Streaming area-averaging resampler: averages the input samples that fall into each output
/// period, which doubles as the anti-aliasing low-pass when downsampling (48k/44.1k -> 16k).
pub struct Resampler {
    step: f64,
    t: f64,
    acc: f32,
    n: u32,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Resampler { step: from as f64 / to as f64, t: 0.0, acc: 0.0, n: 0 }
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 2);
        for &x in input {
            if self.step <= 1.0 {
                // Upsampling (rare: 8 kHz mics) — sample-and-hold is adequate for speech.
                self.t += 1.0;
                while self.t >= self.step {
                    out.push(x);
                    self.t -= self.step;
                }
                continue;
            }
            self.acc += x;
            self.n += 1;
            self.t += 1.0;
            if self.t >= self.step {
                out.push(self.acc / self.n as f32);
                self.acc = 0.0;
                self.n = 0;
                self.t -= self.step;
            }
        }
        out
    }
}

pub fn to_wav(samples: &[i16]) -> Vec<u8> {
    let mut cur = std::io::Cursor::new(Vec::with_capacity(samples.len() * 2 + 44));
    let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    {
        let mut w = hound::WavWriter::new(&mut cur, spec).expect("wav header");
        for s in samples {
            w.write_sample(*s).expect("wav write");
        }
        w.finalize().expect("wav finalize");
    }
    cur.into_inner()
}

/// Reads any PCM WAV and converts it to 16 kHz mono i16 (used for the bundled sample and tests).
pub fn read_wav(path: &std::path::Path) -> Result<Vec<i16>> {
    let mut r = hound::WavReader::open(path)?;
    let spec = r.spec();
    if spec.channels == 1 && spec.sample_rate == SAMPLE_RATE && spec.bits_per_sample == 16 && spec.sample_format == hound::SampleFormat::Int {
        return Ok(r.samples::<i16>().collect::<Result<_, _>>()?);
    }
    let ch = spec.channels as usize;
    let floats: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 / scale)).collect::<Result<_, _>>()?
        }
    };
    let mono: Vec<f32> = floats.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();
    let out = Resampler::new(spec.sample_rate, SAMPLE_RATE).process(&mono);
    Ok(out.iter().map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).collect())
}

pub fn duration_ms(samples: &[i16]) -> i64 {
    samples.len() as i64 * 1000 / SAMPLE_RATE as i64
}

/// How long the microphone may take to deliver its first samples.
const MIC_READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Level (RMS) below which a 50 ms frame counts as silence.
const SILENCE_RMS: f64 = 150.0;
const FRAME: usize = SAMPLE_RATE as usize / 20;
/// Speech needs at least this many loud frames (150 ms): a click, a key or a tap on the desk is one
/// or two, and sending those to Whisper-style models makes them invent text ("Thanks for watching!").
const MIN_SPEECH_FRAMES: usize = 3;

fn rms(w: &[i16]) -> f64 {
    (w.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / w.len() as f64).sqrt()
}

/// True when the recording is effectively silent (nothing worth sending to a provider).
pub fn is_silent(samples: &[i16]) -> bool {
    if samples.len() < SAMPLE_RATE as usize / 4 {
        return true;
    }
    samples.chunks(FRAME).filter(|f| rms(f) >= SILENCE_RMS).take(MIN_SPEECH_FRAMES).count() < MIN_SPEECH_FRAMES
}

/// Where to cut `samples` so the part before can be transcribed on its own: the middle of the
/// last pause of at least `min_pause` samples that follows speech. Without such a pause, audio
/// longer than `max` is cut at its quietest 50 ms frame within the last 5 s.
pub fn split_point(samples: &[i16], min_pause: usize, max: usize) -> Option<usize> {
    let levels: Vec<f64> = samples.chunks_exact(FRAME).map(rms).collect();
    let (mut speech, mut run, mut cut) = (false, 0, None);
    for (i, &level) in levels.iter().enumerate() {
        if level >= SILENCE_RMS {
            (speech, run) = (true, 0);
        } else {
            run += 1;
            if speech && run * FRAME >= min_pause {
                cut = Some((i + 1 - run + run / 2) * FRAME);
            }
        }
    }
    if cut.is_none() && samples.len() > max {
        cut = Some(quietest(samples, levels.len().saturating_sub(5 * SAMPLE_RATE as usize / FRAME) * FRAME));
    }
    cut
}

/// Whether a frame is louder than silence.
pub fn loud(frame: &[i16]) -> bool {
    rms(frame) >= SILENCE_RMS
}

/// The middle of the quietest 50 ms frame of `samples` from `from` on (`samples.len()` if there is none).
pub fn quietest(samples: &[i16], from: usize) -> usize {
    let from = from / FRAME * FRAME;
    samples.get(from..).unwrap_or_default().chunks_exact(FRAME).enumerate().min_by(|a, b| rms(a.1).total_cmp(&rms(b.1))).map_or(samples.len(), |(i, _)| from + i * FRAME + FRAME / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live test (needs a microphone): cargo test stop_is_prompt -- --ignored
    #[test]
    #[ignore]
    fn stop_is_prompt() {
        for _ in 0..3 {
            let (rec, ready) = Recording::open(None, None, |_| {}).unwrap();
            Recording::wait_ready(&rec, &ready).unwrap();
            std::thread::sleep(Duration::from_millis(1000));
            let t = Instant::now();
            let ms = duration_ms(&rec.stop());
            assert!(t.elapsed() < Duration::from_millis(250), "stop took {:?}", t.elapsed());
            assert!(ms < 1300, "kept recording after stop: {ms} ms");
        }
    }

    #[test]
    fn splits_at_the_last_pause_after_speech() {
        let rate = SAMPLE_RATE as usize;
        let tone = |secs: f64| -> Vec<i16> { (0..(secs * rate as f64) as usize).map(|i| ((i as f64 * 0.05).sin() * 3000.0) as i16).collect() };
        let quiet = |secs: f64| vec![0i16; (secs * rate as f64) as usize];
        let join = |parts: &[Vec<i16>]| parts.concat();
        let (pause, max) = (rate * 6 / 10, rate * 30);
        // Leading silence alone, short gaps and speech still going: no cut.
        assert_eq!(split_point(&join(&[quiet(1.0), tone(2.0)]), pause, max), None);
        assert_eq!(split_point(&join(&[tone(1.0), quiet(0.3), tone(1.0)]), pause, max), None);
        // A 1 s pause after speech: cut in its middle, so both halves keep some silence.
        let cut = split_point(&join(&[tone(2.0), quiet(1.0), tone(1.0)]), pause, max).unwrap();
        assert!((rate * 2 + rate / 4..rate * 3 - rate / 4).contains(&cut), "cut at {cut}");
        // Two pauses: the last one wins.
        let cut = split_point(&join(&[tone(1.0), quiet(1.0), tone(1.0), quiet(1.0), tone(0.5)]), pause, max).unwrap();
        assert!(cut > rate * 3, "cut at {cut}");
        // No pause but too long: cut at the quietest frame near the end.
        let mut long = tone(31.0);
        let dip = rate * 29;
        long[dip..dip + FRAME].fill(0);
        assert_eq!(split_point(&long, pause, max), Some(dip + FRAME / 2));
    }

    #[test]
    fn resampler_ratio_and_tone() {
        let mut r = Resampler::new(48_000, 16_000);
        let input: Vec<f32> = (0..48_000).map(|i| (i as f32 * 440.0 * 2.0 * std::f32::consts::PI / 48_000.0).sin()).collect();
        // Feed in odd-sized chunks to exercise state carry-over.
        let mut out = Vec::new();
        for c in input.chunks(1234) {
            out.extend(r.process(c));
        }
        assert_eq!(out.len(), 16_000);
        let mut r = Resampler::new(44_100, 16_000);
        assert!((r.process(&vec![0.5; 44_100]).len() as i32 - 16_000).abs() <= 1);
    }

    #[test]
    fn wav_roundtrip() {
        let s: Vec<i16> = (0..1600).map(|i| (i * 10) as i16).collect();
        let wav = to_wav(&s);
        let path = std::env::temp_dir().join("opstrellis-test.wav");
        std::fs::write(&path, wav).unwrap();
        assert_eq!(read_wav(&path).unwrap(), s);
        assert!(is_silent(&vec![0; 16_000]));
        // A click (two loud frames) isn't speech; 150 ms of sound is.
        let mut click = vec![0i16; 16_000];
        click[..1_600].fill(3_000);
        assert!(is_silent(&click));
        click[..2_400].fill(3_000);
        assert!(!is_silent(&click));
    }
}

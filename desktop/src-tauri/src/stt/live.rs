//! Local dictation as it is recorded. A piece is committed at the first pause after it is long
//! enough, so on release only the last piece is left to decode. With models that give word
//! timestamps, each piece is decoded with the audio since a pause about 2 s before it and keeps
//! only the words that start inside it, so no piece starts without context. The live preview
//! settles the words two consecutive decodes agree on and never changes them again.

use super::local::Decoded;
use crate::audio::SAMPLE_RATE;
use crate::vad::Vad;
use anyhow::Result;

const RATE: usize = SAMPLE_RATE as usize;
/// Audio decoded before a piece.
pub const CONTEXT: usize = RATE * 2;
/// Audio decoded after a piece.
pub const LOOKAHEAD: usize = 0;
/// The shortest pause that ends a piece, for models with word timestamps...
pub const MIN_PAUSE: usize = RATE / 2;
/// ...and for models without, whose pieces have no context.
pub const MIN_PAUSE_PLAIN: usize = RATE * 3 / 5;
/// How often the recording thread looks at new audio. A tick with nothing to decode costs little,
/// and a draft started sooner after the last word is more often done when the key is released.
pub const TICK: std::time::Duration = std::time::Duration::from_millis(100);
/// A piece is committed at the first pause after this much audio.
pub const MIN_COMMIT: usize = RATE;
/// No piece is shorter.
const MIN_PIECE: usize = RATE;
const PREVIEW_EVERY: usize = RATE * 4 / 5;
/// Preview window of models without word timestamps.
const PREVIEW_TAIL: usize = RATE * 5;
/// The longest preview decode with word timestamps, which a release may have to wait for.
const PREVIEW_MAX: usize = RATE * 8;
/// Silence after the last speech that already counts as a pause to cut in.
const TRAILING_PAUSE: usize = RATE * 3 / 10;
/// Settled words end this far before the next word's start (half an 80 ms encoder frame).
const GUARD: usize = RATE / 25;
/// How far context may reach beyond its length or stop short of it, to end where it's quietest.
const SNAP: usize = RATE / 2;
/// How much further context may reach back to start at a pause.
const SLACK: usize = RATE * 2;
/// Speech this long without a word in a decoded piece means words were skipped.
const UNHEARD: usize = RATE * 2;
/// After this much quiet, what isn't committed yet is decoded in case the key is released now.
const DRAFT_AFTER: usize = RATE * 3 / 20;

pub type Decoder<'a> = dyn FnMut(&[i16]) -> Result<Decoded> + 'a;

pub struct Plan {
    /// A piece is committed at the first pause after this much audio.
    pub min_commit: usize,
    /// Pauses shorter than this don't end a piece unless the model's limit forces a cut.
    pub min_pause: usize,
    /// The longest audio the model takes in one pass.
    pub max: usize,
    /// Audio decoded before a piece; 0 for models without word timestamps.
    pub context: usize,
    /// Audio decoded after a piece, which a commit waits for; 0 without word timestamps.
    pub lookahead: usize,
}

struct Word {
    /// In samples from the recording's start.
    start: usize,
    /// With its leading space.
    text: String,
}

pub struct Live {
    plan: Plan,
    audio: Vec<i16>,
    vad: Vad,
    committed: usize,
    texts: Vec<String>,
    settled: Vec<Word>,
    settled_end: usize,
    tentative: Vec<Word>,
    previewed_at: usize,
    shown: String,
    /// The rest after `committed`, decoded at a pause: the transcript's end if nothing is said after.
    draft: Option<Draft>,
}

struct Draft {
    committed: usize,
    /// Speech heard up to here was decoded.
    heard_until: usize,
    text: String,
    /// The previous piece's last word as this decode wrote it (see `punctuate_last`).
    boundary: Option<Word>,
}

impl Live {
    pub fn new(plan: Plan) -> Self {
        Self { plan, audio: Vec::new(), vad: Vad::new(), committed: 0, texts: Vec::new(), settled: Vec::new(), settled_end: 0, tentative: Vec::new(), previewed_at: 0, shown: String::new(), draft: None }
    }

    /// Samples received so far.
    pub fn len(&self) -> usize {
        self.audio.len()
    }

    pub fn push(&mut self, samples: &[i16]) {
        self.audio.extend_from_slice(samples);
        self.vad.feed(&self.audio);
    }

    /// Commits what pauses end and, with `preview`, re-decodes the rest about once a second.
    /// Returns the text to show when it changed.
    pub fn tick(&mut self, decode: &mut Decoder, preview: bool) -> Result<Option<String>> {
        let mut changed = false;
        while let Some(cut) = self.cut(false) {
            self.commit(cut, false, decode)?;
            changed = true;
        }
        if self.draft_due() {
            self.draft(decode)?;
            changed = true;
        } else if preview && self.audio.len() >= self.previewed_at + PREVIEW_EVERY {
            self.previewed_at = self.audio.len();
            self.preview(decode)?;
            changed = true;
        }
        if !preview || !changed {
            return Ok(None);
        }
        let shown = self.shown_text();
        if shown.is_empty() || shown == self.shown {
            return Ok(None);
        }
        self.shown.clone_from(&shown);
        Ok(Some(shown))
    }

    /// Decodes what is left and returns the whole transcript.
    pub fn finish(mut self, decode: &mut Decoder) -> Result<String> {
        self.vad.finish(&self.audio);
        while let Some(cut) = self.cut(true) {
            self.commit(cut, false, decode)?;
        }
        let end = self.audio.len();
        // Nothing was said after the draft: it is the rest of the transcript.
        if let Some(d) = self.draft.take().filter(|d| d.committed == self.committed && self.vad.heard_until() <= d.heard_until) {
            self.punctuate_last(d.boundary.as_ref());
            if !d.text.is_empty() {
                self.texts.push(d.text);
            }
            return Ok(self.texts.join(" ").trim().to_string());
        }
        if end > self.committed {
            // Nothing heard at all: decode anyway rather than trust the detector with the whole recording.
            let force = self.vad.segments().is_empty();
            self.commit(end, force, decode)?;
        }
        Ok(self.texts.join(" ").trim().to_string())
    }

    /// Whether pieces are cut by word timestamps.
    fn timed(&self) -> bool {
        self.plan.context > 0 || self.plan.lookahead > 0
    }

    fn has_speech(&self, range: std::ops::Range<usize>) -> bool {
        let closed = self.vad.segments().last().map_or(0, |s| s.end);
        self.vad.speech_in(range.clone()) || (self.vad.speaking() && range.end > closed)
    }

    /// Candidate cuts: the middle of each pause between speech segments, before the speech going
    /// on now, or in the silence after the last segment, with the pause's length.
    fn pauses(&self) -> Vec<(usize, usize)> {
        let segs = self.vad.segments();
        let mut out: Vec<(usize, usize)> = segs.windows(2).map(|w| ((w[0].end + w[1].start) / 2, w[1].start.saturating_sub(w[0].end))).collect();
        if let Some(last) = segs.last() {
            match self.vad.speech_start() {
                Some(next) => out.push(((last.end + next.max(last.end)) / 2, next.saturating_sub(last.end))),
                None => {
                    let gap = self.vad.fed().saturating_sub(last.end);
                    if gap >= TRAILING_PAUSE {
                        out.push((last.end + gap.min(RATE) / 2, gap));
                    }
                }
            }
        }
        out
    }

    /// How far context before a piece reaches at most: back to a pause within `SLACK` of its
    /// length, or the quietest moment within `SNAP` of it.
    fn reach_before(&self) -> usize {
        if self.plan.context > 0 { self.plan.context + SLACK } else { 0 }
    }

    /// How far lookahead after a piece reaches at most: the quietest moment within `SNAP` of its length.
    fn reach_after(&self) -> usize {
        if self.plan.lookahead > 0 { self.plan.lookahead + SNAP } else { 0 }
    }

    /// Where to end the next piece: while recording, the first pause after `min_commit` (with
    /// room for lookahead). Once the piece would outgrow the model, and at the end while what is
    /// left doesn't fit in one pass, the latest pause of `min_pause` that fits, else the longest
    /// pause, else the quietest moment.
    fn cut(&self, last: bool) -> Option<usize> {
        let (start, len) = (self.committed, self.audio.len());
        let (before, after) = (self.reach_before(), self.reach_after());
        let longest = self.plan.max.saturating_sub(before + after).max(MIN_PIECE * 2);
        let pending = len - start;
        if last && pending + before <= self.plan.max {
            return None;
        }
        if !last && pending < self.plan.min_commit.min(longest) {
            return None;
        }
        let room = if last { 0 } else { after };
        let (lo, hi) = (start + MIN_PIECE, (start + longest).min(len.saturating_sub(room)));
        if hi <= lo {
            return None;
        }
        let pauses: Vec<(usize, usize)> = self.pauses().into_iter().filter(|(c, _)| (lo..=hi).contains(c)).collect();
        // The longest pause, the earliest among equals.
        let best = |min: usize, pause: usize| pauses.iter().filter(|(c, gap)| c - start >= min && *gap >= pause).min_by_key(|(c, gap)| (std::cmp::Reverse(*gap), *c)).map(|(c, _)| *c);
        if !last {
            if let Some(c) = best(self.plan.min_commit, self.plan.min_pause) {
                return Some(c);
            }
            if pending < longest + room {
                return None;
            }
        }
        let latest = pauses.iter().filter(|(_, gap)| *gap >= self.plan.min_pause).map(|(c, _)| *c).max();
        latest.or_else(|| best(0, 0)).or_else(|| Some(crate::audio::quietest(&self.audio[..hi], hi.saturating_sub(RATE * 5).max(lo))))
    }

    /// The audio decoded for the piece `start..cut`, with its context and lookahead. Context
    /// starts at the pause nearest `context` before the piece: a model that starts mid-sentence
    /// may write the whole piece in lower case without punctuation.
    fn context(&self, start: usize, cut: usize) -> (usize, usize) {
        let (ctx, ahead, len) = (self.plan.context, self.plan.lookahead, self.audio.len());
        let target = start.saturating_sub(ctx);
        let pause = self.pauses().into_iter().map(|(c, _)| c).filter(|&c| c + SNAP <= start && c + ctx + SLACK >= start).min_by_key(|&c| c.abs_diff(target));
        let from = match (ctx, pause) {
            (0, _) => start,
            (_, Some(p)) => p,
            _ if target <= SNAP => 0,
            _ => crate::audio::quietest(&self.audio[..target + SNAP], target - SNAP),
        };
        let to = if ahead == 0 {
            cut
        } else if cut + ahead.saturating_sub(SNAP) >= len {
            len
        } else {
            crate::audio::quietest(&self.audio[..(cut + ahead + SNAP).min(len)], cut + ahead - SNAP)
        };
        (from, to)
    }

    /// The longest stretch of detected speech in `start..cut` in which no word starts.
    fn unheard(&self, words: &[Word], start: usize, cut: usize) -> usize {
        let closed = self.vad.segments().last().map_or(start, |s| s.end);
        let open = (self.vad.speaking() && closed < cut).then(|| closed.max(start)..cut);
        self.vad
            .segments()
            .iter()
            .map(|s| s.start.max(start)..s.end.min(cut))
            .chain(open)
            .filter(|s| s.start < s.end)
            .map(|s| {
                let mut at: Vec<usize> = words.iter().map(|w| w.start).filter(|w| s.contains(w)).collect();
                at.insert(0, s.start);
                at.push(s.end);
                at.windows(2).map(|p| p[1] - p[0]).max().unwrap_or(0)
            })
            .max()
            .unwrap_or(0)
    }

    /// A draft is made once per pause, for models with word timestamps, when the rest fits in one pass.
    fn draft_due(&self) -> bool {
        let fresh = self.draft.as_ref().is_none_or(|d| d.committed != self.committed || d.heard_until < self.vad.heard_until());
        self.timed() && fresh && self.vad.quiet_for() >= DRAFT_AFTER && self.has_speech(self.committed..self.audio.len()) && self.audio.len() - self.committed + self.reach_before() <= self.plan.max
    }

    fn draft(&mut self, decode: &mut Decoder) -> Result<()> {
        let (start, end) = (self.committed, self.audio.len());
        let (text, boundary) = self.decode_piece(start, end, decode)?;
        // The draft is also the best preview of the rest.
        self.settled = if text.is_empty() { Vec::new() } else { vec![Word { start, text: format!(" {text}") }] };
        self.tentative.clear();
        self.settled_end = self.settled_end.max(self.vad.heard_until());
        self.previewed_at = end;
        self.draft = Some(Draft { committed: start, heard_until: self.vad.heard_until(), text, boundary });
        Ok(())
    }

    fn commit(&mut self, cut: usize, force: bool, decode: &mut Decoder) -> Result<()> {
        let start = self.committed;
        let speech = force || self.has_speech(start..cut);
        self.committed = cut;
        self.settled.retain(|w| w.start >= cut);
        self.tentative.retain(|w| w.start >= cut);
        self.settled_end = self.settled_end.max(cut);
        if !speech {
            return Ok(());
        }
        let (text, boundary) = self.decode_piece(start, cut, decode)?;
        self.punctuate_last(boundary.as_ref());
        if !text.is_empty() {
            self.texts.push(text);
        }
        Ok(())
    }

    /// The text of the piece `start..cut`, and the previous piece's last word as this decode
    /// wrote it, if it had that as context.
    fn decode_piece(&self, start: usize, cut: usize, decode: &mut Decoder) -> Result<(String, Option<Word>)> {
        let (from, to) = self.context(start, cut);
        let d = decode(&self.audio[from..to])?;
        let (text, boundary) = match d.words {
            Some(words) if self.timed() => {
                let mut words: Vec<Word> = words_at(words, from).collect();
                let inside = words.iter().position(|w| w.start >= start).unwrap_or(words.len());
                let boundary = inside.checked_sub(1).map(|i| words.remove(i));
                let in_piece = |words: Vec<Word>| words.into_iter().filter(|w| (start..cut).contains(&w.start)).collect::<Vec<_>>();
                let mut kept = in_piece(words);
                // A transducer can skip whole sentences after context that starts badly (seen in noise).
                // Then decoding the piece from its own start usually hears them.
                if from < start && self.unheard(&kept, start, cut) > UNHEARD {
                    let retry = in_piece(words_at(decode(&self.audio[start..to])?.words.unwrap_or_default(), start).collect());
                    if self.unheard(&retry, start, cut) < self.unheard(&kept, start, cut) {
                        kept = retry;
                    }
                }
                (kept.into_iter().map(|w| w.text).collect(), boundary)
            }
            // No timestamps this time: the text covers the context too, so decode the piece alone.
            None if from < start => (decode(&self.audio[start..to])?.text, None),
            _ => (d.text, None),
        };
        Ok((text.trim().to_string(), boundary))
    }

    /// A piece decoded up to a pause may end without the punctuation it gets once the model hears
    /// what follows. `boundary` is the previous piece's last word as the next decode, which had it
    /// as context, wrote it.
    fn punctuate_last(&mut self, boundary: Option<&Word>) {
        let (Some(last), Some(boundary)) = (self.texts.last_mut(), boundary) else { return };
        let mark = boundary.text.chars().last().filter(|c| ".?!".contains(*c));
        let ends_bare = last.chars().last().is_some_and(char::is_alphanumeric);
        let same = last.split_whitespace().last().is_some_and(|w| same_word(w, &boundary.text));
        if let (Some(mark), true, true) = (mark, ends_bare, same) {
            last.push(mark);
        }
    }

    fn preview(&mut self, decode: &mut Decoder) -> Result<()> {
        let len = self.audio.len();
        let from_settled = self.settled_end.max(self.committed);
        if !self.has_speech(from_settled..len) {
            return Ok(());
        }
        if !self.timed() {
            let from = from_settled.max(len.saturating_sub(PREVIEW_TAIL));
            let d = decode(&self.audio[from..])?;
            self.tentative = vec![Word { start: from, text: format!(" {}", d.text.trim()) }];
            return Ok(());
        }
        let from = from_settled.saturating_sub(self.plan.context).max(len.saturating_sub(PREVIEW_MAX.min(self.plan.max)));
        let d = decode(&self.audio[from..])?;
        let mut hyp: Vec<Word> = words_at(d.words.unwrap_or_default(), from).filter(|w| w.start >= from_settled).collect();
        // The newest word may still be cut off by the end of the audio.
        let agreed = hyp.iter().zip(&self.tentative).take_while(|(a, b)| same_word(&a.text, &b.text)).count().min(hyp.len().saturating_sub(1));
        let rest = hyp.split_off(agreed);
        if let Some(next) = rest.first().filter(|_| agreed > 0) {
            self.settled_end = self.settled_end.max(next.start.saturating_sub(GUARD));
        }
        self.settled.extend(hyp);
        self.tentative = rest;
        Ok(())
    }

    fn shown_text(&self) -> String {
        let live: String = self.settled.iter().chain(&self.tentative).map(|w| w.text.as_str()).collect();
        format!("{} {}", self.texts.join(" "), live.trim()).trim().to_string()
    }
}

fn words_at(words: Vec<super::local::Word>, from: usize) -> impl Iterator<Item = Word> {
    words.into_iter().map(move |w| Word { start: from + (w.start.max(0.0) * RATE as f32) as usize, text: w.text })
}

fn same_word(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::super::local::{Decoded, Word as W};
    use super::*;

    /// A recording of "words": each word is 0.4 s of tone, its index encoded in the pitch, so a
    /// fake decoder can read the words back with timestamps, like a transducer would.
    fn speech(words: &[usize], gaps: &[f32]) -> Vec<i16> {
        let mut audio = vec![0i16; RATE / 2];
        for (i, w) in words.iter().enumerate() {
            let period = 8 + *w;
            audio.extend((0..RATE * 2 / 5).map(|j| if (j / period) % 2 == 0 { 4000 } else { -4000 }));
            audio.extend(vec![0i16; (gaps.get(i).copied().unwrap_or(0.15) * RATE as f32) as usize]);
        }
        audio
    }

    fn fake_decoder(calls: &std::cell::Cell<usize>) -> impl FnMut(&[i16]) -> Result<Decoded> + '_ {
        move |a: &[i16]| {
            calls.set(calls.get() + 1);
            let mut words = Vec::new();
            let mut i = 0;
            while i < a.len() {
                if a[i] == 0 {
                    i += 1;
                    continue;
                }
                let run = a[i..].iter().take_while(|s| **s == a[i]).count();
                let end = i + a[i..].iter().take_while(|s| **s != 0).count();
                // Only whole words are recognised.
                if end - i >= RATE * 2 / 5 - 2 {
                    words.push(W { text: format!(" w{}", run - 8), start: i as f32 / RATE as f32 });
                }
                i = end;
            }
            Ok(Decoded { text: words.iter().map(|w| w.text.as_str()).collect::<String>().trim().to_string(), words: Some(words) })
        }
    }

    fn plan() -> Plan {
        Plan { min_commit: MIN_COMMIT, min_pause: MIN_PAUSE, max: RATE * 20, context: CONTEXT, lookahead: LOOKAHEAD }
    }

    fn run(audio: &[i16], preview: bool) -> (String, Vec<String>) {
        let calls = std::cell::Cell::new(0);
        let mut decode = fake_decoder(&calls);
        let mut live = Live::new(plan());
        live.vad = Vad::loudness();
        let mut shown = Vec::new();
        for chunk in audio.chunks(RATE * 3 / 10) {
            live.push(chunk);
            if let Some(s) = live.tick(&mut decode, preview).unwrap() {
                shown.push(s);
            }
        }
        (live.finish(&mut decode).unwrap(), shown)
    }

    fn expected(words: &[usize]) -> String {
        words.iter().map(|w| format!("w{w}")).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn pieces_join_without_lost_or_doubled_words() {
        // 40 words with pauses of 0.15–0.9 s: several commits with overlapping context.
        let words: Vec<usize> = (0..40).map(|i| i % 9).collect();
        let gaps: Vec<f32> = (0..40).map(|i| [0.15, 0.4, 0.15, 0.9, 0.3][i % 5]).collect();
        let (text, _) = run(&speech(&words, &gaps), false);
        assert_eq!(text, expected(&words));
    }

    #[test]
    fn speech_without_pauses_is_cut_and_still_complete() {
        let words: Vec<usize> = (0..70).map(|i| (i * 7) % 11).collect();
        let gaps = vec![0.05; 70];
        let (text, _) = run(&speech(&words, &gaps), false);
        assert_eq!(text, expected(&words));
    }

    #[test]
    fn preview_only_grows_and_ends_with_the_transcript() {
        let words: Vec<usize> = (0..30).map(|i| (i * 5) % 10).collect();
        let gaps: Vec<f32> = (0..30).map(|i| if i % 6 == 5 { 0.7 } else { 0.15 }).collect();
        let (text, shown) = run(&speech(&words, &gaps), true);
        assert_eq!(text, expected(&words));
        assert!(shown.len() > 5, "{shown:?}");
        for pair in shown.windows(2) {
            let (a, b): (Vec<&str>, Vec<&str>) = (pair[0].split(' ').collect(), pair[1].split(' ').collect());
            // Only the newest (tentative) word may change between updates.
            let kept = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
            assert!(kept + 1 >= a.len(), "retracted settled words: {:?} -> {:?}", pair[0], pair[1]);
        }
    }

    #[test]
    fn silence_is_never_decoded_and_short_recordings_decode_once() {
        let calls = std::cell::Cell::new(0);
        let mut decode = fake_decoder(&calls);
        let mut live = Live::new(plan());
        live.vad = Vad::loudness();
        live.push(&vec![0; RATE * 30]);
        live.tick(&mut decode, true).unwrap();
        assert_eq!(calls.get(), 0, "no preview or commit of silence");
        // Nothing heard: one decode anyway, in pieces the model takes.
        assert_eq!(live.finish(&mut decode).unwrap(), "");
        assert!(calls.get() >= 1);

        let calls = std::cell::Cell::new(0);
        let mut decode = fake_decoder(&calls);
        let mut live = Live::new(plan());
        live.vad = Vad::loudness();
        live.push(&speech(&[1, 2, 3], &[0.15, 0.7, 0.2]));
        assert_eq!(live.finish(&mut decode).unwrap(), "w1 w2 w3");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_piece_that_skipped_words_after_its_context_is_decoded_again() {
        let calls = std::cell::Cell::new(0);
        let mut inner = fake_decoder(&calls);
        // Like a transducer that loses its way after context: clips over 9 s drop words 2.5–6 s in.
        let mut decode = |a: &[i16]| {
            inner(a).map(|mut d| {
                if a.len() > RATE * 9 {
                    d.words.as_mut().unwrap().retain(|w| !(2.5..6.0).contains(&w.start));
                }
                d
            })
        };
        let mut live = Live::new(Plan { min_commit: RATE * 5, ..plan() });
        live.vad = Vad::loudness();
        let words: Vec<usize> = (0..36).map(|i| i % 10).collect();
        let gaps: Vec<f32> = (0..36).map(|i| if i % 6 == 5 { 0.7 } else { 0.15 }).collect();
        for chunk in speech(&words, &gaps).chunks(RATE * 3 / 10) {
            live.push(chunk);
            live.tick(&mut decode, false).unwrap();
        }
        assert_eq!(live.finish(&mut decode).unwrap(), expected(&words));
    }

    #[test]
    fn a_draft_made_at_the_last_pause_is_the_end_of_the_transcript() {
        let calls = std::cell::Cell::new(0);
        let mut decode = fake_decoder(&calls);
        let mut live = Live::new(plan());
        live.vad = Vad::loudness();
        let words = [1, 2, 3, 4, 5, 6];
        let audio = [speech(&words, &[0.15, 0.15, 0.7, 0.15, 0.15, 0.0]), vec![0; RATE * 2 / 5]].concat();
        for chunk in audio.chunks(RATE / 10) {
            live.push(chunk);
            live.tick(&mut decode, false).unwrap();
        }
        let before = calls.get();
        assert_eq!(live.finish(&mut decode).unwrap(), expected(&words));
        assert_eq!(calls.get(), before, "released 0.4 s after the last word: nothing left to decode");

        // Speech after the draft: it is decoded again.
        let calls = std::cell::Cell::new(0);
        let mut decode = fake_decoder(&calls);
        let mut live = Live::new(plan());
        live.vad = Vad::loudness();
        let more = [1, 2, 3, 4, 5, 6, 7];
        let audio = speech(&more, &[0.15, 0.15, 0.7, 0.15, 0.15, 0.4, 0.0]);
        for chunk in audio.chunks(RATE / 10) {
            live.push(chunk);
            live.tick(&mut decode, false).unwrap();
        }
        assert_eq!(live.finish(&mut decode).unwrap(), expected(&more));
    }

    #[test]
    fn a_pause_is_cut_even_when_seen_only_after_speech_resumed() {
        let calls = std::cell::Cell::new(0);
        let mut decode = fake_decoder(&calls);
        let mut live = Live::new(plan());
        live.vad = Vad::loudness();
        // One tick sees the first phrase, a 0.8 s pause and the start of the next phrase at once.
        live.push(&speech(&[1, 2, 3, 4, 5], &[0.15, 0.15, 0.15, 0.15, 0.8]));
        live.push(&speech(&[6, 7], &[0.15, 0.15])[RATE / 2..]);
        live.tick(&mut decode, false).unwrap();
        assert_eq!(live.texts, ["w1 w2 w3 w4 w5"]);
    }

    #[test]
    fn a_piece_gets_the_period_the_next_decode_heard() {
        let mut live = Live::new(plan());
        let word = |text: &str| Word { start: 0, text: text.into() };
        live.texts = vec!["Run the tests".into()];
        live.punctuate_last(Some(&word(" tests.")));
        assert_eq!(live.texts, ["Run the tests."]);
        // Only a bare ending, and only the same word.
        live.punctuate_last(Some(&word(" tests?")));
        live.texts.push("then deploy,".into());
        live.punctuate_last(Some(&word(" deploy.")));
        live.texts.push("and wait".into());
        live.punctuate_last(Some(&word(" later.")));
        assert_eq!(live.texts, ["Run the tests.", "then deploy,", "and wait"]);
    }

    #[test]
    fn models_without_timestamps_get_plain_pieces() {
        let calls = std::cell::Cell::new(0);
        let mut inner = fake_decoder(&calls);
        let mut decode = |a: &[i16]| inner(a).map(|d| Decoded { words: None, ..d });
        let mut live = Live::new(Plan { context: 0, lookahead: 0, ..plan() });
        live.vad = Vad::loudness();
        let words: Vec<usize> = (0..30).map(|i| i % 7).collect();
        let gaps: Vec<f32> = (0..30).map(|i| if i % 5 == 4 { 0.8 } else { 0.15 }).collect();
        for chunk in speech(&words, &gaps).chunks(RATE / 4) {
            live.push(chunk);
            live.tick(&mut decode, true).unwrap();
        }
        assert_eq!(live.finish(&mut decode).unwrap(), expected(&words));
    }
}

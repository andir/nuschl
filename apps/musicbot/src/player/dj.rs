// SPDX-License-Identifier: EUPL-1.2
//! DJ mode: two decks, each fed by its own decode child, mixed live into the
//! player's outputs (Mumble and browser listeners). Controls set targets in
//! the state; the engine moves towards them frame by frame (`dsp.rs`), so
//! changes that arrive in bursts over the network glide smoothly.
use super::{
    CHANNELS, Decoding, Entry, FRAME, HistoryOutcome, MAX_QUEUE, Output, Player, Playing, State,
    TICK, UNDERRUN_END, apply_gain, decode,
    dsp::{self, Curve, Limiter, Ramp, Resampler, Tone, ToneFilter},
    next_gain, target_gain, wall_clock_ms,
};
use crate::subsonic::{Library, Track};
use std::{fmt, time::Duration};
use tokio::{
    sync::mpsc::error::TryRecvError,
    time::{Instant, sleep, sleep_until},
};

pub const DECKS: usize = 2;
/// The tempo fader's ranges, in percent.
pub const TEMPO_RANGES: [u8; 2] = [8, 16];
/// The one-button transition's length, in seconds.
pub const MIN_TRANSITION_SECONDS: u32 = 1;
pub const MAX_TRANSITION_SECONDS: u32 = 60;
const FRAME_SAMPLES: u64 = FRAME as u64;
const SAMPLES_PER_MS: u64 = 48;
/// A deck sounds in the room at this gain in the mix or more (−20 dB).
const AUDIBLE: f32 = 0.1;
/// Songs in a row that fail to decode before auto load stops.
const MAX_FAILURES: u32 = 3;
/// The pause before a deck's next decoder after a failure; it doubles with
/// every failure in a row, up to `MAX_BACKOFF`.
const BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(8);
/// Encoder failures in a row before DJ mode pauses.
const MAX_ENCODE_FAILURES: u32 = 3;

/// One deck: a loaded song, its transport and its channel strip.
#[derive(Clone, Debug)]
pub struct Deck {
    /// The loaded song and its play.
    pub track: Option<Playing>,
    pub playing: bool,
    /// The position in the song, in samples at 48 kHz.
    pub position: u64,
    /// The cue point, in samples.
    pub cue: u64,
    /// The channel fader, in percent.
    pub volume: u8,
    /// Low, mid and high, in dB.
    pub eq: [f32; 3],
    /// −1 (low-pass closed) to 1 (high-pass closed); 0 is neutral.
    pub filter: f32,
    /// The speed change as a fraction: 0.08 plays 8 % faster and higher.
    pub tempo: f32,
    /// The tempo fader's range, in percent.
    pub range: u8,
    /// Why this deck's last song stopped early.
    pub error: Option<String>,
    /// The song came from the queue and returns there unheard.
    queued: bool,
    /// Load the queue's next song once this deck has faded out.
    reload: bool,
    /// After a failure, this deck's next decoder starts at this time.
    restart_at: Option<Instant>,
}

impl Default for Deck {
    fn default() -> Self {
        Self {
            track: None,
            playing: false,
            position: 0,
            cue: 0,
            volume: 100,
            eq: [0.0; 3],
            filter: 0.0,
            tempo: 0.0,
            range: TEMPO_RANGES[0],
            error: None,
            queued: false,
            reload: false,
            restart_at: None,
        }
    }
}

impl Deck {
    /// How much faster than recorded the song plays.
    pub fn ratio(&self) -> f64 {
        1.0 + f64::from(self.tempo)
    }

    /// The song's BPM from the library, before the tempo fader.
    pub fn base_bpm(&self) -> Option<u32> {
        self.track.as_ref()?.entry.track.bpm
    }

    /// The BPM at the current tempo.
    pub fn bpm(&self) -> Option<f64> {
        self.base_bpm().map(|bpm| f64::from(bpm) * self.ratio())
    }

    pub fn position_ms(&self) -> u64 {
        self.position / SAMPLES_PER_MS
    }

    pub fn cue_ms(&self) -> u64 {
        self.cue / SAMPLES_PER_MS
    }

    fn tone(&self) -> Tone {
        Tone {
            eq: self.eq,
            filter: self.filter,
        }
    }

    /// Moves to `position` (samples, clamped to the song's length); the
    /// decoder restarts there.
    fn seek_to(&mut self, position: u64) -> Result<(), DjError> {
        let track = self.track.as_mut().ok_or(DjError::Empty)?;
        let length = track
            .entry
            .track
            .duration
            .map_or(u64::MAX, |seconds| u64::from(seconds) * 48_000);
        self.position = position.min(length);
        track.frames = self.position / FRAME_SAMPLES;
        track.generation += 1;
        track.buffering = true;
        Ok(())
    }

    /// Takes the song off; the deck keeps its channel strip.
    fn eject(&mut self) -> Option<Playing> {
        self.playing = false;
        self.position = 0;
        self.cue = 0;
        self.reload = false;
        self.queued = false;
        self.track.take()
    }
}

/// The one-button fade from one deck to the other.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
    /// The deck the mix moves to.
    pub to: usize,
    from: f32,
    target: f32,
    pub frames: u32,
    pub elapsed: u32,
}

impl Transition {
    fn new(crossfader: f32, to: usize, seconds: u32) -> Self {
        Self {
            to,
            from: crossfader,
            target: if to == 0 { -1.0 } else { 1.0 },
            frames: (seconds * 50).max(1),
            elapsed: 0,
        }
    }

    /// One frame on: the crossfader's position and whether the fade is done.
    fn advance(&mut self) -> (f32, bool) {
        self.elapsed = (self.elapsed + 1).min(self.frames);
        let share = self.elapsed as f32 / self.frames as f32;
        (
            self.from + (self.target - self.from) * share,
            self.elapsed == self.frames,
        )
    }

    pub fn duration_ms(&self) -> u64 {
        u64::from(self.frames) * 20
    }

    pub fn elapsed_ms(&self) -> u64 {
        u64::from(self.elapsed) * 20
    }
}

/// DJ mode's state: the decks and the mixer.
#[derive(Clone, Debug)]
pub struct Dj {
    pub decks: [Deck; DECKS],
    /// −1 (only deck A) to 1 (only deck B).
    pub crossfader: f32,
    pub curve: Curve,
    pub transition: Option<Transition>,
    /// After a transition or at the end of a song, the silent deck loads the
    /// queue's next song.
    pub auto_load: bool,
    /// A message for the page, such as why auto load stopped.
    pub notice: Option<String>,
    failures: u32,
    encode_failures: u32,
}

impl Default for Dj {
    fn default() -> Self {
        Self {
            decks: [Deck::default(), Deck::default()],
            crossfader: -1.0,
            curve: Curve::Smooth,
            transition: None,
            auto_load: true,
            notice: None,
            failures: 0,
            encode_failures: 0,
        }
    }
}

impl Dj {
    /// Each deck's gain in the mix: its channel fader times the crossfader.
    pub fn gains(&self) -> [f32; DECKS] {
        let crossfade = dsp::crossfade(self.crossfader, self.curve);
        [0, 1].map(|deck| dsp::fader_gain(self.decks[deck].volume) * crossfade[deck])
    }

    /// The deck the room hears most: a playing deck before a paused one,
    /// then the louder one; deck A on a tie.
    pub fn lead(&self) -> Option<usize> {
        let gains = self.gains();
        (0..DECKS)
            .filter(|&deck| self.decks[deck].track.is_some())
            .max_by(|&a, &b| {
                (self.decks[a].playing, gains[a])
                    .partial_cmp(&(self.decks[b].playing, gains[b]))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.cmp(&a))
            })
    }

    /// The deck song whose heard play started last, else the lead deck's song.
    pub(super) fn now_playing(&self) -> Option<&Playing> {
        self.decks
            .iter()
            .filter_map(|deck| deck.track.as_ref())
            .filter(|playing| playing.started_at_ms.is_some())
            .max_by_key(|playing| playing.started_at_ms)
            .or_else(|| self.decks[self.lead()?].track.as_ref())
    }
}

/// Why a DJ control was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DjError {
    Off,
    Empty,
    NotQueued,
    NoBpm,
    OutOfRange,
    NothingToFadeTo,
}

impl fmt::Display for DjError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "DJ mode is off for this queue.",
            Self::Empty => "That deck has no song.",
            Self::NotQueued => "That song has left the queue.",
            Self::NoBpm => "Sync needs the BPM of both songs.",
            Self::OutOfRange => "The other deck's BPM is beyond this deck's tempo range.",
            Self::NothingToFadeTo => "Load a song into the other deck first.",
        })
    }
}

impl State {
    /// Turns DJ mode on: the current song continues on deck A where it is,
    /// and with auto load the queue's next songs wait on the empty decks.
    fn enter_dj(&mut self, now: fn() -> i64) -> bool {
        if self.dj.is_some() {
            return false;
        }
        let mut dj = Dj::default();
        if let Some(playing) = self.current.take() {
            let deck = &mut dj.decks[0];
            deck.position = playing.frames * FRAME_SAMPLES;
            deck.playing = !self.paused;
            deck.track = Some(playing);
        }
        self.paused = false;
        self.error = None;
        self.dj = Some(dj);
        for deck in 0..DECKS {
            if self
                .dj
                .as_ref()
                .is_some_and(|dj| dj.decks[deck].track.is_none())
            {
                self.load_next(deck, now);
            }
        }
        true
    }

    /// Turns DJ mode off: the lead deck's song continues as the queue's
    /// current song from its position, the other deck's play ends.
    fn leave_dj(&mut self, now: fn() -> i64) -> bool {
        let Some(dj) = self.dj.as_mut() else {
            return false;
        };
        let lead = dj.lead();
        let continued = lead.and_then(|index| {
            let deck = &mut dj.decks[index];
            let playing = deck.playing;
            let position = deck.position;
            let mut song = deck.eject()?;
            song.frames = position / FRAME_SAMPLES;
            song.generation += 1;
            song.buffering = true;
            Some((song, playing))
        });
        for deck in 0..DECKS {
            self.clear_deck(deck, HistoryOutcome::Skipped, None, now);
        }
        self.dj = None;
        self.paused = continued.as_ref().is_some_and(|(_, playing)| !playing);
        self.current = continued.map(|(song, _)| song);
        true
    }

    /// Empties `deck`; its play ends with `outcome`. A queue song the room
    /// has not heard yet goes back to the front of the queue.
    fn clear_deck(
        &mut self,
        deck: usize,
        outcome: HistoryOutcome,
        error: Option<String>,
        now: fn() -> i64,
    ) {
        let Some(dj) = self.dj.as_mut() else {
            return;
        };
        let queued = dj.decks[deck].queued;
        let Some(playing) = dj.decks[deck].eject() else {
            return;
        };
        if queued
            && outcome == HistoryOutcome::Skipped
            && playing.started_at_ms.is_none()
            && self.queue.len() < MAX_QUEUE
        {
            self.queue.push_front(playing.entry);
        } else {
            self.finish_play(playing, outcome, error, now);
        }
    }

    /// Puts `entry` on `deck`, paused at its start.
    fn put_on_deck(&mut self, deck: usize, entry: Entry, queued: bool, now: fn() -> i64) {
        self.clear_deck(deck, HistoryOutcome::Skipped, None, now);
        if let Some(dj) = self.dj.as_mut() {
            let deck = &mut dj.decks[deck];
            deck.track = Some(Playing::new(entry));
            deck.queued = queued;
        }
    }

    /// Loads the queue's next song onto `deck`.
    fn load_next(&mut self, deck: usize, now: fn() -> i64) -> bool {
        let Some(entry) = self.queue.pop_front() else {
            return false;
        };
        self.put_on_deck(deck, entry, true, now);
        true
    }

    /// The end of a deck's decoder; false when it belongs to an older song or seek.
    fn deck_decoded(
        &mut self,
        deck: usize,
        entry: u64,
        generation: u64,
        outcome: Result<(), String>,
        now: fn() -> i64,
        at: Instant,
    ) -> bool {
        let Some(dj) = self.dj.as_ref() else {
            return false;
        };
        let Some(playing) = dj.decks[deck]
            .track
            .as_ref()
            .filter(|playing| playing.entry.id == entry && playing.generation == generation)
        else {
            return false;
        };
        match outcome {
            Ok(()) => self.clear_deck(deck, HistoryOutcome::Completed, None, now),
            Err(error) => {
                let error = format!("{}: {error}", playing.entry.track.title);
                self.clear_deck(deck, HistoryOutcome::Failed, Some(error.clone()), now);
                let Some(dj) = self.dj.as_mut() else {
                    return true;
                };
                dj.failures = dj.failures.saturating_add(1);
                let backoff = BACKOFF
                    .saturating_mul(1 << (dj.failures - 1).min(5))
                    .min(MAX_BACKOFF);
                dj.decks[deck].restart_at = Some(at + backoff);
                dj.decks[deck].error = Some(error);
                if dj.failures >= MAX_FAILURES && dj.auto_load {
                    dj.auto_load = false;
                    dj.notice = Some(format!(
                        "Auto load stopped: {MAX_FAILURES} songs in a row failed to play."
                    ));
                }
            }
        }
        if self.dj.as_ref().is_some_and(|dj| dj.auto_load) {
            self.load_next(deck, now);
        }
        true
    }
}

/// Where a deck's song comes from.
pub enum DeckSong {
    /// A song from a library, added by this person.
    Library(Box<Track>, String),
    /// The queue entry with this ID, which leaves the queue.
    Queued(u64),
}

impl Player {
    /// Turns DJ mode on or off; see [`State::enter_dj`] and [`State::leave_dj`].
    pub fn set_dj(&self, on: bool) {
        self.change(|state| {
            let changed = if on {
                state.enter_dj(wall_clock_ms)
            } else {
                state.leave_dj(wall_clock_ms)
            };
            if changed {
                for cue in &self.cues {
                    cue.reset();
                }
                if !on {
                    self.reset_browser();
                }
            }
        });
    }

    fn dj<R>(&self, change: impl FnOnce(&mut State) -> Result<R, DjError>) -> Result<R, DjError> {
        self.change(|state| {
            if state.dj.is_some() {
                change(state)
            } else {
                Err(DjError::Off)
            }
        })
    }

    fn deck<R>(
        &self,
        deck: usize,
        change: impl FnOnce(&mut Deck) -> Result<R, DjError>,
    ) -> Result<R, DjError> {
        self.dj(|state| change(&mut state.dj.as_mut().unwrap().decks[deck.min(DECKS - 1)]))
    }

    pub fn dj_load(&self, deck: usize, song: DeckSong) -> Result<(), DjError> {
        let deck = deck.min(DECKS - 1);
        self.dj(|state| {
            match song {
                DeckSong::Library(track, added_by) => {
                    let id = state.next_id;
                    state.next_id += 1;
                    let entry = Entry {
                        id,
                        track: *track,
                        added_by,
                    };
                    state.put_on_deck(deck, entry, false, wall_clock_ms);
                }
                DeckSong::Queued(id) => {
                    let position = state
                        .queue
                        .iter()
                        .position(|entry| entry.id == id)
                        .ok_or(DjError::NotQueued)?;
                    let entry = state.queue.remove(position).unwrap();
                    state.put_on_deck(deck, entry, true, wall_clock_ms);
                }
            }
            if let Some(dj) = state.dj.as_mut() {
                dj.decks[deck].error = None;
            }
            self.cues[deck].reset();
            Ok(())
        })
    }

    pub fn dj_eject(&self, deck: usize) -> Result<(), DjError> {
        let deck = deck.min(DECKS - 1);
        self.dj(|state| {
            state.clear_deck(deck, HistoryOutcome::Skipped, None, wall_clock_ms);
            self.cues[deck].reset();
            Ok(())
        })
    }

    pub fn dj_play(&self, deck: usize, playing: bool) -> Result<(), DjError> {
        self.deck(deck, |deck| {
            if deck.track.is_none() {
                return Err(DjError::Empty);
            }
            deck.playing = playing;
            deck.reload = false;
            Ok(())
        })
    }

    pub fn dj_seek(&self, deck: usize, position: Duration) -> Result<(), DjError> {
        let samples = u64::try_from(position.as_millis())
            .unwrap_or(u64::MAX)
            .saturating_mul(SAMPLES_PER_MS);
        self.seek_deck(deck, |_| samples)
    }

    fn seek_deck(&self, deck: usize, position: impl FnOnce(&Deck) -> u64) -> Result<(), DjError> {
        let index = deck.min(DECKS - 1);
        self.deck(index, |deck| deck.seek_to(position(deck)))?;
        self.cues[index].reset();
        Ok(())
    }

    /// Sets the cue point at the deck's position.
    pub fn dj_set_cue(&self, deck: usize) -> Result<(), DjError> {
        self.deck(deck, |deck| {
            if deck.track.is_none() {
                return Err(DjError::Empty);
            }
            deck.cue = deck.position;
            Ok(())
        })
    }

    /// Moves the deck to its cue point.
    pub fn dj_jump_to_cue(&self, deck: usize) -> Result<(), DjError> {
        self.seek_deck(deck, |deck| deck.cue)
    }

    pub fn dj_volume(&self, deck: usize, volume: u8) -> Result<(), DjError> {
        self.deck(deck, |deck| {
            deck.volume = volume.min(100);
            Ok(())
        })
    }

    pub fn dj_eq(&self, deck: usize, band: usize, db: f32) -> Result<(), DjError> {
        self.deck(deck, |deck| {
            deck.eq[band.min(2)] = db.clamp(dsp::EQ_MIN_DB, dsp::EQ_MAX_DB);
            Ok(())
        })
    }

    pub fn dj_filter(&self, deck: usize, filter: f32) -> Result<(), DjError> {
        self.deck(deck, |deck| {
            deck.filter = filter.clamp(-1.0, 1.0);
            Ok(())
        })
    }

    /// Sets the tempo as a fraction, within the deck's range.
    pub fn dj_tempo(&self, deck: usize, tempo: f32) -> Result<(), DjError> {
        self.deck(deck, |deck| {
            let range = f32::from(deck.range) / 100.0;
            deck.tempo = tempo.clamp(-range, range);
            Ok(())
        })
    }

    pub fn dj_tempo_range(&self, deck: usize, range: u8) -> Result<(), DjError> {
        self.deck(deck, |deck| {
            deck.range = if range > TEMPO_RANGES[0] {
                TEMPO_RANGES[1]
            } else {
                TEMPO_RANGES[0]
            };
            let range = f32::from(deck.range) / 100.0;
            deck.tempo = deck.tempo.clamp(-range, range);
            Ok(())
        })
    }

    /// Sets this deck's tempo so its BPM matches the other deck's; widens
    /// the range to 16 % when 8 % is too little.
    pub fn dj_sync(&self, deck: usize) -> Result<(), DjError> {
        let deck = deck.min(DECKS - 1);
        self.dj(|state| {
            let dj = state.dj.as_mut().unwrap();
            let target = dj.decks[1 - deck].bpm().ok_or(DjError::NoBpm)?;
            let own = &mut dj.decks[deck];
            let base = own.base_bpm().ok_or(DjError::NoBpm)?;
            let tempo = (target / f64::from(base) - 1.0) as f32;
            let widest = f32::from(TEMPO_RANGES[1]) / 100.0;
            if tempo.abs() > widest + 1e-6 {
                return Err(DjError::OutOfRange);
            }
            if tempo.abs() > f32::from(own.range) / 100.0 + 1e-6 {
                own.range = TEMPO_RANGES[1];
            }
            own.tempo = tempo.clamp(-widest, widest);
            Ok(())
        })
    }

    /// Moves the crossfader; a running transition stops where it is.
    pub fn dj_crossfader(&self, position: f32) -> Result<(), DjError> {
        self.dj(|state| {
            let dj = state.dj.as_mut().unwrap();
            dj.transition = None;
            dj.crossfader = position.clamp(-1.0, 1.0);
            Ok(())
        })
    }

    pub fn dj_curve(&self, curve: Curve) -> Result<(), DjError> {
        self.dj(|state| {
            state.dj.as_mut().unwrap().curve = curve;
            Ok(())
        })
    }

    /// Fades to the deck the crossfader is away from over `seconds`, starting
    /// that deck when it is paused.
    pub fn dj_transition(&self, seconds: u32) -> Result<(), DjError> {
        self.dj(|state| {
            let dj = state.dj.as_mut().unwrap();
            let to = usize::from(dj.crossfader <= 0.0);
            let deck = &mut dj.decks[to];
            if deck.track.is_none() {
                return Err(DjError::NothingToFadeTo);
            }
            deck.playing = true;
            deck.reload = false;
            let seconds = seconds.clamp(MIN_TRANSITION_SECONDS, MAX_TRANSITION_SECONDS);
            dj.transition = Some(Transition::new(dj.crossfader, to, seconds));
            Ok(())
        })
    }

    pub fn dj_auto_load(&self, on: bool) -> Result<(), DjError> {
        self.dj(|state| {
            let dj = state.dj.as_mut().unwrap();
            dj.auto_load = on;
            if on {
                dj.failures = 0;
                dj.notice = None;
            }
            Ok(())
        })
    }
}

/// What a deck's decoder offers for the next frame.
enum Pull {
    Frame(Vec<f32>),
    Empty,
    End,
    Failed(String),
}

trait Source {
    fn pull(&mut self) -> Pull;
}

impl Source for Decoding {
    fn pull(&mut self) -> Pull {
        match self.frames.try_recv() {
            Ok(Ok(frame)) => Pull::Frame(frame),
            Ok(Err(error)) => Pull::Failed(error),
            Err(TryRecvError::Empty) => Pull::Empty,
            // Whether the song ended well is the decoder's final result.
            Err(TryRecvError::Disconnected) => Pull::End,
        }
    }
}

/// How a deck took part in a frame.
#[derive(Debug, PartialEq)]
enum Rendered {
    /// Paused or empty: nothing.
    Idle,
    /// Playing, but its decoder has no audio yet.
    Starved,
    /// Audio in `cue`, with the deck's mix gain moving from `from` to `to`.
    /// `fading` is the last frame of a song or position the deck just left.
    Audio {
        from: f32,
        to: f32,
        fading: bool,
    },
    Failed(String),
}

/// A deck's signal path in the engine.
struct DeckEngine {
    resampler: Resampler,
    tone: ToneFilter,
    ratio: f64,
    /// 1 while playing, 0 while paused; changes ramp over one frame.
    play: f32,
    fader: Ramp,
    /// The song and seek the buffered audio belongs to.
    bound: Option<(u64, u64)>,
    /// The deck's output before its channel fader, reused every frame.
    cue: Vec<f32>,
}

impl Default for DeckEngine {
    fn default() -> Self {
        Self {
            resampler: Resampler::default(),
            tone: ToneFilter::default(),
            ratio: 1.0,
            play: 0.0,
            fader: Ramp::default(),
            bound: None,
            cue: vec![0.0; FRAME * CHANNELS],
        }
    }
}

impl DeckEngine {
    /// A new decoder for `play` delivers audio from sample `start`; the
    /// deck plays from `position`.
    fn attach(&mut self, play: (u64, u64), start: u64, position: u64, ratio: f64) {
        self.resampler.reset(start, position);
        self.tone.reset();
        self.play = 0.0;
        self.ratio = ratio;
        self.bound = Some(play);
    }

    fn silent(&self) -> bool {
        self.play == 0.0
    }

    /// Buffered audio of its decoder remains to be played.
    fn pending(&self) -> bool {
        self.bound.is_some() && self.resampler.unplayed()
    }

    fn render(&mut self, deck: &Deck, gain: f32, source: Option<&mut dyn Source>) -> Rendered {
        let current = deck
            .track
            .as_ref()
            .map(|playing| (playing.entry.id, playing.generation));
        if self.bound != current {
            // The deck moved on: fade out what it was playing, from what is
            // buffered, then wait for the new decoder.
            // The audio past what is buffered reads as silence, under a gain
            // that is close to zero by then.
            let fading = self.bound.is_some() && self.play > 0.0;
            if !fading {
                self.play = 0.0;
                return if deck.playing {
                    Rendered::Starved
                } else {
                    Rendered::Idle
                };
            }
            self.resampler.render(self.ratio, self.ratio, &mut self.cue);
            self.tone.process(&mut self.cue, deck.tone());
            dsp::scale(&mut self.cue, self.play, 0.0);
            self.play = 0.0;
            self.bound = None;
            let (from, to) = self.fader.step(gain);
            return Rendered::Audio {
                from,
                to,
                fading: true,
            };
        }
        if current.is_none() || (!deck.playing && self.play == 0.0) {
            return Rendered::Idle;
        }
        let from = self.ratio;
        let to = {
            let next = from + (deck.ratio() - from) * 0.35;
            if (deck.ratio() - next).abs() < 1e-6 {
                deck.ratio()
            } else {
                next
            }
        };
        let mut source = source;
        // Pull one frame ahead, so a deck that moves away can still fade out.
        while !self.resampler.ready_ahead(from, to) {
            let pulled = match source.as_deref_mut() {
                Some(source) => source.pull(),
                None => Pull::Empty,
            };
            match pulled {
                Pull::Frame(frame) => self.resampler.push(&frame),
                Pull::Empty if self.resampler.ready(from, to) => break,
                Pull::Empty => return Rendered::Starved,
                Pull::End => self.resampler.end(),
                Pull::Failed(error) => return Rendered::Failed(error),
            }
        }
        self.ratio = to;
        self.resampler.render(from, to, &mut self.cue);
        self.tone.process(&mut self.cue, deck.tone());
        let play = if deck.playing { 1.0 } else { 0.0 };
        dsp::scale(&mut self.cue, self.play, play);
        self.play = play;
        let (from, to) = self.fader.step(gain);
        Rendered::Audio {
            from,
            to,
            fading: false,
        }
    }
}

/// One mixed frame and what it took.
struct Mixed {
    frame: Vec<f32>,
    /// A deck rendered audio into its cue buffer.
    rendered: [bool; DECKS],
    /// The play of each deck heard in this frame (entry, generation).
    heard: [Option<(u64, u64)>; DECKS],
    audio: bool,
    /// Viewers should see the new state.
    changed: bool,
}

/// DJ mode's playback: each deck's decoder and signal path, and the limiter.
#[derive(Default)]
struct Engine {
    decks: [DeckEngine; DECKS],
    slots: [Option<Decoding>; DECKS],
    limiter: Limiter,
}

impl Engine {
    /// Whether DJ mode has anything to play or fade.
    fn active(&self, dj: &Dj) -> bool {
        dj.transition.is_some()
            || (0..DECKS).any(|deck| {
                (dj.decks[deck].track.is_some() && dj.decks[deck].playing)
                    || !self.decks[deck].silent()
            })
    }

    /// Mixes one frame, with the state lock held.
    fn mix(&mut self, state: &mut State, now: fn() -> i64, at: Instant) -> Mixed {
        let mut mixed = Mixed {
            frame: vec![0.0; FRAME * CHANNELS],
            rendered: [false; DECKS],
            heard: [None; DECKS],
            audio: false,
            changed: false,
        };
        let mut failed: [Option<(u64, u64, String)>; DECKS] = [None, None];
        let Some(dj) = state.dj.as_mut() else {
            return mixed;
        };
        if let Some(transition) = dj.transition.as_mut() {
            let (position, done) = transition.advance();
            dj.crossfader = position;
            if done {
                let from = 1 - transition.to;
                dj.transition = None;
                dj.decks[from].playing = false;
                dj.decks[from].reload = dj.auto_load;
                mixed.changed = true;
            }
        }
        let gains = dj.gains();
        let mut started = false;
        for (index, (deck, engine)) in dj.decks.iter_mut().zip(&mut self.decks).enumerate() {
            let play = deck
                .track
                .as_ref()
                .map(|playing| (playing.entry.id, playing.generation));
            let source = self.slots[index]
                .as_mut()
                .filter(|running| {
                    play == Some((running.entry, running.generation)) && !running.retiring()
                })
                .map(|running| running as &mut dyn Source);
            match engine.render(deck, gains[index], source) {
                Rendered::Idle => {}
                Rendered::Starved => {
                    if let Some(playing) = deck.track.as_mut()
                        && !std::mem::replace(&mut playing.buffering, true)
                    {
                        mixed.changed = true;
                    }
                }
                Rendered::Audio { from, to, fading } => {
                    dsp::mix_into(&mut mixed.frame, &engine.cue, from, to);
                    mixed.audio = true;
                    mixed.rendered[index] = true;
                    if fading {
                        continue;
                    }
                    if from.max(to) >= AUDIBLE {
                        mixed.heard[index] = play;
                    }
                    deck.position = engine.resampler.position() as u64;
                    if let Some(playing) = deck.track.as_mut() {
                        playing.frames = deck.position / FRAME_SAMPLES;
                        if std::mem::take(&mut playing.buffering) {
                            mixed.changed = true;
                            started = true;
                        }
                    }
                }
                Rendered::Failed(error) => {
                    if let Some((entry, generation)) = play {
                        failed[index] = Some((entry, generation, error));
                    }
                }
            }
        }
        if started {
            dj.failures = 0;
        }
        if mixed.audio {
            self.limiter.process(&mut mixed.frame);
        }
        for (index, failure) in failed.into_iter().enumerate() {
            if let Some((entry, generation, error)) = failure {
                mixed.changed |= state.deck_decoded(index, entry, generation, Err(error), now, at);
            }
        }
        mixed
    }
}

impl Player {
    /// Plays DJ mode until it is turned off and both deck decoders have
    /// retired. `handover` is the queue's decoder, which continues as deck A's
    /// when deck A has its song; `held` is a frame it already delivered.
    pub(super) async fn run_dj(
        &self,
        out: &mut Output,
        configured_bitrate: u32,
        libraries: &[Library],
        start: &mut impl FnMut(Track, decode::Start) -> Decoding,
        handover: Option<Decoding>,
        held: Option<Vec<f32>>,
    ) {
        let mut engine = Engine::default();
        if let Some(decoding) = handover {
            {
                let state = self.state();
                let deck = state.dj.as_ref().map(|dj| &dj.decks[0]);
                if let Some(deck) = deck
                    && let Some(playing) = deck.track.as_ref()
                    && (playing.entry.id, playing.generation)
                        == (decoding.entry, decoding.generation)
                    && !decoding.retiring()
                {
                    engine.decks[0].attach(
                        (decoding.entry, decoding.generation),
                        playing.frames * FRAME_SAMPLES,
                        deck.position,
                        deck.ratio(),
                    );
                    if let Some(frame) = held {
                        engine.decks[0].resampler.push(&frame);
                    }
                }
            }
            engine.slots[0] = Some(decoding);
        }
        let mut deadline: Option<Instant> = None;
        let mut underrun = 0;
        loop {
            for deck in 0..DECKS {
                self.poll_deck(&mut engine, deck).await;
            }
            if self.state().dj.is_none() {
                if out.talking {
                    self.end(out).await;
                }
                for running in engine.slots.iter_mut().flatten() {
                    if !running.retiring() {
                        running.request_cancel();
                    }
                }
                if engine.slots.iter().all(Option::is_none) {
                    return;
                }
                self.wait_for_retirement().await;
                continue;
            }
            if out.encoder.retirement_pending() && !self.retire_encoder(out).await {
                continue;
            }
            if out.reset_encoder && !self.clear_encoder(out).await {
                continue;
            }
            let due = self.follow_decks(&mut engine, libraries, start);
            let (paused, volume, ducking, active) = {
                let state = self.state();
                let active = state.dj.as_ref().is_some_and(|dj| engine.active(dj));
                (state.paused, state.volume, state.ducking, active)
            };
            let cueing = self.cue_listeners().iter().any(|listeners| *listeners > 0);
            if paused || !active || !(cueing || self.has_output(configured_bitrate)) {
                if out.talking {
                    self.end(out).await;
                }
                deadline = None;
                let wait = due.map_or(Duration::from_millis(500), |due| {
                    due.saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(500))
                });
                tokio::select! {
                    _ = self.wake.notified() => {}
                    _ = sleep(wait) => {}
                }
                continue;
            }
            let next = deadline.get_or_insert_with(Instant::now);
            sleep_until(*next).await;
            *next += TICK;
            let now = Instant::now();
            if *next + TICK * 10 < now {
                *next = now;
            }
            let ducked = self.ducked(&ducking);
            if ducked != out.was_ducked {
                out.was_ducked = ducked;
                self.changes.notify();
            }
            let mixed = {
                let mut state = self.state();
                if state.paused || state.dj.is_none() {
                    continue;
                }
                let mixed = engine.mix(&mut state, wall_clock_ms, now);
                // Under the lock, so a cue reset cannot follow its stale audio.
                for (deck, rendered) in mixed.rendered.iter().enumerate() {
                    if *rendered {
                        self.cues[deck].send(&engine.decks[deck].cue);
                    }
                }
                mixed
            };
            if mixed.changed {
                self.changes.notify();
            }
            if !mixed.audio {
                underrun += 1;
                if underrun >= UNDERRUN_END && out.talking {
                    self.end(out).await;
                }
                continue;
            }
            underrun = 0;
            let bitrate = self.native_bitrate(configured_bitrate);
            if out.talking && bitrate == 0 {
                self.end(out).await;
            }
            let encoded = if bitrate > 0 {
                let target = target_gain(volume, &ducking, ducked);
                let next = next_gain(out.gain, target);
                let mut samples = mixed.frame.clone();
                apply_gain(&mut samples, out.gain, next);
                Some((out.encoder.encode(samples, bitrate).await, next))
            } else {
                None
            };
            let backoff = self.send_mix(out, configured_bitrate, bitrate, encoded, &mixed);
            if let Some(backoff) = backoff {
                self.changes.notify();
                sleep(backoff).await;
            }
        }
    }

    /// Sends a mixed frame to the room and credits the decks it heard;
    /// returns the pause after an encoder failure.
    fn send_mix(
        &self,
        out: &mut Output,
        configured_bitrate: u32,
        bitrate: u32,
        encoded: Option<(Result<Vec<u8>, String>, f32)>,
        mixed: &Mixed,
    ) -> Option<Duration> {
        let mut state = self.state();
        if state.paused
            || state.dj.is_none()
            || !self.has_output(configured_bitrate)
            || (encoded.is_some() && self.native_bitrate(configured_bitrate) < bitrate)
        {
            out.reset_encoder |= encoded.is_some();
            return None;
        }
        let opus = match encoded {
            Some((Ok(opus), next)) => {
                out.last_bitrate = bitrate;
                out.gain = next;
                if let Some(dj) = state.dj.as_mut() {
                    dj.encode_failures = 0;
                }
                Some(opus)
            }
            Some((Err(error), _)) => return Some(self.encode_failed(&mut state, error)),
            None => None,
        };
        let (browser, native) = self.send_room(&mixed.frame, opus);
        out.talking |= native;
        if (browser || native)
            && let Some(dj) = state.dj.as_mut()
        {
            for (deck, heard) in dj.decks.iter_mut().zip(mixed.heard) {
                if let Some(playing) = deck.track.as_mut()
                    && heard == Some((playing.entry.id, playing.generation))
                {
                    playing.record_audio(wall_clock_ms);
                }
            }
        }
        None
    }

    /// An encoder failure costs its frame; after several in a row DJ mode
    /// pauses and says why. Returns the pause before the next attempt.
    fn encode_failed(&self, state: &mut State, error: String) -> Duration {
        tracing::warn!("{}: DJ mix encoding failed: {error}", self.name);
        let Some(dj) = state.dj.as_mut() else {
            return BACKOFF;
        };
        dj.encode_failures = dj.encode_failures.saturating_add(1);
        let failures = dj.encode_failures;
        if failures >= MAX_ENCODE_FAILURES {
            dj.encode_failures = 0;
            dj.notice = Some(format!(
                "DJ mode paused: encoding failed {MAX_ENCODE_FAILURES} times in a row ({error})."
            ));
            state.paused = true;
            self.reset_browser();
        }
        BACKOFF
            .saturating_mul(1 << (failures - 1).min(4))
            .min(MAX_BACKOFF)
    }

    /// Reports a deck decoder's end and frees its slot once it has retired.
    async fn poll_deck(&self, engine: &mut Engine, deck: usize) {
        let pending = engine.decks[deck].pending();
        let Some(running) = engine.slots[deck].as_mut() else {
            return;
        };
        if let Some(outcome) = running.poll_outcome(pending).await {
            let (entry, generation) = (running.entry, running.generation);
            running.request_cancel();
            let mut state = self.state();
            if state.deck_decoded(
                deck,
                entry,
                generation,
                outcome,
                wall_clock_ms,
                Instant::now(),
            ) {
                self.cues[deck].reset();
                drop(state);
                self.changes.notify();
            }
        }
        if running.retiring() && running.retired() {
            engine.slots[deck] = None;
        }
    }

    /// Keeps, retires or starts each deck's decoder so that it plays the
    /// deck's song from its position, one decoder per deck at most. Returns
    /// when a start that waits after a failure is due.
    fn follow_decks(
        &self,
        engine: &mut Engine,
        libraries: &[Library],
        start: &mut impl FnMut(Track, decode::Start) -> Decoding,
    ) -> Option<Instant> {
        let now = Instant::now();
        let mut due: Option<Instant> = None;
        for index in 0..DECKS {
            {
                let mut state = self.state();
                if state.dj.as_ref().is_some_and(|dj| dj.decks[index].reload)
                    && engine.decks[index].silent()
                {
                    if let Some(dj) = state.dj.as_mut() {
                        dj.decks[index].reload = false;
                    }
                    if state.load_next(index, wall_clock_ms) {
                        self.cues[index].reset();
                    }
                    drop(state);
                    self.changes.notify();
                }
            }
            let wanted = {
                let state = self.state();
                state.dj.as_ref().and_then(|dj| {
                    let deck = &dj.decks[index];
                    let playing = deck.track.as_ref()?;
                    Some((playing.entry.id, playing.generation, deck.restart_at))
                })
            };
            if let Some(running) = engine.slots[index].as_mut() {
                if wanted.is_some_and(|(entry, generation, _)| {
                    (entry, generation) == (running.entry, running.generation)
                }) && !running.retiring()
                {
                    continue;
                }
                if !running.retiring() {
                    running.request_cancel();
                }
                if !running.retired() {
                    continue;
                }
                engine.slots[index] = None;
            }
            let Some((_, _, restart_at)) = wanted else {
                continue;
            };
            if let Some(at) = restart_at
                && at > now
            {
                due = Some(due.map_or(at, |due| due.min(at)));
                continue;
            }
            let request = {
                let mut state = self.state();
                state.dj.as_mut().and_then(|dj| {
                    let deck = &mut dj.decks[index];
                    deck.restart_at = None;
                    let (position, ratio) = (deck.position, deck.ratio());
                    let playing = deck.track.as_mut()?;
                    if playing.source.is_none() {
                        playing.source = libraries
                            .get(playing.entry.track.library)
                            .map(Library::identity);
                    }
                    let request = decode::Start {
                        entry: playing.entry.id,
                        generation: playing.generation,
                        frames: position / FRAME_SAMPLES,
                    };
                    Some((playing.entry.track.clone(), request, position, ratio))
                })
            };
            if let Some((track, request, position, ratio)) = request {
                let play = (request.entry, request.generation);
                let first = request.frames * FRAME_SAMPLES;
                engine.slots[index] = Some(start(track, request));
                engine.decks[index].attach(play, first, position, ratio);
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode::tests::controlled, player::Changes};
    use std::sync::{Arc, Mutex};
    use tokio::sync::{mpsc, oneshot};

    fn track(id: &str, bpm: Option<u32>) -> Track {
        Track {
            library: 0,
            id: id.into(),
            title: id.into(),
            artist: None,
            album: None,
            album_id: None,
            artist_id: None,
            duration: Some(300),
            track: None,
            suffix: None,
            content_type: None,
            cover_art: None,
            bpm,
        }
    }

    fn player() -> Arc<Player> {
        Arc::new(Player::new(
            "test".into(),
            100,
            super::super::Ducking::default(),
            Changes::default(),
        ))
    }

    fn now() -> i64 {
        1_000
    }

    fn deck_ids(player: &Player) -> [Option<String>; DECKS] {
        let state = player.state();
        let dj = state.dj.as_ref().unwrap();
        [0, 1].map(|deck| {
            dj.decks[deck]
                .track
                .as_ref()
                .map(|playing| playing.entry.track.id.clone())
        })
    }

    fn queued(player: &Player) -> Vec<String> {
        player
            .state()
            .queue
            .iter()
            .map(|entry| entry.track.id.clone())
            .collect()
    }

    /// A controlled decoder for a deck's song, and the sender of its frames.
    struct Fed {
        sender: mpsc::Sender<decode::Frame>,
        _terminal: oneshot::Sender<Result<(), String>>,
        finish: oneshot::Sender<Result<(), String>>,
    }

    fn feed(engine: &mut Engine, state: &State, deck: usize) -> Fed {
        let mut controlled = controlled();
        let dj = state.dj.as_ref().unwrap();
        let playing = dj.decks[deck].track.as_ref().unwrap();
        controlled.decoding.entry = playing.entry.id;
        controlled.decoding.generation = playing.generation;
        let first = dj.decks[deck].position / FRAME_SAMPLES * FRAME_SAMPLES;
        engine.decks[deck].attach(
            (playing.entry.id, playing.generation),
            first,
            dj.decks[deck].position,
            dj.decks[deck].ratio(),
        );
        engine.slots[deck] = Some(controlled.decoding);
        Fed {
            sender: controlled.sender,
            _terminal: controlled.terminal,
            finish: controlled.finish,
        }
    }

    /// Sends `frames` frames of a continuous 1 kHz tone at `level`.
    fn send_tone(sender: &mpsc::Sender<decode::Frame>, level: f32, frames: usize) {
        for frame in 0..frames {
            let samples = (0..FRAME)
                .flat_map(|index| {
                    let at = (frame * FRAME + index) as f32 / 48.0;
                    [(std::f32::consts::TAU * at).sin() * level; CHANNELS]
                })
                .collect();
            sender.try_send(Ok(samples)).unwrap();
        }
    }

    fn peak(samples: &[f32]) -> f32 {
        samples
            .iter()
            .fold(0.0, |peak, sample| peak.max(sample.abs()))
    }

    #[test]
    fn a_transition_moves_the_crossfader_linearly_over_its_time() {
        let mut transition = Transition::new(-1.0, 1, 2);
        assert_eq!(transition.frames, 100);
        assert_eq!(transition.duration_ms(), 2_000);
        let mut position = -1.0;
        for frame in 1..=100 {
            let (next, done) = transition.advance();
            assert!(next > position);
            position = next;
            if frame == 50 {
                assert!(next.abs() < 1e-6);
                assert_eq!(transition.elapsed_ms(), 1_000);
            }
            assert_eq!(done, frame == 100);
        }
        assert_eq!(position, 1.0);
        // From halfway back to A.
        let mut back = Transition::new(0.5, 0, 1);
        for _ in 0..25 {
            back.advance();
        }
        assert!((back.advance().0 - (0.5 - 1.5 * 26.0 / 50.0)).abs() < 1e-6);
    }

    #[test]
    fn entering_continues_the_current_song_on_deck_a_and_loads_the_queue() {
        let player = player();
        player.enqueue(
            vec![track("a", None), track("b", None), track("c", None)],
            "ada",
            None,
        );
        let (entry, generation) = {
            let mut state = player.state();
            assert!(state.start_next(|_| None));
            let playing = state.current.as_mut().unwrap();
            playing.frames = 250;
            (playing.entry.id, playing.generation)
        };
        player.set_dj(true);
        assert_eq!(deck_ids(&player), [Some("a".into()), Some("b".into())]);
        assert_eq!(queued(&player), ["c"]);
        let state = player.state();
        let dj = state.dj.as_ref().unwrap();
        let a = &dj.decks[0];
        assert!(a.playing && !dj.decks[1].playing);
        assert_eq!(a.position, 250 * FRAME_SAMPLES);
        assert_eq!(a.position_ms(), 5_000);
        // The same play and decoder generation: the decoder carries on.
        let playing = a.track.as_ref().unwrap();
        assert_eq!((playing.entry.id, playing.generation), (entry, generation));
        assert!(state.current.is_none());
        assert_eq!(dj.crossfader, -1.0);
    }

    #[test]
    fn leaving_continues_the_lead_deck_and_returns_unheard_queue_songs() {
        let player = player();
        player.enqueue(
            vec![track("a", None), track("b", None), track("c", None)],
            "ada",
            None,
        );
        player.set_dj(true);
        player.dj_play(1, true).unwrap();
        player.dj_crossfader(1.0).unwrap();
        {
            let mut state = player.state();
            let deck = &mut state.dj.as_mut().unwrap().decks[1];
            deck.position = 480_000;
            deck.track.as_mut().unwrap().started_at_ms = Some(5);
        }
        player.set_dj(false);
        let state = player.state();
        assert!(state.dj.is_none());
        let current = state.current.as_ref().unwrap();
        assert_eq!(current.entry.track.id, "b");
        assert_eq!(current.frames, 500);
        assert_eq!(
            current.generation, 1,
            "the decoder restarts at the position"
        );
        assert_eq!(current.started_at_ms, Some(5), "one play continues");
        assert!(!state.paused);
        // Deck A's song was never heard and goes back to the queue.
        let queue: Vec<_> = state.queue.iter().map(|entry| &*entry.track.id).collect();
        assert_eq!(queue, ["a", "c"]);
        assert!(state.history.is_empty());
    }

    #[test]
    fn controls_validate_state_and_clamp() {
        let player = player();
        assert_eq!(player.dj_volume(0, 50), Err(DjError::Off));
        player.set_dj(true);
        assert_eq!(player.dj_play(0, true), Err(DjError::Empty));
        assert_eq!(player.dj_transition(5), Err(DjError::NothingToFadeTo));
        assert_eq!(
            player.dj_load(0, DeckSong::Queued(99)),
            Err(DjError::NotQueued)
        );
        player
            .dj_load(
                0,
                DeckSong::Library(Box::new(track("x", Some(120))), "ada".into()),
            )
            .unwrap();
        player.dj_tempo(0, 0.5).unwrap();
        player.dj_eq(0, 1, -100.0).unwrap();
        player.dj_filter(0, 3.0).unwrap();
        player.dj_volume(0, 250).unwrap();
        {
            let state = player.state();
            let deck = &state.dj.as_ref().unwrap().decks[0];
            assert_eq!(deck.tempo, 0.08);
            assert_eq!(deck.eq[1], dsp::EQ_MIN_DB);
            assert_eq!(deck.filter, 1.0);
            assert_eq!(deck.volume, 100);
            assert_eq!(deck.track.as_ref().unwrap().entry.added_by, "ada");
        }
        player.dj_tempo_range(0, 16).unwrap();
        player.dj_tempo(0, 0.12).unwrap();
        player.dj_tempo_range(0, 8).unwrap();
        assert_eq!(player.state().dj.as_ref().unwrap().decks[0].tempo, 0.08);
    }

    #[test]
    fn sync_matches_the_other_decks_tempo_adjusted_bpm() {
        let player = player();
        player.set_dj(true);
        player
            .dj_load(
                0,
                DeckSong::Library(Box::new(track("a", Some(120))), "ada".into()),
            )
            .unwrap();
        assert_eq!(player.dj_sync(0), Err(DjError::NoBpm));
        player
            .dj_load(
                1,
                DeckSong::Library(Box::new(track("b", Some(125))), "ada".into()),
            )
            .unwrap();
        player.dj_tempo(1, 0.04).unwrap();
        player.dj_sync(0).unwrap();
        {
            let state = player.state();
            let dj = state.dj.as_ref().unwrap();
            assert!((dj.decks[0].bpm().unwrap() - 130.0).abs() < 1e-3);
            // 130 / 120 needs more than 8 %: the range widens.
            assert_eq!(dj.decks[0].range, 16);
        }
        player
            .dj_load(
                1,
                DeckSong::Library(Box::new(track("c", Some(174))), "ada".into()),
            )
            .unwrap();
        assert_eq!(player.dj_sync(0), Err(DjError::OutOfRange));
    }

    #[test]
    fn seeking_counts_song_time_whatever_the_tempo() {
        let player = player();
        player.set_dj(true);
        player
            .dj_load(
                0,
                DeckSong::Library(Box::new(track("a", None)), "ada".into()),
            )
            .unwrap();
        player.dj_tempo(0, -0.08).unwrap();
        player.dj_seek(0, Duration::from_millis(10_010)).unwrap();
        player.dj_set_cue(0).unwrap();
        let state = player.state();
        let deck = &state.dj.as_ref().unwrap().decks[0];
        assert_eq!(deck.position, 480_480);
        assert_eq!(deck.cue_ms(), 10_010);
        let playing = deck.track.as_ref().unwrap();
        assert_eq!(playing.frames, 500, "the decoder skips whole frames");
        assert_eq!(playing.generation, 1);
    }

    #[tokio::test]
    async fn position_follows_the_tempo_and_starts_mid_frame() {
        let player = player();
        player.set_dj(true);
        player
            .dj_load(
                0,
                DeckSong::Library(Box::new(track("a", None)), "ada".into()),
            )
            .unwrap();
        player.dj_seek(0, Duration::from_millis(1_005)).unwrap();
        player.dj_tempo(0, 0.08).unwrap();
        player.dj_play(0, true).unwrap();
        let mut engine = Engine::default();
        let fed = feed(&mut engine, &player.state(), 0);
        send_tone(&fed.sender, 0.25, 80);
        for _ in 0..50 {
            let mixed = engine.mix(&mut player.state(), now, Instant::now());
            assert!(mixed.audio);
            assert_eq!(mixed.frame.len(), FRAME * CHANNELS);
        }
        let state = player.state();
        let deck = &state.dj.as_ref().unwrap().decks[0];
        let expected = 48_240.0 + 50.0 * FRAME as f64 * 1.08;
        assert!(
            (deck.position as f64 - expected).abs() <= 1.0,
            "{} vs {expected}",
            deck.position
        );
        assert_eq!(
            deck.track.as_ref().unwrap().frames,
            deck.position / FRAME_SAMPLES
        );
        drop(fed.finish);
    }

    #[tokio::test]
    async fn the_crossfader_decides_which_deck_the_room_hears() {
        let player = player();
        player.enqueue(vec![track("a", None), track("b", None)], "ada", None);
        player.set_dj(true);
        player.dj_play(0, true).unwrap();
        player.dj_play(1, true).unwrap();
        let mut engine = Engine::default();
        let fed = [0, 1].map(|deck| feed(&mut engine, &player.state(), deck));
        for sender in &fed {
            send_tone(&sender.sender, 0.5, 80);
        }
        let mut mixed = engine.mix(&mut player.state(), now, Instant::now());
        for _ in 0..30 {
            mixed = engine.mix(&mut player.state(), now, Instant::now());
        }
        let a = player.state().dj.as_ref().unwrap().decks[0]
            .track
            .as_ref()
            .map(|playing| (playing.entry.id, playing.generation));
        assert_eq!(mixed.heard, [a, None]);
        assert!(mixed.rendered[1], "deck B still feeds its cue");
        assert!(
            (peak(&mixed.frame) - 0.5).abs() < 5e-3,
            "{}",
            peak(&mixed.frame)
        );
        assert!(
            (peak(&engine.decks[1].cue) - 0.5).abs() < 5e-3,
            "the cue is taken before the faders"
        );
        player.dj_crossfader(0.0).unwrap();
        for _ in 0..30 {
            mixed = engine.mix(&mut player.state(), now, Instant::now());
        }
        assert!(mixed.heard.iter().all(Option::is_some));
        // Both at −3 dB: 0.5 · √2 = 0.707.
        let both = peak(&mixed.frame);
        assert!((both - 0.707).abs() < 5e-3, "{both}");
        for fed in fed {
            drop(fed.finish);
        }
    }

    #[tokio::test]
    async fn a_finished_transition_pauses_the_old_deck_and_loads_the_next_song() {
        let player = player();
        player.enqueue(
            vec![track("a", None), track("b", None), track("c", None)],
            "ada",
            None,
        );
        player.set_dj(true);
        player.dj_play(0, true).unwrap();
        player.state().dj.as_mut().unwrap().decks[0]
            .track
            .as_mut()
            .unwrap()
            .started_at_ms = Some(7);
        let mut engine = Engine::default();
        let fed = [0, 1].map(|deck| feed(&mut engine, &player.state(), deck));
        for sender in &fed {
            send_tone(&sender.sender, 0.3, 120);
        }
        player.dj_transition(1).unwrap();
        assert!(player.state().dj.as_ref().unwrap().decks[1].playing);
        for _ in 0..50 {
            engine.mix(&mut player.state(), now, Instant::now());
        }
        {
            let state = player.state();
            let dj = state.dj.as_ref().unwrap();
            assert!(dj.transition.is_none());
            assert_eq!(dj.crossfader, 1.0);
            assert!(!dj.decks[0].playing);
            assert!(dj.decks[0].reload);
        }
        // The old deck fades out within a frame, then the queue's next song loads.
        engine.mix(&mut player.state(), now, Instant::now());
        assert!(engine.decks[0].silent());
        let start = &mut |_: Track, _: decode::Start| controlled().decoding;
        player.follow_decks(&mut engine, &[], start);
        assert_eq!(deck_ids(&player), [Some("c".into()), Some("b".into())]);
        let state = player.state();
        assert_eq!(state.history.len(), 1);
        assert_eq!(state.history[0].entry.track.id, "a");
        assert_eq!(state.history[0].outcome, HistoryOutcome::Skipped);
        assert!(state.queue.is_empty());
    }

    #[test]
    fn a_finished_song_is_history_and_auto_load_takes_the_next() {
        let player = player();
        player.enqueue(
            vec![track("a", None), track("b", None), track("c", None)],
            "ada",
            None,
        );
        player.set_dj(true);
        let (entry, generation) = {
            let mut state = player.state();
            let playing = state.dj.as_mut().unwrap().decks[0].track.as_mut().unwrap();
            playing.started_at_ms = Some(3);
            playing.played_ms = 200_000;
            (playing.entry.id, playing.generation)
        };
        let mut state = player.state();
        assert!(!state.deck_decoded(0, entry, generation + 1, Ok(()), now, Instant::now()));
        assert!(state.deck_decoded(0, entry, generation, Ok(()), now, Instant::now()));
        assert_eq!(state.history[0].outcome, HistoryOutcome::Completed);
        assert_eq!(state.history[0].played_ms, 200_000);
        drop(state);
        assert_eq!(deck_ids(&player), [Some("c".into()), Some("b".into())]);
    }

    #[test]
    fn repeated_failures_back_off_and_stop_auto_load() {
        let player = player();
        let songs = (0..6).map(|n| track(&n.to_string(), None)).collect();
        player.enqueue(songs, "ada", None);
        player.set_dj(true);
        let at = Instant::now();
        let mut waits = Vec::new();
        for _ in 0..MAX_FAILURES {
            let mut state = player.state();
            let playing = state.dj.as_ref().unwrap().decks[0].track.clone().unwrap();
            assert!(state.deck_decoded(
                0,
                playing.entry.id,
                playing.generation,
                Err("decoder crashed".into()),
                now,
                at,
            ));
            let deck = &state.dj.as_ref().unwrap().decks[0];
            waits.push(deck.restart_at.unwrap() - at);
            assert!(deck.error.as_ref().unwrap().ends_with("decoder crashed"));
        }
        assert_eq!(
            waits,
            [
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2)
            ]
        );
        let state = player.state();
        let dj = state.dj.as_ref().unwrap();
        assert!(!dj.auto_load);
        assert!(dj.notice.as_ref().unwrap().contains("Auto load stopped"));
        assert!(
            dj.decks[0].track.is_none(),
            "nothing loads by itself any more"
        );
        assert_eq!(state.history.len(), 3);
        assert!(
            state
                .history
                .iter()
                .all(|item| item.outcome == HistoryOutcome::Failed)
        );
    }

    #[tokio::test]
    async fn a_seek_on_a_playing_deck_fades_out_before_it_waits() {
        let player = player();
        player.enqueue(vec![track("a", None)], "ada", None);
        player.set_dj(true);
        player.dj_play(0, true).unwrap();
        let mut engine = Engine::default();
        let fed = feed(&mut engine, &player.state(), 0);
        send_tone(&fed.sender, 0.5, 40);
        for _ in 0..25 {
            engine.mix(&mut player.state(), now, Instant::now());
        }
        player.dj_seek(0, Duration::from_secs(30)).unwrap();
        let mixed = engine.mix(&mut player.state(), now, Instant::now());
        assert!(mixed.audio);
        let (first, last) = (
            peak(&mixed.frame[..96]),
            peak(&mixed.frame[mixed.frame.len() - 96..]),
        );
        assert!(first > 0.4 && last < 0.03, "{first} … {last}");
        assert_eq!(
            player.state().dj.as_ref().unwrap().decks[0].position,
            1_440_000,
            "the fading frame leaves the new position alone"
        );
        let mixed = engine.mix(&mut player.state(), now, Instant::now());
        assert!(!mixed.audio);
        drop(fed.finish);
    }

    /// Records the decoders the player starts; each is a controlled one.
    #[derive(Clone, Default)]
    struct Starts(Arc<Mutex<Vec<(u64, u64, Fed)>>>);

    impl Starts {
        fn starter(&self) -> impl FnMut(Track, decode::Start) -> Decoding + use<> {
            let starts = self.0.clone();
            move |_, request| {
                let mut controlled = controlled();
                controlled.decoding.entry = request.entry;
                controlled.decoding.generation = request.generation;
                starts.lock().unwrap().push((
                    request.entry,
                    request.generation,
                    Fed {
                        sender: controlled.sender,
                        _terminal: controlled.terminal,
                        finish: controlled.finish,
                    },
                ));
                controlled.decoding
            }
        }

        fn count(&self) -> usize {
            self.0.lock().unwrap().len()
        }
    }

    async fn settle() {
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn two_decks_cost_two_decoders_that_retire_before_the_queue_resumes() {
        let player = player();
        player.enqueue(
            vec![track("a", None), track("b", None), track("c", None)],
            "ada",
            None,
        );
        let _listener = player.subscribe_browser(super::super::Listen::Room);
        let starts = Starts::default();
        let output = Output::new(
            super::super::Client::new(
                super::super::Role::Encode,
                "/nonexistent/nuschl-musicbot-codec-worker".into(),
            ),
            96_000,
        );
        let task = tokio::spawn(player.clone().play(
            Arc::new(Vec::new()),
            96_000,
            output,
            starts.starter(),
        ));
        settle().await;
        assert_eq!(starts.count(), 1, "the queue plays a");
        player.set_dj(true);
        settle().await;
        // Deck A continues with the queue's decoder; deck B gets its own.
        assert_eq!(starts.count(), 2);
        assert_eq!(deck_ids(&player), [Some("a".into()), Some("b".into())]);
        player.dj_seek(1, Duration::from_secs(10)).unwrap();
        settle().await;
        assert_eq!(
            starts.count(),
            2,
            "deck B's old decoder keeps its slot until it has retired"
        );
        let first_b = starts.0.lock().unwrap().remove(1);
        first_b.2.finish.send(Ok(())).unwrap();
        settle().await;
        assert_eq!(starts.count(), 2, "one replacement for deck B");
        player.set_dj(false);
        settle().await;
        assert_eq!(
            starts.count(),
            2,
            "the queue waits until both deck decoders retire"
        );
        let decks: Vec<_> = starts.0.lock().unwrap().drain(..).collect();
        for (_, _, fed) in decks {
            fed.finish.send(Ok(())).unwrap();
        }
        settle().await;
        assert_eq!(starts.count(), 1, "then exactly one queue decoder starts");
        assert!(player.state().current.is_some());
        task.abort();
    }
}

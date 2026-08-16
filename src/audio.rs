use std::{
    fs::File,
    io::BufReader,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, unbounded};
use rodio::{
    Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source,
    cpal::traits::{DeviceTrait, HostTrait},
    queue::{SourcesQueueInput, queue},
};

use crate::{
    model::{PlaybackState, TrackId},
    spectrum::{SPECTRUM_BANDS, SpectrumSource, SpectrumTap},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaSource {
    LocalFile(PathBuf),
}

#[derive(Clone, Debug)]
pub struct StagedTrack {
    pub source: MediaSource,
    pub track_id: TrackId,
    pub duration: Duration,
    pub gain: f32,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioEvent {
    TrackStarted { track_id: TrackId, generation: u64 },
    TrackFinished { track_id: TrackId, generation: u64 },
    DeviceLost(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct AudioSnapshot {
    pub state: PlaybackState,
    pub track_id: Option<TrackId>,
    pub position: Duration,
    pub duration: Duration,
    pub volume: f32,
    pub active_device: String,
    pub active_device_id: Option<String>,
    pub staged_track_id: Option<TrackId>,
    pub generation: u64,
    pub error: Option<String>,
}

impl Default for AudioSnapshot {
    fn default() -> Self {
        Self {
            state: PlaybackState::Stopped,
            track_id: None,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            volume: 0.8,
            active_device: "Default".into(),
            active_device_id: None,
            staged_track_id: None,
            generation: 0,
            error: None,
        }
    }
}

pub trait AudioEngine {
    fn load_and_play(&mut self, track: StagedTrack) -> Result<()>;
    fn stage_next(&mut self, track: StagedTrack) -> Result<()>;
    fn clear_staged(&mut self);
    fn set_pending(&mut self, track_id: TrackId, duration: Duration, state: PlaybackState);
    fn play(&mut self) -> Result<()>;
    fn pause(&mut self);
    fn stop(&mut self);
    fn seek(&mut self, position: Duration) -> Result<()>;
    fn set_volume(&mut self, volume: f32);
    fn set_output_gain(&mut self, gain: f32);
    fn snapshot(&self) -> AudioSnapshot;
    fn drain_events(&mut self) -> Vec<AudioEvent>;
    #[cfg(test)]
    fn simulate_staged_transition(&mut self) {}
    fn spectrum(&self) -> [f32; SPECTRUM_BANDS] {
        [0.0; SPECTRUM_BANDS]
    }
}

pub fn output_devices() -> Vec<AudioDevice> {
    let Ok(devices) = rodio::cpal::default_host().output_devices() else {
        return Vec::new();
    };
    let mut result: Vec<_> = devices
        .filter_map(|device| {
            Some(AudioDevice {
                id: device.id().ok()?.to_string(),
                name: device.description().ok()?.name().to_owned(),
            })
        })
        .collect();
    result.sort_by_key(|device| device.name.to_lowercase());
    result.dedup_by(|left, right| left.id == right.id);
    result
}

pub fn create_engine(
    no_audio: bool,
    volume: f32,
    preferred_device: Option<&str>,
) -> Result<Box<dyn AudioEngine>> {
    if no_audio {
        return Ok(Box::new(SilentEngine::new(volume)));
    }
    Ok(Box::new(RodioEngine::new(volume, preferred_device)?))
}

struct TrackClock {
    samples: AtomicU64,
    seek_micros: AtomicU64,
    channels: u64,
    sample_rate: u64,
}

impl TrackClock {
    fn new(channels: u64, sample_rate: u64) -> Self {
        Self {
            samples: AtomicU64::new(0),
            seek_micros: AtomicU64::new(0),
            channels: channels.max(1),
            sample_rate: sample_rate.max(1),
        }
    }

    fn position(&self) -> Duration {
        let micros = self.seek_micros.load(Ordering::Relaxed);
        let samples = self.samples.load(Ordering::Relaxed);
        let sample_micros = samples
            .saturating_mul(1_000_000)
            .checked_div(self.channels.saturating_mul(self.sample_rate))
            .unwrap_or(0);
        Duration::from_micros(micros.saturating_add(sample_micros))
    }

    fn seeked_to(&self, position: Duration) {
        self.samples.store(0, Ordering::Relaxed);
        self.seek_micros.store(
            position.as_micros().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }
}

struct SignalledSource<S> {
    inner: S,
    track_id: TrackId,
    generation: u64,
    events: Sender<AudioEvent>,
    clock: Arc<TrackClock>,
    started: bool,
    finished: bool,
}

struct GainSource<S> {
    inner: S,
    gain_bits: Arc<AtomicU64>,
}

impl<S> GainSource<S> {
    fn new(inner: S, gain: f32) -> (Self, Arc<AtomicU64>) {
        let gain_bits = Arc::new(AtomicU64::new(u64::from(gain.max(0.0).to_bits())));
        (
            Self {
                inner,
                gain_bits: gain_bits.clone(),
            },
            gain_bits,
        )
    }
}

impl<S: Source> Iterator for GainSource<S> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let gain = f32::from_bits(self.gain_bits.load(Ordering::Relaxed) as u32);
        self.inner.next().map(|sample| sample * gain)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S: Source> Source for GainSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(
        &mut self,
        position: Duration,
    ) -> std::result::Result<(), rodio::source::SeekError> {
        self.inner.try_seek(position)
    }
}

impl<S> SignalledSource<S> {
    fn new(
        inner: S,
        track_id: TrackId,
        generation: u64,
        events: Sender<AudioEvent>,
        clock: Arc<TrackClock>,
    ) -> Self {
        Self {
            inner,
            track_id,
            generation,
            events,
            clock,
            started: false,
            finished: false,
        }
    }
}

impl<S: Source> Iterator for SignalledSource<S> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next();
        match sample {
            Some(sample) => {
                if !self.started {
                    self.started = true;
                    let _ = self.events.send(AudioEvent::TrackStarted {
                        track_id: self.track_id,
                        generation: self.generation,
                    });
                }
                self.clock.samples.fetch_add(1, Ordering::Relaxed);
                Some(sample)
            }
            None => {
                if self.started && !self.finished {
                    self.finished = true;
                    let _ = self.events.send(AudioEvent::TrackFinished {
                        track_id: self.track_id,
                        generation: self.generation,
                    });
                }
                None
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S: Source> Source for SignalledSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(
        &mut self,
        position: Duration,
    ) -> std::result::Result<(), rodio::source::SeekError> {
        self.inner.try_seek(position)
    }
}

pub struct RodioEngine {
    _device_sink: MixerDeviceSink,
    player: Player,
    queue: Arc<SourcesQueueInput>,
    snapshot: AudioSnapshot,
    events_tx: Sender<AudioEvent>,
    events_rx: Receiver<AudioEvent>,
    stream_errors: Receiver<String>,
    current_clock: Option<Arc<TrackClock>>,
    staged_clock: Option<Arc<TrackClock>>,
    current_gain: Option<Arc<AtomicU64>>,
    staged_gain: Option<Arc<AtomicU64>>,
    staged_duration: Option<Duration>,
    staged_generation: Option<u64>,
    spectrum: SpectrumTap,
}

impl RodioEngine {
    pub fn new(volume: f32, preferred_device: Option<&str>) -> Result<Self> {
        let (mut device_sink, active_device, active_device_id, stream_errors) =
            open_output(preferred_device)?;
        device_sink.log_on_drop(false);
        let player = Player::connect_new(device_sink.mixer());
        player.set_volume(volume.clamp(0.0, 1.0));
        let (events_tx, events_rx) = unbounded();
        let (queue, output) = queue(true);
        player.append(output);
        Ok(Self {
            _device_sink: device_sink,
            player,
            queue,
            snapshot: AudioSnapshot {
                volume: volume.clamp(0.0, 1.0),
                active_device,
                active_device_id: Some(active_device_id),
                ..AudioSnapshot::default()
            },
            events_tx,
            events_rx,
            stream_errors,
            current_clock: None,
            staged_clock: None,
            current_gain: None,
            staged_gain: None,
            staged_duration: None,
            staged_generation: None,
            spectrum: SpectrumTap::new(),
        })
    }

    fn reset_queue(&mut self) {
        self.player.clear();
        let (queue, output) = queue(true);
        self.queue = queue;
        self.player.append(output);
    }

    fn append_track(&self, track: &StagedTrack) -> Result<(Arc<TrackClock>, Arc<AtomicU64>)> {
        let MediaSource::LocalFile(path) = &track.source;
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let byte_len = file.metadata().ok().map(|metadata| metadata.len());
        let mut builder = Decoder::builder()
            .with_data(BufReader::new(file))
            .with_gapless(true);
        if let Some(byte_len) = byte_len {
            builder = builder.with_byte_len(byte_len);
        }
        let decoder = builder
            .build()
            .with_context(|| format!("decoding {}", path.display()))?;
        let clock = Arc::new(TrackClock::new(
            u64::from(decoder.channels().get()),
            u64::from(decoder.sample_rate().get()),
        ));
        let (source, gain) = GainSource::new(decoder, track.gain);
        let source = SpectrumSource::new(source, self.spectrum.clone());
        self.queue.append(SignalledSource::new(
            source,
            track.track_id,
            track.generation,
            self.events_tx.clone(),
            clock.clone(),
        ));
        Ok((clock, gain))
    }

    fn apply_event(&mut self, event: &AudioEvent) {
        match event {
            AudioEvent::TrackStarted {
                track_id,
                generation,
            } => {
                if self.snapshot.staged_track_id == Some(*track_id)
                    && self.staged_generation == Some(*generation)
                {
                    self.current_clock = self.staged_clock.take();
                    self.current_gain = self.staged_gain.take();
                    self.snapshot.duration = self.staged_duration.take().unwrap_or_default();
                    self.snapshot.staged_track_id = None;
                    self.staged_generation = None;
                }
                self.snapshot.track_id = Some(*track_id);
                self.snapshot.generation = *generation;
                self.snapshot.position = Duration::ZERO;
                self.snapshot.state = if self.player.is_paused() {
                    PlaybackState::Paused
                } else {
                    PlaybackState::Playing
                };
                self.snapshot.error = None;
            }
            AudioEvent::DeviceLost(error) => {
                self.player.pause();
                self.snapshot.error = Some(error.clone());
                self.snapshot.state = PlaybackState::Paused;
            }
            AudioEvent::TrackFinished { .. } => {}
        }
    }
}

impl AudioEngine for RodioEngine {
    fn load_and_play(&mut self, track: StagedTrack) -> Result<()> {
        self.reset_queue();
        self.spectrum.clear();
        let (clock, gain) = self.append_track(&track)?;
        self.current_clock = Some(clock);
        self.current_gain = Some(gain);
        self.staged_clock = None;
        self.staged_gain = None;
        self.staged_duration = None;
        self.staged_generation = None;
        self.snapshot.state = PlaybackState::Playing;
        self.snapshot.track_id = Some(track.track_id);
        self.snapshot.position = Duration::ZERO;
        self.snapshot.duration = track.duration;
        self.snapshot.staged_track_id = None;
        self.snapshot.generation = track.generation;
        self.snapshot.error = None;
        self.player.play();
        Ok(())
    }

    fn stage_next(&mut self, track: StagedTrack) -> Result<()> {
        self.clear_staged();
        let (clock, gain) = self.append_track(&track)?;
        self.staged_clock = Some(clock);
        self.staged_gain = Some(gain);
        self.staged_duration = Some(track.duration);
        self.staged_generation = Some(track.generation);
        self.snapshot.staged_track_id = Some(track.track_id);
        Ok(())
    }

    fn clear_staged(&mut self) {
        self.queue.clear();
        self.staged_clock = None;
        self.staged_gain = None;
        self.staged_duration = None;
        self.staged_generation = None;
        self.snapshot.staged_track_id = None;
    }

    fn set_pending(&mut self, track_id: TrackId, duration: Duration, state: PlaybackState) {
        self.stop();
        self.snapshot.track_id = Some(track_id);
        self.snapshot.duration = duration;
        self.snapshot.state = state;
    }

    fn play(&mut self) -> Result<()> {
        if self.snapshot.track_id.is_none() {
            return Err(anyhow!("no track is loaded"));
        }
        self.player.play();
        self.snapshot.state = PlaybackState::Playing;
        Ok(())
    }

    fn pause(&mut self) {
        self.player.pause();
        if self.snapshot.track_id.is_some() {
            self.snapshot.state = PlaybackState::Paused;
        }
    }

    fn stop(&mut self) {
        self.reset_queue();
        self.spectrum.clear();
        let volume = self.snapshot.volume;
        let active_device = self.snapshot.active_device.clone();
        let active_device_id = self.snapshot.active_device_id.clone();
        self.snapshot = AudioSnapshot {
            volume,
            active_device,
            active_device_id,
            ..AudioSnapshot::default()
        };
        self.current_clock = None;
        self.staged_clock = None;
        self.current_gain = None;
        self.staged_gain = None;
        self.staged_duration = None;
        self.staged_generation = None;
    }

    fn seek(&mut self, position: Duration) -> Result<()> {
        let position = position.min(self.snapshot.duration);
        self.player
            .try_seek(position)
            .context("seeking current track")?;
        if let Some(clock) = &self.current_clock {
            clock.seeked_to(position);
        }
        self.snapshot.position = position;
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) {
        self.snapshot.volume = volume.clamp(0.0, 1.0);
        self.player.set_volume(self.snapshot.volume);
    }

    fn set_output_gain(&mut self, gain: f32) {
        if let Some(current_gain) = &self.current_gain {
            current_gain.store(u64::from(gain.max(0.0).to_bits()), Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> AudioSnapshot {
        let mut snapshot = self.snapshot.clone();
        if let Some(clock) = &self.current_clock {
            snapshot.position = clock.position().min(snapshot.duration);
        }
        snapshot
    }

    fn drain_events(&mut self) -> Vec<AudioEvent> {
        let mut events: Vec<_> = self.events_rx.try_iter().collect();
        events.extend(self.stream_errors.try_iter().map(AudioEvent::DeviceLost));
        for event in &events {
            self.apply_event(event);
        }
        events
    }

    fn spectrum(&self) -> [f32; SPECTRUM_BANDS] {
        self.spectrum.snapshot()
    }
}

fn open_output(
    preferred_device: Option<&str>,
) -> Result<(MixerDeviceSink, String, String, Receiver<String>)> {
    let host = rodio::cpal::default_host();
    let preferred = if let Some(preferred) = preferred_device {
        preferred.parse().ok().and_then(|id| host.device_by_id(&id))
    } else {
        None
    };
    let (error_tx, error_rx) = unbounded();
    let preferred_result = preferred.and_then(|device| {
        let id = device.id().ok()?.to_string();
        let name = device
            .description()
            .ok()
            .map(|description| description.name().to_owned())
            .unwrap_or_else(|| "Selected device".into());
        open_device(device, error_tx.clone())
            .ok()
            .map(|sink| (sink, name, id))
    });
    let (sink, active_name, active_id) = if let Some((sink, name, id)) = preferred_result {
        (sink, name, id)
    } else {
        let device = host
            .default_output_device()
            .context("no default audio output device")?;
        let name = device
            .description()
            .ok()
            .map(|description| description.name().to_owned())
            .unwrap_or_else(|| "Default".into());
        let id = device.id()?.to_string();
        (open_device(device, error_tx)?, name, id)
    };
    Ok((sink, active_name, active_id, error_rx))
}

fn open_device(device: rodio::Device, error_tx: Sender<String>) -> Result<MixerDeviceSink> {
    let callback = move |error: rodio::cpal::StreamError| {
        let _ = error_tx.send(error.to_string());
    };
    Ok(DeviceSinkBuilder::from_device(device)?
        .with_error_callback(callback)
        .open_sink_or_fallback()?)
}

pub struct SilentEngine {
    snapshot: AudioSnapshot,
    events: Vec<AudioEvent>,
    staged: Option<StagedTrack>,
}

impl SilentEngine {
    pub fn new(volume: f32) -> Self {
        Self {
            snapshot: AudioSnapshot {
                volume: volume.clamp(0.0, 1.0),
                active_device: "Disabled (--no-audio)".into(),
                ..AudioSnapshot::default()
            },
            events: Vec::new(),
            staged: None,
        }
    }
}

impl AudioEngine for SilentEngine {
    fn load_and_play(&mut self, track: StagedTrack) -> Result<()> {
        self.snapshot.track_id = Some(track.track_id);
        self.snapshot.duration = track.duration;
        self.snapshot.position = Duration::ZERO;
        self.snapshot.state = PlaybackState::Playing;
        self.snapshot.generation = track.generation;
        self.snapshot.error = None;
        self.staged = None;
        self.events.push(AudioEvent::TrackStarted {
            track_id: track.track_id,
            generation: track.generation,
        });
        Ok(())
    }

    fn stage_next(&mut self, track: StagedTrack) -> Result<()> {
        self.snapshot.staged_track_id = Some(track.track_id);
        self.staged = Some(track);
        Ok(())
    }

    fn clear_staged(&mut self) {
        self.staged = None;
        self.snapshot.staged_track_id = None;
    }

    fn set_pending(&mut self, track_id: TrackId, duration: Duration, state: PlaybackState) {
        self.stop();
        self.snapshot.track_id = Some(track_id);
        self.snapshot.duration = duration;
        self.snapshot.state = state;
    }

    fn play(&mut self) -> Result<()> {
        if self.snapshot.track_id.is_none() {
            return Err(anyhow!("no track is loaded"));
        }
        self.snapshot.state = PlaybackState::Playing;
        Ok(())
    }

    fn pause(&mut self) {
        if self.snapshot.track_id.is_some() {
            self.snapshot.state = PlaybackState::Paused;
        }
    }

    fn stop(&mut self) {
        let volume = self.snapshot.volume;
        let active_device = self.snapshot.active_device.clone();
        let active_device_id = self.snapshot.active_device_id.clone();
        self.snapshot = AudioSnapshot {
            volume,
            active_device,
            active_device_id,
            ..AudioSnapshot::default()
        };
        self.events.clear();
        self.staged = None;
    }

    fn seek(&mut self, position: Duration) -> Result<()> {
        self.snapshot.position = position.min(self.snapshot.duration);
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) {
        self.snapshot.volume = volume.clamp(0.0, 1.0);
    }

    fn set_output_gain(&mut self, _gain: f32) {}

    fn snapshot(&self) -> AudioSnapshot {
        self.snapshot.clone()
    }

    fn drain_events(&mut self) -> Vec<AudioEvent> {
        std::mem::take(&mut self.events)
    }

    #[cfg(test)]
    fn simulate_staged_transition(&mut self) {
        let Some(next) = self.staged.take() else {
            return;
        };
        if let Some(track_id) = self.snapshot.track_id {
            self.events.push(AudioEvent::TrackFinished {
                track_id,
                generation: self.snapshot.generation,
            });
        }
        self.snapshot.track_id = Some(next.track_id);
        self.snapshot.duration = next.duration;
        self.snapshot.position = Duration::ZERO;
        self.snapshot.generation = next.generation;
        self.snapshot.staged_track_id = None;
        self.events.push(AudioEvent::TrackStarted {
            track_id: next.track_id,
            generation: next.generation,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use std::num::NonZero;

    fn staged(id: TrackId, generation: u64) -> StagedTrack {
        StagedTrack {
            source: MediaSource::LocalFile(PathBuf::from("test.wav")),
            track_id: id,
            duration: Duration::from_secs(5),
            gain: 1.0,
            generation,
        }
    }

    #[test]
    fn silent_engine_obeys_controls_and_staging() -> Result<()> {
        let mut engine = SilentEngine::new(0.8);
        engine.load_and_play(staged(4, 1))?;
        engine.stage_next(staged(5, 2))?;
        assert_eq!(engine.snapshot().staged_track_id, Some(5));
        engine.pause();
        assert_eq!(engine.snapshot().state, PlaybackState::Paused);
        engine.seek(Duration::from_secs(10))?;
        assert_eq!(engine.snapshot().position, Duration::from_secs(5));
        engine.set_volume(2.0);
        assert_eq!(engine.snapshot().volume, 1.0);
        assert_eq!(
            engine.drain_events(),
            vec![AudioEvent::TrackStarted {
                track_id: 4,
                generation: 1
            }]
        );
        engine.stop();
        assert_eq!(engine.snapshot().state, PlaybackState::Stopped);
        assert_eq!(engine.spectrum(), [0.0; SPECTRUM_BANDS]);
        Ok(())
    }

    #[test]
    fn staged_sources_are_sample_contiguous_and_emit_ordered_boundaries() {
        let (event_tx, event_rx) = unbounded();
        let (queue_tx, mut queue_rx) = queue(false);
        for (track_id, generation, gain, samples) in [
            (1, 11, 0.5, vec![0.2, 0.4, 0.6, 0.8]),
            (2, 12, 2.0, vec![0.25, 0.3, 0.35, 0.4]),
        ] {
            let source = SamplesBuffer::new(
                NonZero::new(2).unwrap(),
                NonZero::new(48_000).unwrap(),
                samples,
            );
            let (source, _) = GainSource::new(source, gain);
            let clock = Arc::new(TrackClock::new(2, 48_000));
            queue_tx.append(SignalledSource::new(
                source,
                track_id,
                generation,
                event_tx.clone(),
                clock,
            ));
        }

        let samples: Vec<_> = queue_rx.by_ref().collect();
        assert_eq!(samples, vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8]);
        assert_eq!(
            event_rx.try_iter().collect::<Vec<_>>(),
            vec![
                AudioEvent::TrackStarted {
                    track_id: 1,
                    generation: 11,
                },
                AudioEvent::TrackFinished {
                    track_id: 1,
                    generation: 11,
                },
                AudioEvent::TrackStarted {
                    track_id: 2,
                    generation: 12,
                },
                AudioEvent::TrackFinished {
                    track_id: 2,
                    generation: 12,
                },
            ]
        );
    }
}

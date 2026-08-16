use std::{
    f32::consts::PI,
    sync::{Arc, Mutex},
    time::Duration,
};

use rodio::{ChannelCount, SampleRate, Source, source::SeekError};

pub const SPECTRUM_BANDS: usize = 16;

const WINDOW: usize = 2048;
const FREQS: [f32; SPECTRUM_BANDS] = [
    40.0, 60.0, 100.0, 160.0, 250.0, 400.0, 630.0, 1_000.0, 1_600.0, 2_500.0, 4_000.0, 6_300.0,
    8_000.0, 10_000.0, 12_500.0, 16_000.0,
];

#[derive(Clone)]
pub struct SpectrumTap {
    bands: Arc<Mutex<[f32; SPECTRUM_BANDS]>>,
}

impl SpectrumTap {
    pub fn new() -> Self {
        Self {
            bands: Arc::new(Mutex::new([0.0; SPECTRUM_BANDS])),
        }
    }

    pub fn snapshot(&self) -> [f32; SPECTRUM_BANDS] {
        lock(&self.bands)
    }

    pub fn clear(&self) {
        *self.bands.lock().unwrap_or_else(|error| error.into_inner()) = [0.0; SPECTRUM_BANDS];
    }

    fn store(&self, bands: [f32; SPECTRUM_BANDS]) {
        *self.bands.lock().unwrap_or_else(|error| error.into_inner()) = bands;
    }
}

fn lock(bands: &Mutex<[f32; SPECTRUM_BANDS]>) -> [f32; SPECTRUM_BANDS] {
    *bands.lock().unwrap_or_else(|error| error.into_inner())
}

pub struct SpectrumSource<S> {
    inner: S,
    tap: SpectrumTap,
    analyzer: Analyzer,
    mix: f32,
    mix_count: u16,
}

impl<S: Source> SpectrumSource<S> {
    pub fn new(inner: S, tap: SpectrumTap) -> Self {
        let sample_rate = inner.sample_rate().get();
        Self {
            inner,
            tap,
            analyzer: Analyzer::new(sample_rate),
            mix: 0.0,
            mix_count: 0,
        }
    }
}

impl<S: Source> Iterator for SpectrumSource<S> {
    type Item = S::Item;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;
        self.mix += sample;
        self.mix_count += 1;
        if self.mix_count >= self.inner.channels().get() {
            if let Some(bands) = self.analyzer.push(
                self.mix / f32::from(self.mix_count),
                self.inner.sample_rate().get(),
            ) {
                self.tap.store(bands);
            }
            self.mix = 0.0;
            self.mix_count = 0;
        }
        Some(sample)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S: Source> Source for SpectrumSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.analyzer.reset();
        self.mix = 0.0;
        self.mix_count = 0;
        self.inner.try_seek(pos)
    }
}

struct Analyzer {
    sample_rate: u32,
    coeff: [f32; SPECTRUM_BANDS],
    s1: [f32; SPECTRUM_BANDS],
    s2: [f32; SPECTRUM_BANDS],
    env: [f32; SPECTRUM_BANDS],
    count: usize,
}

impl Analyzer {
    fn new(sample_rate: u32) -> Self {
        let mut analyzer = Self {
            sample_rate: 0,
            coeff: [0.0; SPECTRUM_BANDS],
            s1: [0.0; SPECTRUM_BANDS],
            s2: [0.0; SPECTRUM_BANDS],
            env: [0.0; SPECTRUM_BANDS],
            count: 0,
        };
        analyzer.set_sample_rate(sample_rate);
        analyzer
    }

    fn set_sample_rate(&mut self, sample_rate: u32) {
        if sample_rate == 0 || sample_rate == self.sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        let sr = sample_rate as f32;
        let nyquist = sr / 2.0;
        for (coeff, &freq) in self.coeff.iter_mut().zip(FREQS.iter()) {
            *coeff = if freq < nyquist {
                2.0 * (2.0 * PI * freq / sr).cos()
            } else {
                0.0
            };
        }
        self.reset();
    }

    fn reset(&mut self) {
        self.s1 = [0.0; SPECTRUM_BANDS];
        self.s2 = [0.0; SPECTRUM_BANDS];
        self.count = 0;
    }

    fn push(&mut self, sample: f32, sample_rate: u32) -> Option<[f32; SPECTRUM_BANDS]> {
        self.set_sample_rate(sample_rate);
        for i in 0..SPECTRUM_BANDS {
            if self.coeff[i] == 0.0 {
                continue;
            }
            let s0 = sample + self.coeff[i] * self.s1[i] - self.s2[i];
            self.s2[i] = self.s1[i];
            self.s1[i] = s0;
        }
        self.count += 1;
        if self.count < WINDOW {
            return None;
        }
        Some(self.finish_window())
    }

    fn finish_window(&mut self) -> [f32; SPECTRUM_BANDS] {
        let scale = 1.0 / WINDOW as f32;
        for i in 0..SPECTRUM_BANDS {
            let mag = if self.coeff[i] == 0.0 {
                0.0
            } else {
                let power = self.s1[i] * self.s1[i] + self.s2[i] * self.s2[i]
                    - self.coeff[i] * self.s1[i] * self.s2[i];
                (power.max(0.0) * scale).sqrt()
            };
            let db = 20.0 * (mag + 1e-9).log10();
            let value = ((db + 50.0) / 50.0).clamp(0.0, 1.0);
            if value > self.env[i] {
                self.env[i] = value;
            } else {
                self.env[i] = (self.env[i] * 0.72).max(value);
            }
            self.s1[i] = 0.0;
            self.s2[i] = 0.0;
        }
        self.count = 0;
        self.env
    }
}

pub fn resample_spectrum(src: &[f32], count: usize) -> Vec<f64> {
    if count == 0 {
        return Vec::new();
    }
    if src.is_empty() {
        return vec![0.0; count];
    }
    if count == 1 {
        return vec![src.iter().copied().fold(0.0_f32, f32::max) as f64];
    }
    let last = (src.len() - 1) as f32;
    (0..count)
        .map(|i| {
            let pos = i as f32 * last / (count - 1) as f32;
            let lo = pos as usize;
            let hi = (lo + 1).min(src.len() - 1);
            let t = pos - lo as f32;
            f64::from(src[lo] * (1.0 - t) + src[hi] * t)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_interpolates_between_bands() {
        let out = resample_spectrum(&[0.0, 1.0], 3);
        assert_eq!(out.len(), 3);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!((out[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn analyzer_peaks_near_a_1khz_tone() {
        let mut analyzer = Analyzer::new(44_100);
        let mut last = [0.0; SPECTRUM_BANDS];
        for n in 0..(WINDOW * 4) {
            let sample = (2.0 * PI * 1_000.0 * n as f32 / 44_100.0).sin() * 0.6;
            if let Some(bands) = analyzer.push(sample, 44_100) {
                last = bands;
            }
        }
        let peak = last
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(index, _)| index)
            .unwrap();
        assert!(
            (6..=9).contains(&peak),
            "1 kHz tone peaked at band {peak}, values {last:?}"
        );
        assert!(last[peak] > 0.2, "peak band too quiet: {last:?}");
    }
}

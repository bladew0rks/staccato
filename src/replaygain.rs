use std::{
    collections::BTreeMap,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    thread,
};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use ebur128::{EbuR128, Mode};
use lofty::{
    config::WriteOptions,
    file::{AudioFile, TaggedFileExt},
    probe::Probe,
    tag::{ItemKey, Tag},
};
use rodio::{Decoder, Source};

use crate::model::{ReplayGainInfo, TrackId};

pub const TARGET_LUFS: f64 = -18.0;

pub enum ReplayGainEvent {
    Started {
        total: usize,
    },
    Track {
        id: TrackId,
        info: ReplayGainInfo,
        path: PathBuf,
    },
    Failed {
        error: String,
    },
    Finished {
        scanned: usize,
        failed: usize,
    },
}

pub struct ReplayGainHandle {
    pub events: Receiver<ReplayGainEvent>,
}

pub fn parse_gain(value: &str) -> Option<f32> {
    let value = value.replace('−', "-");
    let value = value
        .trim()
        .trim_end_matches("dB")
        .trim_end_matches("db")
        .trim_end_matches("DB")
        .trim();
    value.parse().ok()
}

pub fn parse_peak(value: &str) -> Option<f32> {
    value.trim().parse().ok()
}

pub fn read_from_tag(tag: &lofty::tag::Tag) -> ReplayGainInfo {
    let track_gain = tag
        .get_string(ItemKey::ReplayGainTrackGain)
        .and_then(parse_gain)
        .or_else(|| tag.get_string(ItemKey::R128TrackGain).and_then(parse_r128));
    let album_gain = tag
        .get_string(ItemKey::ReplayGainAlbumGain)
        .and_then(parse_gain)
        .or_else(|| tag.get_string(ItemKey::R128AlbumGain).and_then(parse_r128));
    ReplayGainInfo {
        track_gain,
        track_peak: tag
            .get_string(ItemKey::ReplayGainTrackPeak)
            .and_then(parse_peak),
        album_gain,
        album_peak: tag
            .get_string(ItemKey::ReplayGainAlbumPeak)
            .and_then(parse_peak),
    }
}

fn parse_r128(value: &str) -> Option<f32> {
    let raw: f32 = value.trim().parse().ok()?;
    Some(raw / 256.0 + 5.0)
}

pub fn write_tags(path: &Path, info: &ReplayGainInfo) -> Result<()> {
    let mut tagged = Probe::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .read()
        .with_context(|| format!("reading tags from {}", path.display()))?;
    if tagged.primary_tag().is_none() {
        let tag_type = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .primary_tag_mut()
        .context("file has no writable tag")?;
    if let Some(gain) = info.track_gain {
        tag.insert_text(ItemKey::ReplayGainTrackGain, format!("{gain:.2} dB"));
    }
    if let Some(peak) = info.track_peak {
        tag.insert_text(ItemKey::ReplayGainTrackPeak, format!("{peak:.6}"));
    }
    if let Some(gain) = info.album_gain {
        tag.insert_text(ItemKey::ReplayGainAlbumGain, format!("{gain:.2} dB"));
    }
    if let Some(peak) = info.album_peak {
        tag.insert_text(ItemKey::ReplayGainAlbumPeak, format!("{peak:.6}"));
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .with_context(|| format!("writing ReplayGain tags to {}", path.display()))?;
    Ok(())
}

pub fn start(jobs: Vec<(TrackId, PathBuf, String, String)>) -> ReplayGainHandle {
    let (tx, rx) = unbounded();
    thread::spawn(move || {
        if let Err(error) = scan_jobs(jobs, &tx) {
            tracing::error!(%error, "replaygain scan failed");
        }
    });
    ReplayGainHandle { events: rx }
}

fn scan_jobs(
    jobs: Vec<(TrackId, PathBuf, String, String)>,
    tx: &Sender<ReplayGainEvent>,
) -> Result<()> {
    let _ = tx.send(ReplayGainEvent::Started { total: jobs.len() });
    let mut groups: BTreeMap<(String, String), Vec<(TrackId, PathBuf)>> = BTreeMap::new();
    for (id, path, artist, album) in jobs {
        groups
            .entry((artist.to_lowercase(), album.to_lowercase()))
            .or_default()
            .push((id, path));
    }
    let mut scanned = 0;
    let mut failed = 0;
    for members in groups.into_values() {
        match scan_album(&members) {
            Ok(results) => {
                for (id, path, info) in results {
                    scanned += 1;
                    if let Err(error) = write_tags(&path, &info) {
                        tracing::warn!(%error, path = %path.display(), "could not write ReplayGain tags");
                    }
                    let _ = tx.send(ReplayGainEvent::Track { id, info, path });
                }
            }
            Err(error) => {
                for _ in members {
                    failed += 1;
                    let _ = tx.send(ReplayGainEvent::Failed {
                        error: error.to_string(),
                    });
                }
            }
        }
    }
    let _ = tx.send(ReplayGainEvent::Finished { scanned, failed });
    Ok(())
}

fn scan_album(members: &[(TrackId, PathBuf)]) -> Result<Vec<(TrackId, PathBuf, ReplayGainInfo)>> {
    let mut track_results = Vec::new();
    let mut album_ebur: Option<EbuR128> = None;
    let mut album_layout: Option<(u32, u32)> = None;
    for (id, path) in members {
        let measured = analyze_file(path, album_ebur.as_mut(), &mut album_layout)?;
        track_results.push((
            *id,
            path.clone(),
            ReplayGainInfo {
                track_gain: Some(measured.gain),
                track_peak: Some(measured.peak),
                album_gain: None,
                album_peak: None,
            },
        ));
        if album_ebur.is_none()
            && let Some((rate, channels)) = album_layout
        {
            album_ebur = Some(
                EbuR128::new(channels, rate, Mode::I | Mode::TRUE_PEAK)
                    .context("creating album loudness meter")?,
            );
            // First track's samples were only added to the track meter. Re-read
            // into the album meter so album gain includes every file.
            let _ = feed_file(path, album_ebur.as_mut().expect("just created"));
        }
    }
    let (album_gain, album_peak) = if let Some(ebur) = album_ebur.as_ref() {
        (Some(gain_from_ebur(ebur)?), Some(peak_from_ebur(ebur)?))
    } else {
        let gains: Vec<f32> = track_results
            .iter()
            .filter_map(|(_, _, info)| info.track_gain)
            .collect();
        let peaks: Vec<f32> = track_results
            .iter()
            .filter_map(|(_, _, info)| info.track_peak)
            .collect();
        let gain = if gains.is_empty() {
            None
        } else {
            Some(gains.iter().sum::<f32>() / gains.len() as f32)
        };
        let peak = peaks.into_iter().max_by(|a, b| a.total_cmp(b));
        (gain, peak)
    };
    for (_, _, info) in &mut track_results {
        info.album_gain = album_gain;
        info.album_peak = album_peak;
    }
    Ok(track_results)
}

struct Measured {
    gain: f32,
    peak: f32,
}

fn analyze_file(
    path: &Path,
    album_ebur: Option<&mut EbuR128>,
    album_layout: &mut Option<(u32, u32)>,
) -> Result<Measured> {
    let (rate, channels, mut track_ebur) = open_meter(path)?;
    match *album_layout {
        None => *album_layout = Some((rate, channels)),
        Some((rate0, ch0)) if rate0 != rate || ch0 != channels => {}
        Some(_) => {}
    }
    feed_file(path, &mut track_ebur)?;
    if let Some(album) = album_ebur
        && album_layout.is_some_and(|(rate0, ch0)| rate0 == rate && ch0 == channels)
    {
        feed_file(path, album)?;
    }
    Ok(Measured {
        gain: gain_from_ebur(&track_ebur)?,
        peak: peak_from_ebur(&track_ebur)?,
    })
}

fn open_meter(path: &Path) -> Result<(u32, u32, EbuR128)> {
    let decoder = open_decoder(path)?;
    let rate = decoder.sample_rate().get();
    let channels = u32::from(decoder.channels().get());
    let ebur = EbuR128::new(channels, rate, Mode::I | Mode::TRUE_PEAK)
        .context("creating loudness meter")?;
    Ok((rate, channels, ebur))
}

fn open_decoder(path: &Path) -> Result<Decoder<BufReader<File>>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let byte_len = file.metadata().ok().map(|metadata| metadata.len());
    let mut builder = Decoder::builder()
        .with_data(BufReader::new(file))
        .with_gapless(true);
    if let Some(byte_len) = byte_len {
        builder = builder.with_byte_len(byte_len);
    }
    builder
        .build()
        .with_context(|| format!("decoding {}", path.display()))
}

fn feed_file(path: &Path, ebur: &mut EbuR128) -> Result<()> {
    let decoder = open_decoder(path)?;
    let channels = usize::from(decoder.channels().get());
    let chunk = channels * 48_000;
    let mut buf = Vec::with_capacity(chunk);
    for sample in decoder {
        buf.push(sample);
        if buf.len() >= chunk {
            ebur.add_frames_f32(&buf).context("measuring loudness")?;
            buf.clear();
        }
    }
    if !buf.is_empty() {
        let leftover = buf.len() % channels;
        if leftover != 0 {
            buf.truncate(buf.len() - leftover);
        }
        if !buf.is_empty() {
            ebur.add_frames_f32(&buf).context("measuring loudness")?;
        }
    }
    Ok(())
}

fn gain_from_ebur(ebur: &EbuR128) -> Result<f32> {
    let loudness = ebur.loudness_global().context("integrated loudness")?;
    if !loudness.is_finite() {
        anyhow::bail!("could not measure loudness");
    }
    Ok((TARGET_LUFS - loudness) as f32)
}

fn peak_from_ebur(ebur: &EbuR128) -> Result<f32> {
    let channels = ebur.channels();
    let mut peak = 0.0f32;
    for channel in 0..channels {
        if let Ok(value) = ebur.true_peak(channel) {
            peak = peak.max(value as f32);
        }
    }
    Ok(peak)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gain_accepts_common_tag_spellings() {
        assert_eq!(parse_gain("-6.42 dB"), Some(-6.42));
        assert_eq!(parse_gain("−3.00dB"), Some(-3.0));
        assert_eq!(parse_gain("1.5"), Some(1.5));
    }

    #[test]
    fn r128_converts_to_replaygain_reference() {
        assert!((parse_r128("-1280").unwrap() - 0.0).abs() < 0.01);
    }
}

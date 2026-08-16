use super::*;
use crate::action::Action;
use crate::model::{Track, TrackId, TrackOrigin};
use anyhow::Result;
use std::{path::PathBuf, time::Duration};

#[test]
fn shuffle_is_a_stable_permutation() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut app = App::open(&directory.path().join("shuffle.db"), true)?;
    app.playlists[0].items = vec![10, 20, 30, 40, 50];
    app.rebuild_shuffle();
    let first = app.shuffle.clone();
    app.rebuild_shuffle();
    assert_eq!(app.shuffle, first);
    let mut sorted = app.shuffle.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![0, 1, 2, 3, 4]);
    Ok(())
}

fn sample_track(id: TrackId, artist: &str, album: &str, title: &str) -> Track {
    Track {
        id,
        path: PathBuf::from(format!("/music/{title}.flac")),
        title: title.into(),
        artist: artist.into(),
        album: album.into(),
        date: Some(2024),
        track_number: Some(1),
        duration: Duration::from_secs(180),
        codec: "FLAC".into(),
        sample_rate: Some(44_100),
        channels: Some(2),
        file_size: 10,
        modified_ns: 0,
        unavailable: false,
        scan_error: None,
        origin: TrackOrigin::Local,
        replay_gain: crate::model::ReplayGainInfo::default(),
    }
}

#[test]
fn queue_plays_before_the_playlist() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut app = App::open(&directory.path().join("queue.db"), true)?;
    app.tracks.insert(1, sample_track(1, "A", "A", "One"));
    app.tracks.insert(2, sample_track(2, "A", "A", "Two"));
    app.playlists[0].items = vec![1, 2];
    app.playlist_selection = 0;
    app.queue.push(2);
    app.playing = Some((app.playlists[0].id, 0));
    app.handle(Action::Next);
    app.tick();
    assert_eq!(app.audio_snapshot.track_id, Some(2));
    assert!(app.queue.is_empty());
    Ok(())
}

#[test]
fn stop_after_current_halts_automatic_advance() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut app = App::open(&directory.path().join("sac.db"), true)?;
    app.tracks.insert(1, sample_track(1, "A", "A", "One"));
    app.tracks.insert(2, sample_track(2, "A", "A", "Two"));
    app.playlists[0].items = vec![1, 2];
    app.playing = Some((app.playlists[0].id, 0));
    app.stop_after_current = true;
    app.advance(true)?;
    assert!(app.playing.is_none());
    assert!(!app.stop_after_current);
    Ok(())
}

#[test]
fn staged_transition_advances_without_polling_for_an_empty_player() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut app = App::open(&directory.path().join("gapless.db"), true)?;
    app.tracks.insert(1, sample_track(1, "A", "A", "One"));
    app.tracks.insert(2, sample_track(2, "A", "A", "Two"));
    app.playlists[0].items = vec![1, 2];

    app.play_at(0, 0)?;
    assert_eq!(app.audio.snapshot().track_id, Some(1));
    assert_eq!(app.audio.snapshot().staged_track_id, None);
    app.tick();
    assert_eq!(app.audio.snapshot().staged_track_id, Some(2));
    app.audio.simulate_staged_transition();
    app.tick();

    assert_eq!(app.audio_snapshot.track_id, Some(2));
    assert_eq!(app.playing, Some((app.playlists[0].id, 1)));
    assert_eq!(app.audio_snapshot.staged_track_id, None);
    Ok(())
}

#[test]
fn queue_and_stop_after_current_replace_the_staged_successor() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut app = App::open(&directory.path().join("restage.db"), true)?;
    app.tracks.insert(1, sample_track(1, "A", "A", "One"));
    app.tracks.insert(2, sample_track(2, "A", "A", "Two"));
    app.tracks.insert(3, sample_track(3, "B", "B", "Queued"));
    app.playlists[0].items = vec![1, 2, 3];
    app.play_at(0, 0)?;
    app.tick();
    assert_eq!(app.audio.snapshot().staged_track_id, Some(2));

    app.playlist_selection = 2;
    app.queue_selection();
    assert_eq!(app.audio.snapshot().staged_track_id, Some(3));
    app.handle(Action::ToggleStopAfterCurrent);
    assert_eq!(app.audio.snapshot().staged_track_id, None);
    Ok(())
}

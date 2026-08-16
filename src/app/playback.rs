use std::time::Duration;

use anyhow::{Context, Result, anyhow};

use crate::{
    audio::{AudioEvent, MediaSource, StagedTrack, create_engine},
    model::{PlaybackOrder, PlaybackState, PlaylistId, Track, TrackId, TrackOrigin},
    net::{cache_is_complete, cache_path},
};

use super::{App, ContentPane, util::xorshift_shuffle};

pub(crate) struct StagedPlayback {
    pub track_id: TrackId,
    pub generation: u64,
    pub playlist: Option<(PlaylistId, usize)>,
    pub queue_index: Option<usize>,
}

struct PlaybackCandidate {
    track_id: TrackId,
    playlist: Option<(PlaylistId, usize)>,
    queue_index: Option<usize>,
}

pub(crate) enum PlayOrigin {
    Playlist {
        playlist_index: usize,
        item_index: usize,
    },
    Track(TrackId),
}

enum PlaySource {
    Buffering,
    Disconnected,
}

impl App {
    pub(crate) fn clear_transport(&mut self) {
        self.audio.stop();
        self.playing = None;
        self.staged_playback = None;
        self.audio_started_generation = None;
    }

    pub(crate) fn toggle_play(&mut self) -> Result<()> {
        match self.audio.snapshot().state {
            PlaybackState::Playing => self.audio.pause(),
            PlaybackState::Paused => self.audio.play()?,
            PlaybackState::Loading | PlaybackState::Buffering => {
                self.clear_transport();
                self.pending_play = None;
                self.pending_track = None;
                self.status = "Playback canceled".into();
            }
            PlaybackState::Stopped => self.play_selected()?,
        }
        Ok(())
    }

    pub(crate) fn play_selected(&mut self) -> Result<()> {
        if self.playlists[self.active_playlist].items.is_empty() {
            return Err(anyhow!("the active playlist is empty"));
        }
        self.play(PlayOrigin::Playlist {
            playlist_index: self.active_playlist,
            item_index: self.playlist_selection,
        })
    }

    pub(crate) fn play_at(&mut self, playlist_index: usize, item_index: usize) -> Result<()> {
        self.play(PlayOrigin::Playlist {
            playlist_index,
            item_index,
        })
    }

    pub(crate) fn play_track_id(&mut self, track_id: TrackId) -> Result<()> {
        self.play(PlayOrigin::Track(track_id))
    }

    pub(crate) fn play(&mut self, origin: PlayOrigin) -> Result<()> {
        let (track, playlist) = match origin {
            PlayOrigin::Playlist {
                playlist_index,
                item_index,
            } => {
                let playlist_id = self
                    .playlists
                    .get(playlist_index)
                    .context("playlist disappeared")?
                    .id;
                let track_id = *self
                    .playlists
                    .get(playlist_index)
                    .and_then(|playlist| playlist.items.get(item_index))
                    .context("track selection is out of range")?;
                (
                    self.track_or_err(track_id)?,
                    Some((playlist_id, playlist_index, item_index)),
                )
            }
            PlayOrigin::Track(track_id) => (self.track_or_err(track_id)?, None),
        };
        if track.unavailable {
            return Err(anyhow!("file is unavailable: {}", track.path.display()));
        }
        if let Some(error) = &track.scan_error {
            return Err(anyhow!("{}: {error}", track.path.display()));
        }
        match self.playable_source(&track) {
            Ok(source) => self.start_track(track, source, playlist),
            Err(PlaySource::Buffering) => {
                self.audio
                    .set_pending(track.id, track.duration, PlaybackState::Buffering);
                self.staged_playback = None;
                self.audio_started_generation = None;
                if let Some((_, playlist_index, item_index)) = playlist {
                    self.pending_play = Some((playlist_index, item_index));
                    self.prefetch_neighbors(playlist_index, item_index);
                } else {
                    self.pending_track = Some(track.id);
                }
                self.status = format!("Buffering {} — {}", track.artist, track.title);
                Ok(())
            }
            Err(PlaySource::Disconnected) => Err(anyhow!(
                "not connected to {}",
                self.remote_name.as_deref().unwrap_or("the server")
            )),
        }
    }

    fn track_or_err(&self, track_id: TrackId) -> Result<Track> {
        self.tracks
            .get(&track_id)
            .cloned()
            .context("track metadata is missing")
    }

    fn playable_source(&self, track: &Track) -> Result<MediaSource, PlaySource> {
        if let Some(source) = self.local_or_cached_source(track) {
            return Ok(source);
        }
        match &track.origin {
            TrackOrigin::Remote { remote_id, .. } if self.remote.is_some() => {
                let etag = format!("{}-{}", track.modified_ns, track.file_size);
                if let Some(remote) = &self.remote {
                    remote.fetch(remote_id.clone(), track.file_size, etag);
                }
                Err(PlaySource::Buffering)
            }
            TrackOrigin::Remote { .. } => Err(PlaySource::Disconnected),
            TrackOrigin::Local => Err(PlaySource::Disconnected),
        }
    }

    pub(crate) fn local_or_cached_source(&self, track: &Track) -> Option<MediaSource> {
        match &track.origin {
            TrackOrigin::Local => Some(MediaSource::LocalFile(track.path.clone())),
            TrackOrigin::Remote {
                fingerprint,
                remote_id,
                ..
            } => {
                let cached = cache_path(&self.cache_dir(), fingerprint, remote_id);
                cache_is_complete(&cached, track.file_size)
                    .then_some(MediaSource::LocalFile(cached))
            }
        }
    }

    fn start_track(
        &mut self,
        track: Track,
        source: MediaSource,
        playlist: Option<(PlaylistId, usize, usize)>,
    ) -> Result<()> {
        self.audio
            .set_pending(track.id, track.duration, PlaybackState::Loading);
        let generation = self.next_playback_generation();
        self.audio.load_and_play(StagedTrack {
            source,
            track_id: track.id,
            duration: track.duration,
            gain: self.gain_for_track(&track),
            generation,
        })?;
        self.audio_started_generation = None;
        if let Some((playlist_id, playlist_index, item_index)) = playlist {
            self.playing = Some((playlist_id, item_index));
            self.pending_play = None;
            self.pending_track = None;
            if self.should_follow_playback(playlist_id) {
                self.playlist_selection = item_index;
            }
            self.status = match &track.origin {
                TrackOrigin::Remote { server_name, .. } => format!(
                    "Streaming from {server_name}: {} — {}",
                    track.artist, track.title
                ),
                TrackOrigin::Local => format!("Playing: {} — {}", track.artist, track.title),
            };
            self.prefetch_neighbors(playlist_index, item_index);
        } else {
            self.pending_track = None;
            self.status = format!("Playing: {} — {}", track.artist, track.title);
        }
        self.apply_replay_gain();
        Ok(())
    }

    pub(crate) fn should_follow_playback(&self, playlist_id: PlaylistId) -> bool {
        self.cursor_follows_playback
            && self.content_pane() == ContentPane::Playlist
            && self
                .playlists
                .get(self.active_playlist)
                .is_some_and(|playlist| playlist.id == playlist_id)
    }

    pub(crate) fn previous(&mut self) -> Result<()> {
        if self.audio.snapshot().position >= Duration::from_secs(5) {
            return self.audio.seek(Duration::ZERO);
        }
        let Some((playlist_id, index)) = self.playing else {
            return self.play_selected();
        };
        let playlist_index = self
            .playlists
            .iter()
            .position(|playlist| playlist.id == playlist_id)
            .unwrap_or(self.active_playlist);
        self.play_at(playlist_index, index.saturating_sub(1))
    }

    pub(crate) fn advance(&mut self, automatic: bool) -> Result<()> {
        if automatic && self.stop_after_current {
            self.stop_after_current = false;
            self.clear_transport();
            self.status = "Stopped after current".into();
            return Ok(());
        }
        if !self.queue.is_empty() {
            let id = self.queue.remove(0);
            self.queue_selection = self.queue_selection.min(self.queue.len().saturating_sub(1));
            return self.play_track_id(id);
        }
        let Some((playlist_id, index)) = self.playing else {
            return if automatic {
                Ok(())
            } else {
                self.play_selected()
            };
        };
        let playlist_index = self
            .playlists
            .iter()
            .position(|playlist| playlist.id == playlist_id)
            .context("playing playlist was closed")?;
        let len = self.playlists[playlist_index].items.len();
        if len == 0 {
            self.clear_transport();
            return Ok(());
        }
        let candidates = match self.playback_order {
            PlaybackOrder::RepeatTrack if automatic => vec![index],
            PlaybackOrder::Default | PlaybackOrder::RepeatTrack => {
                if index + 1 >= len {
                    self.clear_transport();
                    self.status = "Playback finished".into();
                    return Ok(());
                }
                self.successor_indices(playlist_index, index, PlaybackOrder::Default)
            }
            order => self.successor_indices(playlist_index, index, order),
        };
        for next in candidates {
            match self.play_at(playlist_index, next) {
                Ok(()) => {
                    if self.playback_order == PlaybackOrder::Shuffle {
                        self.shuffle_cursor = self
                            .shuffle
                            .iter()
                            .position(|candidate| *candidate == next)
                            .unwrap_or(self.shuffle_cursor);
                    }
                    return Ok(());
                }
                Err(error) => self.status = format!("Skipped unplayable track: {error:#}"),
            }
        }
        self.clear_transport();
        self.status = "No playable tracks remain".into();
        Ok(())
    }

    fn successor_indices(
        &mut self,
        playlist_index: usize,
        current: usize,
        order: PlaybackOrder,
    ) -> Vec<usize> {
        let len = self
            .playlists
            .get(playlist_index)
            .map_or(0, |playlist| playlist.items.len());
        if len == 0 {
            return Vec::new();
        }
        let playlist_id = self.playlists[playlist_index].id;
        match order {
            PlaybackOrder::RepeatTrack => vec![current],
            PlaybackOrder::Shuffle => {
                if self.shuffle.len() != len || self.shuffle_playlist != Some(playlist_id) {
                    self.rebuild_shuffle_for(playlist_index);
                }
                (1..=len)
                    .map(|step| self.shuffle[(self.shuffle_cursor + step) % len])
                    .collect()
            }
            PlaybackOrder::ShuffleAlbums => self.album_shuffle_candidates(playlist_index, current),
            PlaybackOrder::RepeatPlaylist => (1..=len).map(|step| (current + step) % len).collect(),
            PlaybackOrder::Default => ((current + 1)..len).collect(),
        }
    }

    pub(crate) fn rebuild_album_groups(&mut self, playlist_index: usize) {
        let Some(playlist) = self.playlists.get(playlist_index) else {
            self.album_groups.clear();
            return;
        };
        let mut order = Vec::new();
        let mut groups: std::collections::BTreeMap<(String, String), Vec<usize>> =
            std::collections::BTreeMap::new();
        for (index, id) in playlist.items.iter().enumerate() {
            let Some(track) = self.tracks.get(id) else {
                continue;
            };
            let key = (track.artist.to_lowercase(), track.album.to_lowercase());
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(index);
        }
        let seed = playlist.id as u64 ^ (order.len() as u64).wrapping_mul(0x9E37_79B9);
        xorshift_shuffle(&mut order, seed);
        self.album_groups = order
            .into_iter()
            .filter_map(|key| groups.remove(&key))
            .collect();
    }

    fn album_shuffle_candidates(&mut self, playlist_index: usize, current: usize) -> Vec<usize> {
        if self.album_groups.is_empty() {
            self.rebuild_album_groups(playlist_index);
        }
        let Some(group_index) = self
            .album_groups
            .iter()
            .position(|group| group.contains(&current))
        else {
            return ((current + 1)..self.playlists[playlist_index].items.len()).collect();
        };
        let group = &self.album_groups[group_index];
        if let Some(pos) = group.iter().position(|index| *index == current)
            && pos + 1 < group.len()
        {
            return group[pos + 1..].to_vec();
        }
        (1..=self.album_groups.len())
            .map(|step| {
                let next = (group_index + step) % self.album_groups.len();
                self.album_groups[next][0]
            })
            .collect()
    }

    pub(crate) fn next_playback_generation(&mut self) -> u64 {
        self.playback_generation = self.playback_generation.wrapping_add(1).max(1);
        self.playback_generation
    }

    pub(crate) fn gain_for_track(&self, track: &Track) -> f32 {
        track.replay_gain.apply(
            self.replay_gain_mode,
            self.replay_gain_preamp,
            self.replay_gain_prevent_clip,
        )
    }

    pub(crate) fn staging_source(&self, track: &Track) -> Option<MediaSource> {
        if track.unavailable || track.scan_error.is_some() {
            return None;
        }
        self.local_or_cached_source(track)
    }

    fn successor_candidates(&mut self) -> Vec<PlaybackCandidate> {
        if self.stop_after_current {
            return Vec::new();
        }
        if !self.queue.is_empty() {
            return self
                .queue
                .iter()
                .copied()
                .enumerate()
                .map(|(queue_index, track_id)| PlaybackCandidate {
                    track_id,
                    playlist: None,
                    queue_index: Some(queue_index),
                })
                .collect();
        }
        let Some((playlist_id, index)) = self.playing else {
            return Vec::new();
        };
        let Some(playlist_index) = self
            .playlists
            .iter()
            .position(|playlist| playlist.id == playlist_id)
        else {
            return Vec::new();
        };
        self.successor_indices(playlist_index, index, self.playback_order)
            .into_iter()
            .filter_map(|next| {
                self.playlists[playlist_index]
                    .items
                    .get(next)
                    .copied()
                    .map(|track_id| PlaybackCandidate {
                        track_id,
                        playlist: Some((playlist_id, next)),
                        queue_index: None,
                    })
            })
            .collect()
    }

    fn stage_successor(&mut self) -> Result<()> {
        let snapshot = self.audio.snapshot();
        if snapshot.track_id.is_none() || self.audio_started_generation != Some(snapshot.generation)
        {
            return Ok(());
        }
        self.audio.clear_staged();
        self.staged_playback = None;
        for candidate in self.successor_candidates() {
            let Some(track) = self.tracks.get(&candidate.track_id).cloned() else {
                continue;
            };
            let Some(source) = self.staging_source(&track) else {
                continue;
            };
            let generation = self.next_playback_generation();
            self.audio.stage_next(StagedTrack {
                source,
                track_id: candidate.track_id,
                duration: track.duration,
                gain: self.gain_for_track(&track),
                generation,
            })?;
            self.staged_playback = Some(StagedPlayback {
                track_id: candidate.track_id,
                generation,
                playlist: candidate.playlist,
                queue_index: candidate.queue_index,
            });
            break;
        }
        Ok(())
    }

    pub(crate) fn restage_successor(&mut self) {
        if let Err(error) = self.stage_successor() {
            self.status = format!("Could not stage next track: {error:#}");
        }
    }

    pub(crate) fn drain_audio_events(&mut self) {
        let events = self.audio.drain_events();
        let mut stage_after_transition = false;
        let mut finished = Vec::new();
        for event in events {
            match event {
                AudioEvent::TrackStarted {
                    track_id,
                    generation,
                } => {
                    self.audio_started_generation = Some(generation);
                    if let Some(staged) = self.staged_playback.take_if(|staged| {
                        staged.track_id == track_id && staged.generation == generation
                    }) {
                        if let Some(queue_index) = staged.queue_index
                            && self.queue.get(queue_index) == Some(&track_id)
                        {
                            self.queue.drain(..=queue_index);
                            self.queue_selection =
                                self.queue_selection.min(self.queue.len().saturating_sub(1));
                        }
                        if let Some((playlist_id, index)) = staged.playlist {
                            self.playing = Some((playlist_id, index));
                            if self.playback_order == PlaybackOrder::Shuffle {
                                self.shuffle_cursor = self
                                    .shuffle
                                    .iter()
                                    .position(|candidate| *candidate == index)
                                    .unwrap_or(self.shuffle_cursor);
                            }
                            if self.should_follow_playback(playlist_id) {
                                self.playlist_selection = index;
                            }
                        }
                        if let Some(track) = self.tracks.get(&track_id) {
                            self.status = format!("Playing: {} — {}", track.artist, track.title);
                        }
                    }
                    stage_after_transition = true;
                }
                AudioEvent::TrackFinished { generation, .. } => finished.push(generation),
                AudioEvent::DeviceLost(error) => {
                    self.audio_error = Some(error.clone());
                    self.status = format!("Audio device lost: {error}");
                }
            }
        }
        if stage_after_transition {
            self.restage_successor();
        }
        if finished.contains(&self.audio.snapshot().generation)
            && let Err(error) = self.advance(true)
        {
            self.status = format!("Playback error: {error:#}");
        }
    }

    pub(crate) fn retry_audio(&mut self) -> Result<()> {
        if self.no_audio {
            self.status = "Audio is disabled by --no-audio".into();
            return Ok(());
        }
        self.switch_output_device()?;
        self.status = "Audio output initialized".into();
        Ok(())
    }

    pub(crate) fn switch_output_device(&mut self) -> Result<()> {
        let snapshot = self.audio.snapshot();
        let current = snapshot
            .track_id
            .and_then(|track_id| self.tracks.get(&track_id).cloned());
        let mut replacement = create_engine(
            false,
            snapshot.volume,
            self.preferred_output_device.as_deref(),
        )?;
        if let Some(track) = current
            && let Some(source) = self.staging_source(&track)
        {
            let generation = self.next_playback_generation();
            replacement.load_and_play(StagedTrack {
                source,
                track_id: track.id,
                duration: track.duration,
                gain: self.gain_for_track(&track),
                generation,
            })?;
            replacement.seek(snapshot.position)?;
            if snapshot.state == PlaybackState::Paused {
                replacement.pause();
            }
        }
        self.audio = replacement;
        self.audio_started_generation = None;
        self.staged_playback = None;
        self.audio_error = None;
        Ok(())
    }

    pub(crate) fn rebuild_shuffle(&mut self) {
        self.rebuild_shuffle_for(self.active_playlist);
        self.rebuild_album_groups(self.active_playlist);
    }

    pub(crate) fn rebuild_shuffle_for(&mut self, playlist_index: usize) {
        let len = self
            .playlists
            .get(playlist_index)
            .map_or(0, |playlist| playlist.items.len());
        self.shuffle = (0..len).collect();
        let seed = self
            .playlists
            .get(playlist_index)
            .map_or(1, |playlist| playlist.id as u64)
            ^ (len as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        xorshift_shuffle(&mut self.shuffle, seed);
        self.shuffle_cursor = self
            .playing
            .and_then(|(_, current)| self.shuffle.iter().position(|item| *item == current))
            .unwrap_or(0);
        self.shuffle_playlist = self
            .playlists
            .get(playlist_index)
            .map(|playlist| playlist.id);
    }
}

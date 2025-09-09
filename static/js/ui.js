// Navigation and UI functions

// Show different sections
function showSection(section) {
    // Hide all sections
    const sections = ['library', 'playlists', 'playlist-detail', 'search', 'downloads'];
    sections.forEach(s => {
        const element = document.getElementById(s + '-section');
        if (element) {
            element.style.display = 'none';
        }
    });

    // Clear playlistTracks when leaving playlist-detail
    if (section !== 'playlist-detail') {
        delete window.playlistTracks;
    }

    // Show selected section
    const selectedSection = document.getElementById(section + '-section');
    if (selectedSection) {
        selectedSection.style.display = 'block';
    }

    // Update navigation
    document.querySelectorAll('.nav-item').forEach(item => {
        item.classList.remove('active');
    });

    const activeNavItem = document.querySelector(`[onclick="showSection('${section}')"]`);
    if (activeNavItem) {
        activeNavItem.classList.add('active');
    }

    currentSection = section;

    // Close mobile sidebar
    if (window.innerWidth <= 768) {
        const sidebar = document.getElementById('sidebar');
        sidebar.classList.remove('open');
    }
}

// Load all tracks
async function loadTracks(sortBy = null) {
    try {
        const response = await fetch('/api/tracks' + (sortBy ? `?sort=${sortBy}` : ''));
        if (response.ok) {
            tracks = await response.json();
            displayTracks();
            updateStats();
        } else {
            console.error('Failed to load tracks');
        }
    } catch (error) {
        console.error('Error loading tracks:', error);
        const tracksContainer = document.getElementById('tracks');
        if (tracksContainer) {
            tracksContainer.innerHTML = '<div class="loading">Failed to load tracks</div>';
        }
    }
}

// Load all playlists
async function loadPlaylists() {
    try {
        const response = await fetch('/api/playlists');
        if (response.ok) {
            playlists = await response.json();
            displayPlaylists();
        } else {
            console.error('Failed to load playlists');
        }
    } catch (error) {
        console.error('Error loading playlists:', error);
        const playlistsContainer = document.getElementById('playlists');
        if (playlistsContainer) {
            playlistsContainer.innerHTML = '<div class="loading">Failed to load playlists</div>';
        }
    }
}

// Display tracks in the library
function displayTracks(tracksToShow = tracks) {
    const tracksContainer = document.getElementById('tracks');
    if (!tracksContainer) return;
    
    if (tracksToShow.length === 0) {
        tracksContainer.innerHTML = '<div class="loading">No tracks found</div>';
        return;
    }
    
    tracksContainer.innerHTML = tracksToShow.map(track => `
        <div class="track-item${currentTrackId === track.id ? ' playing' : ''}" onclick="playTrack(${track.id})">
            ${track.hasAlbumArt ? `<div class="album-art">
                <img src="/albumart/${track.albumArtId}" alt="Album Art" onerror="this.parentElement.style.display='none'">
            </div>` : ''}
            <div class="track-info">
                <div class="track-title">${escapeHtml(track.title)}</div>
                <div class="track-artist">${escapeHtml(track.artist)}</div>
            </div>
            <div class="track-actions">
                <button class="play-btn" onclick="event.stopPropagation(); playTrack(${track.id})">PLAY</button>
                <button class="add-to-playlist-btn" onclick="event.stopPropagation(); showAddToPlaylistModal(${track.id})">+ PLAYLIST</button>
            </div>
        </div>
    `).join('');
}

// Display playlists
function displayPlaylists() {
    const playlistsContainer = document.getElementById('playlists');
    if (!playlistsContainer) return;
    
    if (playlists.length === 0) {
        playlistsContainer.innerHTML = '<div class="loading">No playlists found</div>';
        return;
    }
    
    playlistsContainer.innerHTML = playlists.map(playlist => `
        <div class="playlist-item" onclick="showPlaylist(${playlist.id})">
            <div class="playlist-cover">
                ${playlist.coverPath ? 
                    `<img src="${playlist.coverPath}" alt="Playlist Cover" onerror="this.style.display='none'; this.nextElementSibling.style.display='block';">
                     <i class="nf nf-md-playlist_music default-icon" style="display: none;"></i>` : 
                    '<i class="nf nf-md-playlist_music default-icon"></i>'
                }
                ${playlist.isOwner ? `<button class="playlist-edit-btn" onclick="event.stopPropagation(); showEditPlaylistModal(${playlist.id})" title="Edit Playlist">
                    <i class="nf nf-md-pencil"></i>
                </button>` : ''}
            </div>
            <div class="playlist-meta">
                <div class="playlist-name">${escapeHtml(playlist.name)} ${playlist.shared ? '<i class="nf nf-md-share" title="Shared with all users"></i>' : '<i class="nf nf-md-lock" title="Private playlist"></i>'}</div>
                <div class="playlist-info">${playlist.trackCount || 0} tracks</div>
            </div>
            <div class="playlist-actions">
                <button class="btn btn-secondary" onclick="event.stopPropagation(); showPlaylist(${playlist.id})">VIEW</button>
                ${playlist.isOwner ? `
                    <button class="btn ${playlist.shared ? 'btn-warning' : 'btn-success'}" onclick="event.stopPropagation(); togglePlaylistSharing(${playlist.id}, ${playlist.shared})" title="${playlist.shared ? 'Make private' : 'Share with all users'}">
                        <i class="nf nf-md-${playlist.shared ? 'lock' : 'share'}"></i> ${playlist.shared ? 'PRIVATE' : 'SHARE'}
                    </button>
                    <button class="btn btn-danger" onclick="event.stopPropagation(); deletePlaylist(${playlist.id})">DELETE</button>
                ` : ''}
            </div>
        </div>
    `).join('');
}

// Handle search input
function handleSearch(event) {
    const query = event.target.value.toLowerCase();
    if (query === '') {
        displayTracks();
        return;
    }
    
    const filteredTracks = tracks.filter(track => 
        track.title.toLowerCase().includes(query) ||
        track.artist.toLowerCase().includes(query) ||
        track.album.toLowerCase().includes(query)
    );
    
    displayTracks(filteredTracks);
}

// Handle sort change
function handleSortChange(event) {
    const sortBy = event.target.value;
    loadTracks(sortBy === 'default' ? null : sortBy);
}

// Perform search (for dedicated search section)
function performSearch(event) {
    const query = event.target.value.toLowerCase();
    const resultsContainer = document.getElementById('search-results');
    
    if (query === '') {
        resultsContainer.innerHTML = '<div class="loading">Enter a search query</div>';
        return;
    }
    
    const filteredTracks = tracks.filter(track => 
        track.title.toLowerCase().includes(query) ||
        track.artist.toLowerCase().includes(query) ||
        track.album.toLowerCase().includes(query)
    );
    
    if (filteredTracks.length === 0) {
        resultsContainer.innerHTML = '<div class="loading">No tracks found</div>';
        return;
    }
    
    resultsContainer.innerHTML = filteredTracks.map(track => `
        <div class="track-item${currentTrackId === track.id ? ' playing' : ''}" onclick="playTrack(${track.id})">
            <div class="track-info">
                <div class="track-title">${escapeHtml(track.title)}</div>
                <div class="track-artist">${escapeHtml(track.artist)}</div>
            </div>
            <div class="track-actions">
                <button class="play-btn" onclick="event.stopPropagation(); playTrack(${track.id})">PLAY</button>
                <button class="add-to-playlist-btn" onclick="event.stopPropagation(); showAddToPlaylistModal(${track.id})">+ PLAYLIST</button>
            </div>
        </div>
    `).join('');
}

// Utility function to escape HTML
function escapeHtml(text) {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
}

// Play a track
async function playTrack(trackId) {
    console.log(`Playing track ${trackId}`);
    
    try {
        // Determine the source list: playlist or global
        let sourceList = tracks;
        
        if (currentSection === 'playlist-detail' && window.playlistTracks && Array.isArray(window.playlistTracks) && window.playlistTracks.length > 0) {
            sourceList = window.playlistTracks;
        }

        // Find the track info
        const track = sourceList.find(t => t.id === trackId);
        if (!track) {
            console.error(`Track with ID ${trackId} not found in sourceList:`, sourceList);
            return;
        }

        // Update global state
        currentTrackId = trackId;
        currentTrackIndex = sourceList.findIndex(t => t.id === trackId);
        currentTrackList = sourceList;

        // Update UI to show currently playing track
        document.querySelectorAll('.track-item').forEach(item => {
            item.classList.remove('playing');
        });

        // Find and highlight the currently playing track
        const playingTrack = document.querySelector(`[onclick*="playTrack(${trackId})"]`);
        if (playingTrack) {
            playingTrack.classList.add('playing');
        }

        // Show player
        const player = document.getElementById('player');
        if (player) {
            player.style.display = 'block';
        }

        // Update now playing info
        const nowPlayingTitle = document.getElementById('nowPlayingTitle');
        const nowPlayingArtist = document.getElementById('nowPlayingArtist');

        if (nowPlayingTitle) nowPlayingTitle.textContent = track.title;
        if (nowPlayingArtist) nowPlayingArtist.textContent = track.artist;

        // Set audio source and play
        const audioPlayer = document.getElementById('audioPlayer');
        if (audioPlayer) {
            audioPlayer.src = `/stream/${trackId}`;

            // Wait for the audio to load and then play
            audioPlayer.addEventListener('loadeddata', function onLoaded() {
                audioPlayer.removeEventListener('loadeddata', onLoaded);
                // Apply selected RPM behavior before playing
                if (typeof applyPlaybackRate === 'function') {
                    applyPlaybackRate();
                }
                audioPlayer.play().then(() => {
                    console.log('Audio started playing');

                    // Initialize media session for new track
                    if (window.mediaSessionManager) {
                        mediaSessionManager.onTrackStart(track);
                    }
                }).catch(error => {
                    console.error('Error playing audio:', error);
                });
            });

            // Update play button icon
            const playPauseBtn = document.getElementById('playPauseBtn');
            if (playPauseBtn) {
                const icon = playPauseBtn.querySelector('i');
                if (icon) icon.className = 'nf nf-md-pause';
            }
        }

    } catch (error) {
        console.error('Error playing track:', error);
    }
}

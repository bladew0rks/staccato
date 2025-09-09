// Playlist management functions

// Show create playlist modal
function showCreatePlaylistModal() {
    const modal = document.getElementById('createPlaylistModal');
    if (modal) {
        modal.style.display = 'block';
        // Clear form
        document.getElementById('playlistName').value = '';
        document.getElementById('playlistDescription').value = '';
    }
}

// Close create playlist modal
function closeCreatePlaylistModal() {
    const modal = document.getElementById('createPlaylistModal');
    if (modal) {
        modal.style.display = 'none';
    }
}

// Create playlist
async function createPlaylist(event) {
    event.preventDefault();
    
    const name = document.getElementById('playlistName').value;
    const description = document.getElementById('playlistDescription').value;
    
    if (!name.trim()) {
        alert('Please enter a playlist name');
        return;
    }
    
    try {
        const response = await fetch('/api/playlists/create', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: name.trim(), description: description.trim() })
        });
        
        if (response.ok) {
            closeCreatePlaylistModal();
            await loadPlaylists(); // Reload playlists
            showNotification('Playlist created successfully!');
        } else {
            const error = await response.text();
            alert('Failed to create playlist: ' + error);
        }
    } catch (error) {
        console.error('Error creating playlist:', error);
        alert('Error creating playlist');
    }
}

// Show add to playlist modal
function showAddToPlaylistModal(trackId) {
    selectedTrackId = trackId;
    const modal = document.getElementById('addToPlaylistModal');
    const optionsContainer = document.getElementById('playlistOptions');
    
    if (!modal || !optionsContainer) return;
    
    // Filter to only show playlists the user owns
    const ownedPlaylists = playlists.filter(playlist => playlist.isOwner);
    
    if (ownedPlaylists.length === 0) {
        optionsContainer.innerHTML = '<div class="loading">No playlists available. Create a playlist first.</div>';
    } else {
        optionsContainer.innerHTML = `
            <div style="display: grid; grid-template-columns: repeat(auto-fill, minmax(120px, 1fr)); gap: 15px;">
                ${ownedPlaylists.map(playlist => `
                    <div class="playlist-item" onclick="addTrackToPlaylist(${trackId}, ${playlist.id})" style="padding: 12px;">
                        <div class="playlist-cover" style="width: 80px; height: 80px; margin-bottom: 8px;">
                            ${playlist.coverPath ? 
                                `<img src="${playlist.coverPath}" alt="Playlist Cover" onerror="this.parentElement.innerHTML='<i class=\\"nf nf-md-playlist_music default-icon\\" style=\\"font-size: 24px;\\"></i>'">` : 
                                '<i class="nf nf-md-playlist_music default-icon" style="font-size: 24px;"></i>'
                            }
                        </div>
                        <div class="playlist-meta">
                            <div class="playlist-name" style="font-size: 11px;">${escapeHtml(playlist.name)}</div>
                            <div class="playlist-info" style="font-size: 10px;">${playlist.trackCount || 0} tracks</div>
                        </div>
                    </div>
                `).join('')}
            </div>
        `;
    }
    
    modal.style.display = 'block';
}

// Close add to playlist modal
function closeAddToPlaylistModal() {
    const modal = document.getElementById('addToPlaylistModal');
    if (modal) {
        modal.style.display = 'none';
    }
    selectedTrackId = null;
}

// Add track to playlist
async function addTrackToPlaylist(trackId, playlistId) {
    try {
        const response = await fetch(`/api/playlists/${playlistId}/tracks`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ trackId: trackId })
        });
        
        if (response.ok) {
            closeAddToPlaylistModal();
            await loadPlaylists(); // Reload to update track counts
            showNotification('Track added to playlist!');
        } else {
            const error = await response.text();
            alert('Failed to add track to playlist: ' + error);
        }
    } catch (error) {
        console.error('Error adding track to playlist:', error);
        alert('Error adding track to playlist');
    }
}

// Show playlist details
async function showPlaylist(playlistId) {
    currentPlaylistId = playlistId;
    
    try {
        const response = await fetch(`/api/playlists/${playlistId}/tracks`);
        if (response.ok) {
            window.playlistTracks = await response.json();
            const playlist = playlists.find(p => p.id === playlistId);
            
            if (playlist) {
                document.getElementById('playlist-detail-title').textContent = playlist.name;
            }
            
            const tracksContainer = document.getElementById('playlist-tracks');
            if (window.playlistTracks.length === 0) {
                tracksContainer.innerHTML = '<div class="loading">No tracks in this playlist</div>';
            } else {
                tracksContainer.innerHTML = window.playlistTracks.map(track => `
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
                            ${playlist && playlist.isOwner ? `<button class="btn btn-danger" onclick="event.stopPropagation(); removeTrackFromPlaylist(${track.id}, ${playlistId})">REMOVE</button>` : ''}
                        </div>
                    </div>
                `).join('');
            }
            
            showSection('playlist-detail');
        } else {
            console.error('Failed to load playlist tracks');
        }
    } catch (error) {
        console.error('Error loading playlist tracks:', error);
    }
}

// Remove track from playlist
async function removeTrackFromPlaylist(trackId, playlistId) {
    if (!confirm('Remove this track from the playlist?')) {
        return;
    }
    
    try {
        const response = await fetch(`/api/playlists/${playlistId}/tracks/${trackId}`, {
            method: 'DELETE'
        });
        
        if (response.ok) {
            await showPlaylist(playlistId); // Reload playlist
            await loadPlaylists(); // Update playlist counts
            showNotification('Track removed from playlist!');
        } else {
            const error = await response.text();
            alert('Failed to remove track from playlist: ' + error);
        }
    } catch (error) {
        console.error('Error removing track from playlist:', error);
        alert('Error removing track from playlist');
    }
}

// Delete playlist
async function deletePlaylist(playlistId) {
    if (!confirm('Are you sure you want to delete this playlist?')) {
        return;
    }
    
    try {
        const response = await fetch(`/api/playlists/${playlistId}`, {
            method: 'DELETE'
        });
        
        if (response.ok) {
            await loadPlaylists(); // Reload playlists
            showNotification('Playlist deleted successfully!');
            // If we're currently viewing the deleted playlist, go back to library
            if (currentSection === 'playlist-detail') {
                showSection('library');
            }
        } else {
            const error = await response.text();
            alert('Failed to delete playlist: ' + error);
        }
    } catch (error) {
        console.error('Error deleting playlist:', error);
        alert('Error deleting playlist');
    }
}

// Toggle playlist sharing
async function togglePlaylistSharing(playlistId, currentSharedStatus) {
    const newSharedStatus = !currentSharedStatus;
    const action = newSharedStatus ? 'share' : 'make private';
    
    if (!confirm(`Are you sure you want to ${action} this playlist?`)) {
        return;
    }
    
    try {
        const response = await fetch(`/api/playlists/${playlistId}/share`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ shared: newSharedStatus })
        });
        
        if (response.ok) {
            await loadPlaylists(); // Reload playlists to refresh sharing status
            const statusText = newSharedStatus ? 'shared with all users' : 'private';
            showNotification(`Playlist is now ${statusText}!`);
        } else {
            const error = await response.text();
            alert(`Failed to ${action} playlist: ` + error);
        }
    } catch (error) {
        console.error(`Error ${action}ing playlist:`, error);
        alert(`Error ${action}ing playlist`);
    }
}

// Close modals when clicking outside
window.addEventListener('click', function(event) {
    const createModal = document.getElementById('createPlaylistModal');
    const addModal = document.getElementById('addToPlaylistModal');
    const editModal = document.getElementById('editPlaylistModal');
    
    if (event.target === createModal) {
        closeCreatePlaylistModal();
    }
    
    if (event.target === addModal) {
        closeAddToPlaylistModal();
    }
    
    if (event.target === editModal) {
        closeEditPlaylistModal();
    }
});

// Edit playlist functions
let currentEditPlaylistId = null;

// Show edit playlist modal
function showEditPlaylistModal(playlistId) {
    const playlist = playlists.find(p => p.id === playlistId);
    if (!playlist) return;
    
    currentEditPlaylistId = playlistId;
    const modal = document.getElementById('editPlaylistModal');
    
    // Populate form with current values
    document.getElementById('editPlaylistName').value = playlist.name;
    document.getElementById('editPlaylistDescription').value = playlist.description || '';
    
    // Clear cover preview
    document.getElementById('coverPreview').innerHTML = '';
    document.getElementById('playlistCover').value = '';
    
    modal.style.display = 'block';
}

// Close edit playlist modal
function closeEditPlaylistModal() {
    const modal = document.getElementById('editPlaylistModal');
    if (modal) {
        modal.style.display = 'none';
    }
    currentEditPlaylistId = null;
}

// Preview playlist cover
function previewPlaylistCover(event) {
    const file = event.target.files[0];
    const preview = document.getElementById('coverPreview');
    
    if (file) {
        const reader = new FileReader();
        reader.onload = function(e) {
            preview.innerHTML = `
                <div style="text-align: center;">
                    <img src="${e.target.result}" style="max-width: 150px; max-height: 150px; border-radius: 6px;" alt="Cover Preview">
                    <p style="margin-top: 8px; font-size: 12px; color: #888;">Preview</p>
                </div>
            `;
        };
        reader.readAsDataURL(file);
    } else {
        preview.innerHTML = '';
    }
}

// Update playlist
async function updatePlaylist(event) {
    event.preventDefault();
    
    if (!currentEditPlaylistId) return;
    
    const name = document.getElementById('editPlaylistName').value;
    const description = document.getElementById('editPlaylistDescription').value;
    const coverFile = document.getElementById('playlistCover').files[0];
    
    if (!name.trim()) {
        alert('Please enter a playlist name');
        return;
    }
    
    try {
        // Create FormData for file upload
        const formData = new FormData();
        formData.append('name', name.trim());
        formData.append('description', description.trim());
        
        if (coverFile) {
            formData.append('cover', coverFile);
        }
        
        const response = await fetch(`/api/playlists/${currentEditPlaylistId}`, {
            method: 'PUT',
            body: formData
        });
        
        if (response.ok) {
            closeEditPlaylistModal();
            await loadPlaylists(); // Reload playlists
            showNotification('Playlist updated successfully!');
        } else {
            const error = await response.text();
            alert('Failed to update playlist: ' + error);
        }
    } catch (error) {
        console.error('Error updating playlist:', error);
        alert('Error updating playlist');
    }
}

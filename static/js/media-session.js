// Media Session API for MPRIS/Media Control Integration
// This provides integration with OS-level media controls (Windows Media Transport Controls, etc.)

class MediaSessionManager {
    constructor() {
        this.isSupported = 'mediaSession' in navigator;
        this.currentTrackData = null;
        this.positionUpdateInterval = null;
        this.init();
    }

    init() {
        if (!this.isSupported) {
            console.warn('Media Session API not supported in this browser');
            return;
        }

        console.log('Media Session API initialized');
        this.setupActionHandlers();
        this.setupKeyboardListeners();
    }

    setupActionHandlers() {
        try {
            // Play/Pause controls
            navigator.mediaSession.setActionHandler('play', () => {
                console.log('Media Session: Play action triggered');
                this.handlePlay();
            });

            navigator.mediaSession.setActionHandler('pause', () => {
                console.log('Media Session: Pause action triggered');
                this.handlePause();
            });

            // Previous/Next track controls
            navigator.mediaSession.setActionHandler('previoustrack', () => {
                console.log('Media Session: Previous track action triggered');
                this.handlePreviousTrack();
            });

            navigator.mediaSession.setActionHandler('nexttrack', () => {
                console.log('Media Session: Next track action triggered');
                this.handleNextTrack();
            });

            // Seek controls
            navigator.mediaSession.setActionHandler('seekbackward', (details) => {
                console.log('Media Session: Seek backward action triggered', details);
                this.handleSeekBackward(details.seekOffset || 10);
            });

            navigator.mediaSession.setActionHandler('seekforward', (details) => {
                console.log('Media Session: Seek forward action triggered', details);
                this.handleSeekForward(details.seekOffset || 10);
            });

            // Position control
            navigator.mediaSession.setActionHandler('seekto', (details) => {
                console.log('Media Session: Seek to action triggered', details);
                this.handleSeekTo(details.seekTime);
            });

            // Stop control
            navigator.mediaSession.setActionHandler('stop', () => {
                console.log('Media Session: Stop action triggered');
                this.handleStop();
            });

        } catch (error) {
            console.error('Error setting up media session action handlers:', error);
        }
    }

    setupKeyboardListeners() {
        // Listen for hardware media keys
        document.addEventListener('keydown', (event) => {
            // Only handle media keys, not regular keys
            switch(event.code) {
                case 'MediaPlayPause':
                    event.preventDefault();
                    this.handlePlay();
                    break;
                case 'MediaTrackNext':
                    event.preventDefault();
                    this.handleNextTrack();
                    break;
                case 'MediaTrackPrevious':
                    event.preventDefault();
                    this.handlePreviousTrack();
                    break;
                case 'MediaStop':
                    event.preventDefault();
                    this.handleStop();
                    break;
            }
        });
    }

    updateMetadata(track) {
        if (!this.isSupported || !track) return;

        this.currentTrackData = track;

        try {
            navigator.mediaSession.metadata = new MediaMetadata({
                title: track.title || 'Unknown Title',
                artist: track.artist || 'Unknown Artist',
                album: track.album || 'Unknown Album',
                artwork: this.getArtworkArray(track)
            });

            console.log('Media Session metadata updated:', {
                title: track.title,
                artist: track.artist,
                album: track.album
            });

        } catch (error) {
            console.error('Error updating media session metadata:', error);
        }
    }

    getArtworkArray(track) {
        // Use the existing albumart endpoint if track has albumArtId
        if (!track.albumArtId) {
            return []; // No artwork available
        }
        
        const baseUrl = window.location.origin;
        const artworkSizes = [96, 128, 192, 256, 384, 512];
        
        // Use the server's existing albumart endpoint
        return artworkSizes.map(size => ({
            src: `${baseUrl}/albumart/${track.albumArtId}`,
            sizes: `${size}x${size}`,
            type: 'image/jpeg'
        }));
    }

    updatePlaybackState(state) {
        if (!this.isSupported) return;

        try {
            navigator.mediaSession.playbackState = state;
            console.log('Media Session playback state updated:', state);
        } catch (error) {
            console.error('Error updating media session playback state:', error);
        }
    }

    updatePositionState(duration, position, playbackRate = 1.0) {
        if (!this.isSupported) return;

        try {
            if (duration > 0 && position >= 0 && position <= duration) {
                navigator.mediaSession.setPositionState({
                    duration: duration,
                    playbackRate: playbackRate,
                    position: position
                });
            }
        } catch (error) {
            console.error('Error updating media session position state:', error);
        }
    }

    startPositionUpdates() {
        this.stopPositionUpdates();
        
        this.positionUpdateInterval = setInterval(() => {
            const audioPlayer = document.getElementById('audioPlayer');
            if (audioPlayer && !audioPlayer.paused && audioPlayer.duration > 0) {
                const rate = typeof rpm33Mode !== 'undefined' && rpm33Mode ? 0.7407407407 : 1.0;
                this.updatePositionState(audioPlayer.duration, audioPlayer.currentTime, rate);
            }
        }, 1000); // Update every second
    }

    stopPositionUpdates() {
        if (this.positionUpdateInterval) {
            clearInterval(this.positionUpdateInterval);
            this.positionUpdateInterval = null;
        }
    }

    // Action handlers that delegate to existing player functions
    handlePlay() {
        const audioPlayer = document.getElementById('audioPlayer');
        if (audioPlayer && audioPlayer.src) {
            if (audioPlayer.paused) {
                togglePlayPause();
            }
        }
    }

    handlePause() {
        const audioPlayer = document.getElementById('audioPlayer');
        if (audioPlayer && !audioPlayer.paused) {
            togglePlayPause();
        }
    }

    handlePreviousTrack() {
        if (typeof playPreviousTrack === 'function') {
            playPreviousTrack();
        }
    }

    handleNextTrack() {
        if (typeof playNextTrack === 'function') {
            playNextTrack();
        }
    }

    handleSeekBackward(offset) {
        const audioPlayer = document.getElementById('audioPlayer');
        if (audioPlayer && audioPlayer.duration) {
            const newTime = Math.max(0, audioPlayer.currentTime - offset);
            audioPlayer.currentTime = newTime;
            this.updatePositionState(audioPlayer.duration, newTime);
        }
    }

    handleSeekForward(offset) {
        const audioPlayer = document.getElementById('audioPlayer');
        if (audioPlayer && audioPlayer.duration) {
            const newTime = Math.min(audioPlayer.duration, audioPlayer.currentTime + offset);
            audioPlayer.currentTime = newTime;
            this.updatePositionState(audioPlayer.duration, newTime);
        }
    }

    handleSeekTo(seekTime) {
        const audioPlayer = document.getElementById('audioPlayer');
        if (audioPlayer && audioPlayer.duration) {
            audioPlayer.currentTime = seekTime;
            this.updatePositionState(audioPlayer.duration, seekTime);
        }
    }

    handleStop() {
        const audioPlayer = document.getElementById('audioPlayer');
        if (audioPlayer) {
            audioPlayer.pause();
            audioPlayer.currentTime = 0;
            this.updatePlaybackState('none');
            this.stopPositionUpdates();
            
            // Hide player UI
            const player = document.getElementById('player');
            if (player) {
                player.style.display = 'none';
            }
            
            // Reset track info
            if (typeof window !== 'undefined') {
                window.currentTrackId = null;
                window.currentTrackIndex = -1;
            }
            
            // Update UI
            document.querySelectorAll('.track-item').forEach(item => {
                item.classList.remove('playing');
            });

            // Update play button
            const playPauseBtn = document.getElementById('playPauseBtn');
            if (playPauseBtn) {
                const icon = playPauseBtn.querySelector('i');
                if (icon) icon.className = 'nf nf-md-play';
            }
        }
    }

    // Called when track starts playing
    onTrackStart(track) {
        this.updateMetadata(track);
        this.updatePlaybackState('playing');
        this.startPositionUpdates();
        
        const audioPlayer = document.getElementById('audioPlayer');
        if (audioPlayer) {
            const rate = typeof rpm33Mode !== 'undefined' && rpm33Mode ? 0.7407407407 : 1.0;
            this.updatePositionState(audioPlayer.duration || 0, audioPlayer.currentTime || 0, rate);
        }
    }

    // Called when track is paused
    onTrackPause() {
        this.updatePlaybackState('paused');
        this.stopPositionUpdates();
    }

    // Called when track is resumed
    onTrackResume() {
        this.updatePlaybackState('playing');
        this.startPositionUpdates();
    }

    // Called when track ends
    onTrackEnd() {
        this.updatePlaybackState('none');
        this.stopPositionUpdates();
    }

    // Called periodically to update position
    onTimeUpdate(currentTime, duration) {
        if (this.isSupported && duration > 0) {
            const rate = typeof rpm33Mode !== 'undefined' && rpm33Mode ? 0.7407407407 : 1.0;
            this.updatePositionState(duration, currentTime, rate);
        }
    }

    // Get current status for debugging
    getStatus() {
        return {
            supported: this.isSupported,
            currentTrack: this.currentTrackData,
            playbackState: this.isSupported ? navigator.mediaSession.playbackState : 'unknown'
        };
    }
}

// Create global instance
window.mediaSessionManager = new MediaSessionManager();

// Auto-initialize when DOM is ready
document.addEventListener('DOMContentLoaded', function() {
    console.log('Media Session Manager ready');
    console.log('Media Session Status:', window.mediaSessionManager.getStatus());
});

// Clean up media session when page is unloaded
window.addEventListener('beforeunload', () => {
    if (window.mediaSessionManager) {
        window.mediaSessionManager.stopPositionUpdates();
    }
});

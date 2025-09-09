// Audio player functions

// Enhanced seeker & volume interaction state
let isSeeking = false;
let seekWasPlaying = false;
let isAdjustingVolume = false;

function clamp(val, min, max) { return Math.min(max, Math.max(min, val)); }

// Setup keyboard shortcuts
function setupKeyboardShortcuts() {
    document.addEventListener('keydown', function(event) {
        // Only handle shortcuts when not typing in an input field
        if (event.target.tagName === 'INPUT' || event.target.tagName === 'TEXTAREA') {
            return;
        }
        
        switch(event.code) {
            case 'ArrowRight':
            case 'KeyN':
                event.preventDefault();
                playNextTrack();
                break;
            case 'ArrowLeft':
            case 'KeyP':
                event.preventDefault();
                playPreviousTrack();
                break;
            case 'Space':
                event.preventDefault();
                togglePlayPause();
                break;
        }
    });
}

// Toggle play/pause
function togglePlayPause() {
    const audioPlayer = document.getElementById('audioPlayer');
    const playPauseBtn = document.getElementById('playPauseBtn');
    const icon = playPauseBtn.querySelector('i');
    
    if (audioPlayer.src) {
        if (audioPlayer.paused) {
            audioPlayer.play();
            icon.className = 'nf nf-md-pause';
            // Update media session
            if (window.mediaSessionManager) {
                mediaSessionManager.onTrackResume();
            }
        } else {
            audioPlayer.pause();
            icon.className = 'nf nf-md-play';
            // Update media session
            if (window.mediaSessionManager) {
                mediaSessionManager.onTrackPause();
            }
        }
    }
}

// Toggle shuffle mode
function toggleShuffle() {
    isShuffled = !isShuffled;
    const shuffleBtn = document.getElementById('shuffleBtn');
    
    if (isShuffled) {
        shuffleBtn.classList.add('active');
        shuffleBtn.title = 'Shuffle On';
    } else {
        shuffleBtn.classList.remove('active');
        shuffleBtn.title = 'Shuffle Off';
    }
}

// Toggle repeat mode (off -> playlist -> track -> off)
function toggleRepeat() {
    repeatMode = (repeatMode + 1) % 3;
    const repeatBtn = document.getElementById('repeatBtn');
    const icon = repeatBtn.querySelector('i');
    
    switch (repeatMode) {
        case 0:
            repeatBtn.classList.remove('active');
            icon.className = 'nf nf-md-repeat';
            repeatBtn.title = 'Repeat Off';
            break;
        case 1:
            repeatBtn.classList.add('active');
            icon.className = 'nf nf-md-repeat';
            repeatBtn.title = 'Repeat Playlist';
            break;
        case 2:
            repeatBtn.classList.add('active');
            icon.className = 'nf nf-md-repeat_once';
            repeatBtn.title = 'Repeat Track';
            break;
    }
}

// Set volume from slider click
function setVolume(event) {
    const audioPlayer = document.getElementById('audioPlayer');
    const volumeSlider = document.getElementById('volumeSlider');
    const volumeFill = document.getElementById('volumeFill');
    const volumeBtn = document.getElementById('volumeBtn');
    const icon = volumeBtn.querySelector('i');
    
    const rect = volumeSlider.getBoundingClientRect();
    const clickX = event.clientX - rect.left;
    const percentage = Math.max(0, Math.min(1, clickX / rect.width));
    
    audioPlayer.volume = percentage;
    volumeFill.style.width = (percentage * 100) + '%';
    
    // Update volume icon
    if (percentage === 0) {
        icon.className = 'nf nf-md-volume_off';
        isMuted = true;
    } else {
        isMuted = false;
        if (percentage > 0.5) {
            icon.className = 'nf nf-md-volume_high';
        } else {
            icon.className = 'nf nf-md-volume_medium';
        }
    }
    
    lastVolume = percentage > 0 ? percentage : lastVolume;
}

// Toggle mute
function toggleMute() {
    const audioPlayer = document.getElementById('audioPlayer');
    const volumeBtn = document.getElementById('volumeBtn');
    const volumeFill = document.getElementById('volumeFill');
    const icon = volumeBtn.querySelector('i');
    
    if (isMuted) {
        audioPlayer.volume = lastVolume;
        volumeFill.style.width = (lastVolume * 100) + '%';
        icon.className = lastVolume > 0.5 ? 'nf nf-md-volume_high' : 'nf nf-md-volume_medium';
        isMuted = false;
    } else {
        lastVolume = audioPlayer.volume;
        audioPlayer.volume = 0;
        volumeFill.style.width = '0%';
        icon.className = 'nf nf-md-volume_off';
        isMuted = true;
    }
}

// Set volume from slider click
function setVolume(event) {
    const volumeSlider = event.currentTarget;
    const rect = volumeSlider.getBoundingClientRect();
    const percentage = (event.clientX - rect.left) / rect.width;
    const volume = Math.max(0, Math.min(1, percentage));
    
    const audioPlayer = document.getElementById('audioPlayer');
    const volumeFill = document.getElementById('volumeFill');
    const volumeBtn = document.getElementById('volumeBtn');
    const icon = volumeBtn.querySelector('i');
    
    audioPlayer.volume = volume;
    volumeFill.style.width = (volume * 100) + '%';
    
    if (volume === 0) {
        icon.className = 'nf nf-md-volume_off';
        isMuted = true;
    } else {
        icon.className = volume > 0.5 ? 'nf nf-md-volume_high' : 'nf nf-md-volume_medium';
        isMuted = false;
        lastVolume = volume;
    }
}

// Seek to position in track
function seekToPosition(event) {
    const progressBar = event.currentTarget;
    const rect = progressBar.getBoundingClientRect();
    const percentage = (event.clientX - rect.left) / rect.width;
    const audioPlayer = document.getElementById('audioPlayer');
    
    if (audioPlayer.duration) {
        audioPlayer.currentTime = audioPlayer.duration * percentage;
    }
}

// Format time for display
function formatTime(seconds) {
    if (isNaN(seconds)) return '0:00';
    const minutes = Math.floor(seconds / 60);
    const remainingSeconds = Math.floor(seconds % 60);
    return `${minutes}:${remainingSeconds.toString().padStart(2, '0')}`;
}

// Update progress bar and time display
function updateProgress() {
    const audioPlayer = document.getElementById('audioPlayer');
    const progressFill = document.getElementById('progressFill');
    const progressHandle = document.getElementById('progressHandle');
    const progressBuffer = document.getElementById('progressBuffer');
    const currentTimeSpan = document.getElementById('currentTime');
    const totalTimeSpan = document.getElementById('totalTime');
    
    if (audioPlayer.duration) {
        if (!isSeeking) { // freeze visual fill while actively dragging
            const percentage = (audioPlayer.currentTime / audioPlayer.duration) * 100;
            progressFill.style.width = percentage + '%';
            if (progressHandle) {
                progressHandle.style.left = percentage + '%';
            }
        }
        currentTimeSpan.textContent = formatTime(audioPlayer.currentTime);
        totalTimeSpan.textContent = formatTime(audioPlayer.duration);
        // buffered ranges (show the furthest buffered point that covers currentTime)
        if (progressBuffer && audioPlayer.buffered && audioPlayer.buffered.length) {
            let bufferedEnd = 0;
            for (let i = 0; i < audioPlayer.buffered.length; i++) {
                const start = audioPlayer.buffered.start(i);
                const end = audioPlayer.buffered.end(i);
                if (audioPlayer.currentTime >= start && audioPlayer.currentTime <= end) {
                    bufferedEnd = end; break;
                }
                if (end > bufferedEnd) bufferedEnd = end; // fallback: max end
            }
            const bufPct = clamp((bufferedEnd / audioPlayer.duration) * 100, 0, 100);
            progressBuffer.style.width = bufPct + '%';
        }
        
        // Update media session position
        if (window.mediaSessionManager) {
            mediaSessionManager.onTimeUpdate(audioPlayer.currentTime, audioPlayer.duration);
        }
    }
}

// Seek to position when progress bar is clicked
function seekToPosition(event) {
    const audioPlayer = document.getElementById('audioPlayer');
    const progressBar = document.getElementById('progressBar');
    
    if (audioPlayer.duration) {
        const rect = progressBar.getBoundingClientRect();
        const clickX = event.clientX - rect.left;
        const percentage = clickX / rect.width;
        const newTime = percentage * audioPlayer.duration;
        
        audioPlayer.currentTime = newTime;
        updateProgress();
    }
}

// Pointer-based scrubbing (drag seek)
function initSeekerDrag() {
    const progressBar = document.getElementById('progressBar');
    if (!progressBar) return;
    const audioPlayer = document.getElementById('audioPlayer');
    const progressFill = document.getElementById('progressFill');
    const progressHandle = document.getElementById('progressHandle');

    const updateFromClientX = (clientX) => {
        if (!audioPlayer.duration) return;
        const rect = progressBar.getBoundingClientRect();
        const pct = clamp((clientX - rect.left) / rect.width, 0, 1);
        const pct100 = pct * 100;
        progressFill.style.width = pct100 + '%';
        if (progressHandle) progressHandle.style.left = pct100 + '%';
        const previewTime = audioPlayer.duration * pct;
        // live time preview
        const currentTimeSpan = document.getElementById('currentTime');
        if (currentTimeSpan) currentTimeSpan.textContent = formatTime(previewTime);
        progressBar.setAttribute('aria-valuenow', Math.round(pct * 100));
        progressBar.setAttribute('data-preview-time', previewTime);
    };

    const onPointerDown = (e) => {
        if (!audioPlayer.duration) return;
        isSeeking = true;
        progressBar.classList.add('dragging');
        seekWasPlaying = !audioPlayer.paused;
        if (seekWasPlaying) audioPlayer.pause();
        progressBar.setPointerCapture(e.pointerId);
        updateFromClientX(e.clientX);
        e.preventDefault();
    };
    const onPointerMove = (e) => {
        if (!isSeeking) return;
        updateFromClientX(e.clientX);
    };
    const onPointerUp = (e) => {
        if (!isSeeking) return;
        const previewTime = parseFloat(progressBar.getAttribute('data-preview-time') || '0');
        if (!isNaN(previewTime)) {
            audioPlayer.currentTime = previewTime;
        }
        isSeeking = false;
        progressBar.classList.remove('dragging');
        progressBar.releasePointerCapture(e.pointerId);
        if (seekWasPlaying) audioPlayer.play();
        updateProgress();
    };

    progressBar.addEventListener('pointerdown', onPointerDown);
    progressBar.addEventListener('pointermove', onPointerMove);
    progressBar.addEventListener('pointerup', onPointerUp);
    progressBar.addEventListener('pointerleave', (e) => { if (isSeeking) onPointerUp(e); });

    // Keyboard accessibility (when focused)
    progressBar.addEventListener('keydown', (e) => {
        if (!audioPlayer.duration) return;
        let delta = 0;
        if (e.key === 'ArrowLeft') delta = -5;
        else if (e.key === 'ArrowRight') delta = 5;
        else if (e.key === 'Home') { audioPlayer.currentTime = 0; updateProgress(); e.preventDefault(); return; }
        else if (e.key === 'End') { audioPlayer.currentTime = audioPlayer.duration; updateProgress(); e.preventDefault(); return; }
        else if (e.key === 'PageUp') delta = 15;
        else if (e.key === 'PageDown') delta = -15;
        if (delta !== 0) {
            e.preventDefault();
            e.stopPropagation();
            audioPlayer.currentTime = clamp(audioPlayer.currentTime + delta, 0, audioPlayer.duration);
            updateProgress();
        }
    });
}

// Volume dragging
function initVolumeDrag() {
    const volumeSlider = document.getElementById('volumeSlider');
    if (!volumeSlider) return;
    const audioPlayer = document.getElementById('audioPlayer');
    const volumeFill = document.getElementById('volumeFill');
    const volumeHandle = document.getElementById('volumeHandle');
    const volumeBtn = document.getElementById('volumeBtn');

    const applyVolumeFromX = (clientX) => {
        const rect = volumeSlider.getBoundingClientRect();
        const pct = clamp((clientX - rect.left) / rect.width, 0, 1);
        audioPlayer.volume = pct;
        volumeFill.style.width = (pct * 100) + '%';
        if (volumeHandle) volumeHandle.style.left = (pct * 100) + '%';
        if (pct === 0) {
            volumeBtn.querySelector('i').className = 'nf nf-md-volume_off';
            isMuted = true;
        } else {
            isMuted = false;
            volumeBtn.querySelector('i').className = pct > 0.5 ? 'nf nf-md-volume_high' : 'nf nf-md-volume_medium';
            lastVolume = pct;
        }
        volumeSlider.setAttribute('aria-valuenow', Math.round(pct * 100));
    };

    const onPointerDown = (e) => {
        isAdjustingVolume = true;
        volumeSlider.classList.add('dragging');
        volumeSlider.setPointerCapture(e.pointerId);
        applyVolumeFromX(e.clientX);
        e.preventDefault();
    };
    const onPointerMove = (e) => { if (isAdjustingVolume) applyVolumeFromX(e.clientX); };
    const onPointerUp = (e) => {
        if (!isAdjustingVolume) return;
        isAdjustingVolume = false;
        volumeSlider.classList.remove('dragging');
        volumeSlider.releasePointerCapture(e.pointerId);
    };

    volumeSlider.addEventListener('pointerdown', onPointerDown);
    volumeSlider.addEventListener('pointermove', onPointerMove);
    volumeSlider.addEventListener('pointerup', onPointerUp);
    volumeSlider.addEventListener('pointerleave', (e) => { if (isAdjustingVolume) onPointerUp(e); });

    // Keyboard control
    volumeSlider.addEventListener('keydown', (e) => {
        let step = 0;
        if (e.key === 'ArrowLeft' || e.key === 'ArrowDown') step = -0.05;
        else if (e.key === 'ArrowRight' || e.key === 'ArrowUp') step = 0.05;
        else if (e.key === 'Home') { audioPlayer.volume = 0; step = 0; }
        else if (e.key === 'End') { audioPlayer.volume = 1; step = 0; }
        if (step !== 0) {
            e.preventDefault();
            audioPlayer.volume = clamp(audioPlayer.volume + step, 0, 1);
        }
        const pct = audioPlayer.volume;
        volumeFill.style.width = (pct * 100) + '%';
        if (volumeHandle) volumeHandle.style.left = (pct * 100) + '%';
        volumeSlider.setAttribute('aria-valuenow', Math.round(pct * 100));
    });
}

// Play next track
function playNextTrack() {
    if (!Array.isArray(currentTrackList) || currentTrackList.length === 0 || currentTrackIndex === -1) {
        console.log('No tracks to play next');
        return;
    }

    let nextIndex;

    if (isShuffled) {
        // Random next track
        do {
            nextIndex = Math.floor(Math.random() * currentTrackList.length);
        } while (nextIndex === currentTrackIndex && currentTrackList.length > 1);
    } else {
        // Sequential next track
        nextIndex = currentTrackIndex + 1;

        if (nextIndex >= currentTrackList.length) {
            if (repeatMode === 1) { // Repeat playlist
                nextIndex = 0;
            } else {
                console.log('End of playlist reached');
                return;
            }
        }
    }

    if (nextIndex < currentTrackList.length && nextIndex >= 0) {
        playTrack(currentTrackList[nextIndex].id);
    }
}

// Play previous track
function playPreviousTrack() {
    if (!Array.isArray(currentTrackList) || currentTrackList.length === 0 || currentTrackIndex === -1) {
        console.log('No tracks to play previous');
        return;
    }

    let prevIndex = currentTrackIndex - 1;

    if (prevIndex < 0) {
        if (repeatMode === 1) { // Repeat playlist
            prevIndex = currentTrackList.length - 1;
        } else {
            console.log('At beginning of playlist');
            return;
        }
    }

    if (prevIndex < currentTrackList.length && prevIndex >= 0) {
        playTrack(currentTrackList[prevIndex].id);
    }
}

// Setup audio player event listeners
document.addEventListener('DOMContentLoaded', function() {
    const audioPlayer = document.getElementById('audioPlayer');
    if (audioPlayer) {
        audioPlayer.addEventListener('timeupdate', updateProgress);
        audioPlayer.addEventListener('loadedmetadata', updateProgress);
        // Ensure playback rate/pitch is applied on metadata load
        audioPlayer.addEventListener('loadedmetadata', function() {
            applyPlaybackRate();
        });
        
        // Handle track ending
        audioPlayer.addEventListener('ended', function() {
            // Update media session
            if (window.mediaSessionManager) {
                mediaSessionManager.onTrackEnd();
            }
            
            if (repeatMode === 2) { // Repeat track
                audioPlayer.currentTime = 0;
                audioPlayer.play();
                // Update media session for repeated track
                if (window.mediaSessionManager) {
                    mediaSessionManager.onTrackResume();
                }
            } else {
                playNextTrack();
            }
        });
        
        // Handle audio errors
        audioPlayer.addEventListener('error', function(e) {
            console.error('Audio player error:', e);
            const errorMsg = 'Error playing audio. Please try another track.';
            // You could show this error to the user in the UI
        });
        
        // Handle when audio starts playing
        audioPlayer.addEventListener('play', function() {
            if (window.mediaSessionManager && currentTrackId) {
                const track = tracks.find(t => t.id === currentTrackId);
                if (track) {
                    mediaSessionManager.onTrackStart(track);
                }
            }
            // Re-apply desired playback rate and pitch behavior
            applyPlaybackRate();
        });
        
        // Handle when audio is paused
        audioPlayer.addEventListener('pause', function() {
            if (window.mediaSessionManager) {
                mediaSessionManager.onTrackPause();
            }
        });
    }
    initSeekerDrag();
    initVolumeDrag();
});

// Toggle 33⅓ RPM mode (simulate 33 rpm vs typical 45 rpm playback speed)
function toggleRpm33() {
    rpm33Mode = !rpm33Mode;
    const btn = document.getElementById('rpmBtn');
    if (btn) {
        btn.classList.toggle('active', rpm33Mode);
        btn.title = rpm33Mode ? '33⅓ RPM On' : '33⅓ RPM Off';
    }
    applyPlaybackRate();
}

// Apply playbackRate and pitch behavior to simulate turntable speed
function applyPlaybackRate() {
    const audioPlayer = document.getElementById('audioPlayer');
    if (!audioPlayer) return;

    // 33⅓ vs 45 RPM ratio ≈ 0.7407407407; when off, use normal 1.0
    const rate = rpm33Mode ? 0.7407407407 : 1.0;
    try {
        audioPlayer.playbackRate = rate;
    } catch (_) { /* noop */ }

    try {
        // We want pitch to drop when slowed: disable pitch preservation in this mode
        if ('preservesPitch' in audioPlayer) audioPlayer.preservesPitch = !rpm33Mode;
        if ('webkitPreservesPitch' in audioPlayer) audioPlayer.webkitPreservesPitch = !rpm33Mode;
        if ('mozPreservesPitch' in audioPlayer) audioPlayer.mozPreservesPitch = !rpm33Mode;
    } catch (_) { /* noop */ }

    // Update media session with current rate
    if (window.mediaSessionManager && audioPlayer.duration) {
        mediaSessionManager.updatePositionState(audioPlayer.duration, audioPlayer.currentTime, rate);
    }

    // Reflect button state on first run
    const btn = document.getElementById('rpmBtn');
    if (btn) btn.classList.toggle('active', rpm33Mode);
}

// Expose for inline HTML handlers
if (typeof window !== 'undefined') {
    window.toggleRpm33 = toggleRpm33;
    window.applyPlaybackRate = applyPlaybackRate;
}

package server

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"staccato/pkg/models"
)

// handleGetPlaylists returns all playlists (with track counts) as JSON.
func (ms *MusicServer) handleGetPlaylists(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")

	// Check if auth and user folders are both enabled and get current user
	authService := ms.authService
	userFolderManager := authService.GetUserFolderManager()
	var currentUser string
	if authService.IsEnabled() && userFolderManager.IsEnabled() {
		if user := r.Context().Value(UserContextKey); user != nil {
			currentUser = user.(string)
		}
	}

	// Determine which playlists to show:
	// - If auth is enabled AND user folders are enabled AND we have a current user: show user's playlists + shared
	// - Otherwise: show only main library playlists (excludes user playlists) + shared
	showUserPlaylists := authService.IsEnabled() && userFolderManager.IsEnabled() && currentUser != ""

	var playlists []models.Playlist
	var err error
	if showUserPlaylists {
		playlists, err = ms.db.GetPlaylistsForUser(currentUser)
	} else {
		playlists, err = ms.db.GetMainLibraryPlaylists()
	}

	if err != nil {
		http.Error(w, "Error retrieving playlists", http.StatusInternalServerError)
		return
	}

	// Set IsOwner field for each playlist
	for i := range playlists {
		if showUserPlaylists && currentUser != "" {
			playlists[i].IsOwner = playlists[i].Owner == currentUser
		} else {
			// When auth is disabled or no user, consider all playlists as "owned" for editing
			playlists[i].IsOwner = true
		}
	}

	json.NewEncoder(w).Encode(playlists)
}

// handleCreatePlaylist creates a new playlist (POST json name/description).
func (ms *MusicServer) handleCreatePlaylist(w http.ResponseWriter, r *http.Request) {
	if r.Method != "POST" {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	w.Header().Set("Content-Type", "application/json")

	var req struct {
		Name        string `json:"name"`
		Description string `json:"description"`
	}

	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "Invalid JSON", http.StatusBadRequest)
		return
	}

	if req.Name == "" {
		http.Error(w, "Playlist name is required", http.StatusBadRequest)
		return
	}

	// Get current user for ownership
	authService := ms.authService
	userFolderManager := authService.GetUserFolderManager()
	var currentUser string
	if authService.IsEnabled() && userFolderManager.IsEnabled() {
		if user := r.Context().Value(UserContextKey); user != nil {
			currentUser = user.(string)
		}
	}

	// Default to not shared
	id, err := ms.db.CreatePlaylist(req.Name, req.Description, currentUser, false)
	if err != nil {
		http.Error(w, "Error creating playlist", http.StatusInternalServerError)
		return
	}

	response := map[string]interface{}{
		"id":      id,
		"message": "Playlist created successfully",
	}
	json.NewEncoder(w).Encode(response)
}

// handleGetPlaylistTracks returns tracks contained in the specified playlist.
func (ms *MusicServer) handleGetPlaylistTracks(w http.ResponseWriter, r *http.Request) {
	pathParts := strings.Split(r.URL.Path, "/")
	if len(pathParts) < 4 {
		http.Error(w, "Invalid playlist ID", http.StatusBadRequest)
		return
	}

	playlistID, err := strconv.Atoi(pathParts[3])
	if err != nil {
		http.Error(w, "Invalid playlist ID", http.StatusBadRequest)
		return
	}

	// Check playlist access
	authService := ms.authService
	userFolderManager := authService.GetUserFolderManager()
	var currentUser string
	if authService.IsEnabled() && userFolderManager.IsEnabled() {
		if user := r.Context().Value(UserContextKey); user != nil {
			currentUser = user.(string)
		}
	}

	// Check if user can access this playlist
	canAccess := true
	if authService.IsEnabled() && userFolderManager.IsEnabled() && currentUser != "" {
		canAccess, err = ms.db.CheckPlaylistAccess(playlistID, currentUser)
		if err != nil {
			http.Error(w, "Error checking playlist access", http.StatusInternalServerError)
			return
		}
	}

	if !canAccess {
		http.Error(w, "Access denied", http.StatusForbidden)
		return
	}

	w.Header().Set("Content-Type", "application/json")

	tracks, err := ms.db.GetPlaylistTracks(playlistID)
	if err != nil {
		http.Error(w, "Error retrieving playlist tracks", http.StatusInternalServerError)
		return
	}

	json.NewEncoder(w).Encode(tracks)
}

// handleAddTrackToPlaylist appends a track to a playlist (POST json trackId).
func (ms *MusicServer) handleAddTrackToPlaylist(w http.ResponseWriter, r *http.Request) {
	if r.Method != "POST" {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	pathParts := strings.Split(r.URL.Path, "/")
	if len(pathParts) < 4 {
		http.Error(w, "Invalid playlist ID", http.StatusBadRequest)
		return
	}

	playlistID, err := strconv.Atoi(pathParts[3])
	if err != nil {
		http.Error(w, "Invalid playlist ID", http.StatusBadRequest)
		return
	}

	var req struct {
		TrackID int `json:"trackId"`
	}

	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "Invalid JSON", http.StatusBadRequest)
		return
	}

	// Check playlist access for adding tracks
	authService := ms.authService
	userFolderManager := authService.GetUserFolderManager()
	var currentUser string
	if authService.IsEnabled() && userFolderManager.IsEnabled() {
		if user := r.Context().Value(UserContextKey); user != nil {
			currentUser = user.(string)
		}

		// Only allow adding tracks if user owns the playlist (for now, can extend to shared playlists later)
		isOwner, err := ms.db.CheckPlaylistOwnership(playlistID, currentUser)
		if err != nil {
			http.Error(w, "Error checking playlist ownership", http.StatusInternalServerError)
			return
		}

		if !isOwner {
			http.Error(w, "Access denied - you can only add tracks to your own playlists", http.StatusForbidden)
			return
		}
	}

	err = ms.db.AddTrackToPlaylist(playlistID, req.TrackID)
	if err != nil {
		http.Error(w, "Error adding track to playlist", http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]string{"message": "Track added to playlist"})
}

// handleRemoveTrackFromPlaylist removes track from playlist (DELETE route).
func (ms *MusicServer) handleRemoveTrackFromPlaylist(w http.ResponseWriter, r *http.Request) {
	if r.Method != "DELETE" {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	pathParts := strings.Split(r.URL.Path, "/")
	if len(pathParts) < 6 {
		http.Error(w, "Invalid playlist or track ID", http.StatusBadRequest)
		return
	}

	playlistID, err := strconv.Atoi(pathParts[3])
	if err != nil {
		http.Error(w, "Invalid playlist ID", http.StatusBadRequest)
		return
	}

	trackID, err := strconv.Atoi(pathParts[5])
	if err != nil {
		http.Error(w, "Invalid track ID", http.StatusBadRequest)
		return
	}

	// Check playlist access for removing tracks
	authService := ms.authService
	userFolderManager := authService.GetUserFolderManager()
	var currentUser string
	if authService.IsEnabled() && userFolderManager.IsEnabled() {
		if user := r.Context().Value(UserContextKey); user != nil {
			currentUser = user.(string)
		}

		// Only allow removing tracks if user owns the playlist
		isOwner, err := ms.db.CheckPlaylistOwnership(playlistID, currentUser)
		if err != nil {
			http.Error(w, "Error checking playlist ownership", http.StatusInternalServerError)
			return
		}

		if !isOwner {
			http.Error(w, "Access denied - you can only remove tracks from your own playlists", http.StatusForbidden)
			return
		}
	}

	err = ms.db.RemoveTrackFromPlaylist(playlistID, trackID)
	if err != nil {
		http.Error(w, "Error removing track from playlist", http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]string{"message": "Track removed from playlist"})
}

// handleDeletePlaylist deletes a playlist (DELETE).
func (ms *MusicServer) handleDeletePlaylist(w http.ResponseWriter, r *http.Request) {
	if r.Method != "DELETE" {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	pathParts := strings.Split(r.URL.Path, "/")
	if len(pathParts) < 4 {
		http.Error(w, "Invalid playlist ID", http.StatusBadRequest)
		return
	}

	playlistID, err := strconv.Atoi(pathParts[3])
	if err != nil {
		http.Error(w, "Invalid playlist ID", http.StatusBadRequest)
		return
	}

	// Check playlist ownership for delete operations
	authService := ms.authService
	userFolderManager := authService.GetUserFolderManager()
	var currentUser string
	if authService.IsEnabled() && userFolderManager.IsEnabled() {
		if user := r.Context().Value(UserContextKey); user != nil {
			currentUser = user.(string)
		}

		// Only allow deletion if user owns the playlist
		isOwner, err := ms.db.CheckPlaylistOwnership(playlistID, currentUser)
		if err != nil {
			http.Error(w, "Error checking playlist ownership", http.StatusInternalServerError)
			return
		}

		if !isOwner {
			http.Error(w, "Access denied - you can only delete your own playlists", http.StatusForbidden)
			return
		}
	}

	err = ms.db.DeletePlaylist(playlistID)
	if err != nil {
		http.Error(w, "Error deleting playlist", http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]string{"message": "Playlist deleted"})
}

// handleUpdatePlaylist updates playlist name/description and optional cover image (multipart PUT).
func (ms *MusicServer) handleUpdatePlaylist(w http.ResponseWriter, r *http.Request) {
	if r.Method != "PUT" {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	pathParts := strings.Split(r.URL.Path, "/")
	if len(pathParts) < 4 {
		http.Error(w, "Invalid playlist ID", http.StatusBadRequest)
		return
	}

	playlistID, err := strconv.Atoi(pathParts[3])
	if err != nil {
		http.Error(w, "Invalid playlist ID", http.StatusBadRequest)
		return
	}

	// Parse multipart form data
	err = r.ParseMultipartForm(32 << 20) // 32 MB max memory
	if err != nil {
		http.Error(w, "Error parsing form data", http.StatusBadRequest)
		return
	}

	name := r.FormValue("name")
	description := r.FormValue("description")

	if name == "" {
		http.Error(w, "Playlist name is required", http.StatusBadRequest)
		return
	}

	var coverPath string

	// Handle file upload if present
	file, header, err := r.FormFile("cover")
	if err == nil {
		defer file.Close()

		// Create covers directory if it doesn't exist
		coversDir := filepath.Join("static", "covers")
		if err := os.MkdirAll(coversDir, 0755); err != nil {
			http.Error(w, "Error creating covers directory", http.StatusInternalServerError)
			return
		}

		// Generate unique filename
		ext := filepath.Ext(header.Filename)
		filename := fmt.Sprintf("playlist_%d_%d%s", playlistID, time.Now().Unix(), ext)
		coverPath = filepath.Join(coversDir, filename)

		// Save the file
		dst, err := os.Create(coverPath)
		if err != nil {
			http.Error(w, "Error saving cover image", http.StatusInternalServerError)
			return
		}
		defer dst.Close()

		_, err = io.Copy(dst, file)
		if err != nil {
			http.Error(w, "Error saving cover image", http.StatusInternalServerError)
			return
		}

		// Convert to relative path for storage in database
		coverPath = filepath.ToSlash(coverPath) // Convert to forward slashes for consistency
	} else if err != http.ErrMissingFile {
		http.Error(w, "Error processing cover image", http.StatusBadRequest)
		return
	}

	// Update playlist in database
	err = ms.db.UpdatePlaylist(playlistID, name, description, coverPath)
	if err != nil {
		http.Error(w, "Error updating playlist", http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	// CORS headers now applied globally via middleware.
	json.NewEncoder(w).Encode(map[string]string{"message": "Playlist updated successfully"})
}

// handleUpdatePlaylistSharing updates the sharing status of a playlist (POST).
func (ms *MusicServer) handleUpdatePlaylistSharing(w http.ResponseWriter, r *http.Request) {
	if r.Method != "POST" {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	pathParts := strings.Split(r.URL.Path, "/")
	if len(pathParts) < 4 {
		http.Error(w, "Invalid playlist ID", http.StatusBadRequest)
		return
	}

	playlistID, err := strconv.Atoi(pathParts[3])
	if err != nil {
		http.Error(w, "Invalid playlist ID", http.StatusBadRequest)
		return
	}

	var req struct {
		Shared bool `json:"shared"`
	}

	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "Invalid JSON", http.StatusBadRequest)
		return
	}

	// Check playlist ownership for sharing operations
	authService := ms.authService
	userFolderManager := authService.GetUserFolderManager()
	var currentUser string
	if authService.IsEnabled() && userFolderManager.IsEnabled() {
		if user := r.Context().Value(UserContextKey); user != nil {
			currentUser = user.(string)
		}

		// Only allow sharing changes if user owns the playlist
		isOwner, err := ms.db.CheckPlaylistOwnership(playlistID, currentUser)
		if err != nil {
			http.Error(w, "Error checking playlist ownership", http.StatusInternalServerError)
			return
		}

		if !isOwner {
			http.Error(w, "Access denied - you can only change sharing for your own playlists", http.StatusForbidden)
			return
		}
	}

	err = ms.db.UpdatePlaylistSharing(playlistID, req.Shared)
	if err != nil {
		http.Error(w, "Error updating playlist sharing", http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	action := "private"
	if req.Shared {
		action = "shared"
	}
	json.NewEncoder(w).Encode(map[string]string{"message": fmt.Sprintf("Playlist is now %s", action)})
}

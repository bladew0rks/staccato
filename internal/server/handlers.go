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

	"staccato/pkg/models"

	"github.com/sirupsen/logrus"
)

// respondJSON writes a JSON response
func (ms *MusicServer) respondJSON(w http.ResponseWriter, data interface{}) {
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(data); err != nil {
		ms.logger.WithError(err).Error("Failed to encode JSON response")
		http.Error(w, "Internal server error", http.StatusInternalServerError)
	}
}

// handleHome serves the main SPA / index file from the configured static dir.
func (ms *MusicServer) handleHome(w http.ResponseWriter, r *http.Request) {
	http.ServeFile(w, r, filepath.Join(ms.config.Server.StaticDir, "index.html"))
}

// handleGetTracks returns tracks optionally filtered (search) or sorted.
func (ms *MusicServer) handleGetTracks(w http.ResponseWriter, r *http.Request) {
	// Validate search query if provided
	searchQuery := r.URL.Query().Get("search")
	if searchQuery != "" {
		searchQuery = sanitizeInput(searchQuery)
		if validationErr := ms.validateSearchQuery(searchQuery); validationErr != nil {
			ms.respondWithValidationError(w, r, []ValidationError{*validationErr})
			return
		}
	}

	sortBy := r.URL.Query().Get("sort")
	var tracks []models.Track
	var err error

	// Check if auth and user folders are both enabled and get current user
	authService := ms.authService
	userFolderManager := authService.GetUserFolderManager()
	var currentUser string
	if authService.IsEnabled() && userFolderManager.IsEnabled() {
		if user := r.Context().Value(UserContextKey); user != nil {
			currentUser = user.(string)
		}
	}

	// Determine which data source to use:
	// - If auth is enabled AND user folders are enabled AND we have a current user: show user's tracks
	// - Otherwise: show only main library tracks (excludes user tracks)
	showUserTracks := authService.IsEnabled() && userFolderManager.IsEnabled() && currentUser != ""

	if searchQuery != "" {
		if showUserTracks {
			tracks, err = ms.db.SearchTracksForOwner(searchQuery, currentUser)
		} else {
			tracks, err = ms.db.SearchMainLibraryTracks(searchQuery)
		}
	} else if sortBy == "album" {
		if showUserTracks {
			tracks, err = ms.db.GetTracksSortedByAlbumForOwner(currentUser)
		} else {
			tracks, err = ms.db.GetMainLibraryTracksSortedByAlbum()
		}
	} else {
		if showUserTracks {
			tracks, err = ms.db.GetTracksByOwner(currentUser)
		} else {
			tracks, err = ms.db.GetMainLibraryTracks()
		}
	}

	if err != nil {
		ms.respondWithError(w, r, http.StatusInternalServerError, "Error retrieving tracks", err)
		return
	}

	ms.respondJSON(w, tracks)
}

// handleGetTrackCount responds with a JSON count of all tracks.
func (ms *MusicServer) handleGetTrackCount(w http.ResponseWriter, r *http.Request) {
	var tracks []models.Track
	var err error

	// Check if auth and user folders are both enabled and get current user
	authService := ms.authService
	userFolderManager := authService.GetUserFolderManager()
	var currentUser string
	if authService.IsEnabled() && userFolderManager.IsEnabled() {
		if user := r.Context().Value(UserContextKey); user != nil {
			currentUser = user.(string)
		}
	}

	// Determine which data source to use:
	// - If auth is enabled AND user folders are enabled AND we have a current user: show user's tracks
	// - Otherwise: show only main library tracks (excludes user tracks)
	showUserTracks := authService.IsEnabled() && userFolderManager.IsEnabled() && currentUser != ""

	if showUserTracks {
		tracks, err = ms.db.GetTracksByOwner(currentUser)
	} else {
		tracks, err = ms.db.GetMainLibraryTracks()
	}

	if err != nil {
		ms.respondWithError(w, r, http.StatusInternalServerError, "Error retrieving track count", err)
		return
	}

	response := map[string]int{"count": len(tracks)}
	ms.respondJSON(w, response)
}

// handleStreamTrack streams an individual track by ID with Range support.
func (ms *MusicServer) handleStreamTrack(w http.ResponseWriter, r *http.Request) {
	// Extract and validate track ID from URL path
	pathParts := strings.Split(r.URL.Path, "/")
	trackID, validationErr := ms.validateTrackID(pathParts, 3)
	if validationErr != nil {
		ms.respondWithValidationError(w, r, []ValidationError{*validationErr})
		return
	}

	// Check if auth and user folders are both enabled and get current user
	authService := ms.authService
	userFolderManager := authService.GetUserFolderManager()
	var currentUser string
	if authService.IsEnabled() && userFolderManager.IsEnabled() {
		if user := r.Context().Value(UserContextKey); user != nil {
			currentUser = user.(string)
		}
	}

	// Determine which data source to use:
	// - If auth is enabled AND user folders are enabled AND we have a current user: show user's tracks
	// - Otherwise: show only main library tracks (excludes user tracks)
	showUserTracks := authService.IsEnabled() && userFolderManager.IsEnabled() && currentUser != ""

	// Get track from database (with ownership check if user folders enabled)
	var track *models.Track
	var err error
	if showUserTracks {
		// First try to get track as owner
		track, err = ms.db.GetTrackByIDForOwner(trackID, currentUser)

		// If not found as owner, check if track is accessible via shared playlists
		if err != nil {
			// Check if track exists at all first
			trackExists, checkErr := ms.db.GetTrackByID(trackID)
			if checkErr == nil && trackExists != nil {
				// Track exists, now check if it's accessible via shared playlists
				canAccess, accessErr := ms.db.CheckTrackAccessViaSharedPlaylists(trackID, currentUser)
				if accessErr == nil && canAccess {
					track = trackExists
					err = nil // Clear the error since we found the track via shared playlist
				}
			}
		}
	} else {
		track, err = ms.db.GetMainLibraryTrackByID(trackID)
	}

	if err != nil {
		ms.respondWithError(w, r, http.StatusNotFound, "Track not found", err)
		return
	}

	// Validate file path security
	if validationErr := ms.validateFilePath(track.FilePath); validationErr != nil {
		ms.respondWithValidationError(w, r, []ValidationError{*validationErr})
		return
	}

	// Validate content type
	if validationErr := ms.validateContentType(track.FilePath); validationErr != nil {
		ms.respondWithValidationError(w, r, []ValidationError{*validationErr})
		return
	}

	// Open the audio file
	file, err := os.Open(track.FilePath)
	if err != nil {
		ms.logger.WithError(err).WithField("file_path", track.FilePath).Error("Error opening audio file")
		ms.respondWithError(w, r, http.StatusInternalServerError, "Error opening audio file", err)
		return
	}
	defer file.Close()

	// Get file info for content length
	stat, err := file.Stat()
	if err != nil {
		ms.respondWithError(w, r, http.StatusInternalServerError, "Error reading file info", err)
		return
	}

	// Set appropriate headers for audio streaming
	w.Header().Set("Content-Type", ms.extractor.GetContentType(track.FilePath))
	w.Header().Set("Content-Length", fmt.Sprintf("%d", stat.Size()))
	w.Header().Set("Accept-Ranges", "bytes")
	// CORS header applied by middleware if enabled

	// Handle range requests for seeking support
	rangeHeader := r.Header.Get("Range")
	if rangeHeader != "" {
		ms.handleRangeRequest(w, r, file, stat.Size(), rangeHeader)
		return
	}

	// Stream the entire file
	ms.logger.WithFields(logrus.Fields{
		"track_id": trackID,
		"artist":   track.Artist,
		"title":    track.Title,
	}).Info("Streaming track")

	_, err = io.Copy(w, file)
	if err != nil {
		ms.logger.WithError(err).WithField("track_id", trackID).Error("Error streaming file")
	}
}

// handleRangeRequest implements simple single-range byte serving for seeking.
func (ms *MusicServer) handleRangeRequest(w http.ResponseWriter, _ *http.Request, file *os.File, fileSize int64, rangeHeader string) {
	// Parse range header (e.g., "bytes=0-1023")
	ranges := strings.TrimPrefix(rangeHeader, "bytes=")
	rangeParts := strings.Split(ranges, "-")

	start, err := strconv.ParseInt(rangeParts[0], 10, 64)
	if err != nil {
		start = 0
	}

	var end int64
	if len(rangeParts) > 1 && rangeParts[1] != "" {
		end, err = strconv.ParseInt(rangeParts[1], 10, 64)
		if err != nil {
			end = fileSize - 1
		}
	} else {
		end = fileSize - 1
	}

	// Validate range
	if start < 0 || end >= fileSize || start > end {
		w.Header().Set("Content-Range", fmt.Sprintf("bytes */%d", fileSize))
		http.Error(w, "Range Not Satisfiable", http.StatusRequestedRangeNotSatisfiable)
		return
	}

	// Set partial content headers
	contentLength := end - start + 1
	w.Header().Set("Content-Range", fmt.Sprintf("bytes %d-%d/%d", start, end, fileSize))
	w.Header().Set("Content-Length", fmt.Sprintf("%d", contentLength))
	w.WriteHeader(http.StatusPartialContent)

	// Seek to start position and copy the requested range
	file.Seek(start, io.SeekStart)
	io.CopyN(w, file, contentLength)
}

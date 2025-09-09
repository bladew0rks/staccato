package server

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"staccato/internal/auth"
	"staccato/internal/config"
	"staccato/internal/database"
	"staccato/internal/downloader"
	"staccato/internal/metadata"
	"staccato/internal/ngrok"

	"github.com/fsnotify/fsnotify"
	"github.com/sirupsen/logrus"
)

// Context keys for request context values
type contextKey string

const (
	UserContextKey contextKey = "user"
)

// MusicServer encapsulates application state and HTTP handling for the music
// service including DB access, metadata extraction, optional downloader,
// optional ngrok tunneling, and filesystem watching.
type MusicServer struct {
	db           *database.Database
	config       *config.Config
	watcher      *fsnotify.Watcher
	extractor    *metadata.Extractor
	downloader   *downloader.Downloader
	ngrokService *ngrok.Service
	authService  *auth.Service
	server       *http.Server
	handler      http.Handler // root HTTP handler (router + middleware chain)
	shutdownCh   chan struct{}
	logger       *logrus.Logger
}

// NewMusicServer constructs a MusicServer with optional components (downloader,
// ngrok). Missing optional components degrade functionality gracefully.
func NewMusicServer(cfg *config.Config, db *database.Database) (*MusicServer, error) {
	// Initialize structured logger
	logger := logrus.New()

	// Configure logger based on config
	level, err := logrus.ParseLevel(cfg.Logging.Level)
	if err != nil {
		level = logrus.InfoLevel
	}
	logger.SetLevel(level)

	if cfg.Logging.Format == "json" {
		logger.SetFormatter(&logrus.JSONFormatter{})
	} else {
		logger.SetFormatter(&logrus.TextFormatter{
			FullTimestamp: true,
		})
	}

	// Set output file if specified
	if cfg.Logging.File != "" {
		file, err := os.OpenFile(cfg.Logging.File, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0666)
		if err != nil {
			logger.Warnf("Failed to open log file %s, using stdout: %v", cfg.Logging.File, err)
		} else {
			logger.SetOutput(file)
		}
	}

	// Create downloader
	dl, err := downloader.NewDownloader(cfg)
	if err != nil {
		logger.WithError(err).Warn("Downloader not available")
		dl = nil // Downloader will be nil if not available
	}

	// Create ngrok service
	ngrokSvc, err := ngrok.NewService(&cfg.Ngrok)
	if err != nil {
		logger.WithError(err).Warn("Ngrok service not available")
		ngrokSvc = nil
	}

	// Create auth service
	authSvc, err := auth.NewService(&cfg.Auth)
	if err != nil {
		return nil, fmt.Errorf("failed to create auth service: %w", err)
	}

	server := &MusicServer{
		db:           db,
		config:       cfg,
		extractor:    metadata.NewExtractor(cfg.Music.SupportedFormats),
		downloader:   dl,
		ngrokService: ngrokSvc,
		authService:  authSvc,
		shutdownCh:   make(chan struct{}),
		logger:       logger,
	}

	// Set up user data cleanup callback
	authSvc.SetCleanupCallback(func(username string) error {
		return db.DeleteTracksByOwner(username)
	})

	// Attach ingestion capabilities if downloader available
	if server.downloader != nil {
		server.downloader.AttachIngest(server.db, server.extractor)
	}

	return server, nil
}

// ScanMusicLibrary walks the configured music directory ingesting supported
// audio files into the database. Concurrency is sized to runtime.NumCPU.
func (ms *MusicServer) ScanMusicLibrary() error {
	if !ms.config.Music.ScanOnStartup {
		ms.logger.Info("Skipping library scan (disabled in config)")
		return nil
	}

	// Determine which path to scan based on user_folders setting
	scanPath := ms.config.Music.LibraryPath
	if ms.authService.GetUserFolderManager().IsEnabled() {
		scanPath = ms.config.Auth.UserMusicPath
		ms.logger.WithFields(logrus.Fields{
			"library_path":    ms.config.Music.LibraryPath,
			"user_music_path": scanPath,
		}).Info("User folders enabled - scanning user music directory")
	} else {
		ms.logger.WithField("library_path", scanPath).Info("Scanning music library")
	}

	var wg sync.WaitGroup
	var trackCount int64
	jobs := make(chan string, 100)

	// Start worker pool
	numWorkers := runtime.NumCPU()
	for i := 0; i < numWorkers; i++ {
		go func() {
			for path := range jobs {
				track, err := ms.extractor.ExtractFromFile(path, 0)
				if err != nil {
					ms.logger.WithError(err).WithField("file_path", path).Error("Error extracting metadata")
					wg.Done()
					continue
				}

				// Determine ownership if user folders are enabled
				if ms.authService.GetUserFolderManager().IsEnabled() {
					owner := ms.authService.GetUserFolderManager().GetOwnerFromPath(path)
					track.Owner = owner
				}

				_, err = ms.db.InsertTrack(track)
				if err != nil {
					ms.logger.WithError(err).Error("Error inserting track into database")
				} else {
					atomic.AddInt64(&trackCount, 1)
					ms.logger.WithFields(logrus.Fields{
						"artist": track.Artist,
						"title":  track.Title,
						"album":  track.Album,
						"owner":  track.Owner,
					}).Debug("Added track")
				}
				wg.Done()
			}
		}()
	}

	// Walk directory and enqueue jobs
	walkErr := filepath.Walk(scanPath, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if ms.extractor.IsAudioFile(path) {
			wg.Add(1)
			jobs <- path
		}
		return nil
	})

	// Close jobs channel and wait for all workers
	close(jobs)
	wg.Wait()

	ms.logger.WithField("track_count", trackCount).Info("Library scan completed")
	return walkErr
}

// Start begins serving HTTP requests and (optionally) establishes an ngrok
// tunnel. It blocks until a shutdown signal is received or a fatal error.
func (ms *MusicServer) Start() {
	// Start file watcher if enabled
	if ms.config.Music.WatchForChanges {
		if err := ms.startFileWatcher(); err != nil {
			ms.logger.WithError(err).Warn("Could not start file watcher")
		}
	}

	// Start user file watcher if auth is enabled
	if ms.authService.IsEnabled() {
		if err := ms.authService.StartUserWatcher(); err != nil {
			ms.logger.WithError(err).Warn("Could not start user file watcher")
		}
	}

	// Set up routes (build handler chain)
	ms.handler = ms.setupRoutes()

	// Get track count from database
	tracks, err := ms.db.GetAllTracks()
	trackCount := 0
	if err == nil {
		trackCount = len(tracks)
	}

	localAddress := fmt.Sprintf("http://%s", ms.config.GetAddress())

	ms.logger.WithFields(logrus.Fields{
		"port":        ms.config.Server.Port,
		"track_count": trackCount,
	}).Info("Staccato server starting")

	if ms.config.Music.WatchForChanges {
		watchPath := ms.config.Music.LibraryPath
		if ms.authService.GetUserFolderManager().IsEnabled() {
			watchPath = ms.config.Auth.UserMusicPath
		}
		ms.logger.WithField("watch_path", watchPath).Info("File watcher monitoring library")
	}
	ms.logger.WithField("local_address", localAddress).Info("Local access available")

	// Start ngrok tunnel if enabled
	if ms.ngrokService != nil {
		ctx := context.Background()
		if err := ms.ngrokService.StartTunnel(ctx, localAddress); err != nil {
			ms.logger.WithError(err).Warn("Could not start ngrok tunnel")
		}
	}

	// Create server with proper timeouts
	ms.server = &http.Server{
		Addr:         ms.config.GetAddress(),
		ReadTimeout:  time.Duration(ms.config.Server.ReadTimeout) * time.Second,
		WriteTimeout: time.Duration(ms.config.Server.WriteTimeout) * time.Second,
		IdleTimeout:  time.Duration(ms.config.Server.IdleTimeout) * time.Second,
		Handler:      ms.handler,
	}

	// Start server in a goroutine
	serverErrCh := make(chan error, 1)
	go func() {
		ms.logger.WithField("address", ms.server.Addr).Info("HTTP server listening")
		if err := ms.server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			serverErrCh <- fmt.Errorf("server failed to start: %w", err)
		}
	}()

	// Wait for shutdown signal or server error
	select {
	case err := <-serverErrCh:
		ms.logger.WithError(err).Fatal("Server error")
	case <-ms.shutdownCh:
		ms.logger.Info("Shutdown signal received, starting graceful shutdown")
	}
}

func (ms *MusicServer) setupRoutes() http.Handler {
	// Create a new ServeMux
	mux := http.NewServeMux()

	// Set up routes
	mux.HandleFunc("/login", ms.handleLogin)
	mux.HandleFunc("/api/auth/login", ms.handleAuthLogin)
	mux.HandleFunc("/api/auth/logout", ms.handleAuthLogout)
	mux.HandleFunc("/api/auth/register", ms.handleAuthRegister)
	mux.HandleFunc("/api/config", ms.handleGetConfig)
	mux.HandleFunc("/", ms.handleHome)
	mux.Handle("/static/", http.StripPrefix("/static/", http.FileServer(http.Dir(ms.config.Server.StaticDir))))
	mux.HandleFunc("/api/tracks", ms.handleGetTracks)
	mux.HandleFunc("/api/tracks/count", ms.handleGetTrackCount)
	mux.HandleFunc("/api/tracks/upload", ms.handleUploadTrack)
	mux.HandleFunc("/stream/", ms.handleStreamTrack)
	mux.HandleFunc("/albumart/", ms.handleAlbumArt) // Album art endpoint
	mux.HandleFunc("/health", ms.handleHealthCheck) // Health check endpoint

	// Download routes
	mux.HandleFunc("/api/download", ms.handleDownloadMusic)
	mux.HandleFunc("/api/downloads", ms.handleGetDownloads)
	mux.HandleFunc("/api/downloads/", ms.handleGetDownloads) // For specific job ID
	mux.HandleFunc("/api/validate-url", ms.handleValidateURL)
	mux.HandleFunc("/api/downloads/cleanup", ms.handleCleanupDownloads)

	// Playlist routes
	mux.HandleFunc("/api/playlists", ms.handleGetPlaylists)
	mux.HandleFunc("/api/playlists/create", ms.handleCreatePlaylist)
	mux.HandleFunc("/api/playlists/", func(w http.ResponseWriter, r *http.Request) {
		pathParts := strings.Split(r.URL.Path, "/")
		if len(pathParts) >= 5 && pathParts[4] == "tracks" {
			switch r.Method {
			case "GET":
				ms.handleGetPlaylistTracks(w, r)
			case "POST":
				ms.handleAddTrackToPlaylist(w, r)
			case "DELETE":
				ms.handleRemoveTrackFromPlaylist(w, r)
			}
		} else if len(pathParts) >= 5 && pathParts[4] == "share" {
			switch r.Method {
			case "POST":
				ms.handleUpdatePlaylistSharing(w, r)
			}
		} else {
			switch r.Method {
			case "DELETE":
				ms.handleDeletePlaylist(w, r)
			case "PUT":
				ms.handleUpdatePlaylist(w, r)
			}
		}
	})

	// Apply middleware chain (order: auth -> panic recovery -> logging)
	handler := ms.authMiddleware(mux)
	handler = ms.panicRecoveryMiddleware(handler)
	handler = ms.corsMiddleware(handler)
	handler = ms.requestLoggingMiddleware(handler)
	return handler
}

// Shutdown gracefully stops all server components (HTTP listener, watcher,
// ngrok tunnel, database connection).
func (ms *MusicServer) Shutdown() {
	ms.logger.Info("Shutting down music server")

	// Signal the start method to begin shutdown (safely)
	select {
	case <-ms.shutdownCh:
		// Already signaled
	default:
		close(ms.shutdownCh)
	}

	// Create a context with timeout for graceful shutdown
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	// Stop the HTTP server gracefully
	if ms.server != nil {
		ms.logger.Info("Shutting down HTTP server")
		if err := ms.server.Shutdown(ctx); err != nil {
			ms.logger.WithError(err).Error("Error during HTTP server shutdown")
			// Force close if graceful shutdown fails
			if err := ms.server.Close(); err != nil {
				ms.logger.WithError(err).Error("Error force closing HTTP server")
			}
		} else {
			ms.logger.Info("HTTP server shut down gracefully")
		}
	}

	// Stop file watcher
	ms.logger.Info("Stopping file watcher")
	ms.stopFileWatcher()

	// Stop user file watcher
	if ms.authService != nil {
		ms.logger.Info("Stopping user file watcher")
		ms.authService.StopUserWatcher()
	}

	// Stop ngrok service
	if ms.ngrokService != nil {
		ms.logger.Info("Stopping ngrok service")
		ms.ngrokService.Stop()
	}

	// Close database connection
	if ms.db != nil {
		ms.logger.Info("Closing database connection")
		ms.db.Close()
	}

	ms.logger.Info("Music server shutdown complete")
}

// RequestShutdown can be called from other goroutines to initiate graceful
// shutdown (idempotent).
func (ms *MusicServer) RequestShutdown() {
	select {
	case <-ms.shutdownCh:
		// Already shutting down
		return
	default:
		close(ms.shutdownCh)
	}
}

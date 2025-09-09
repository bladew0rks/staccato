package database

import (
	"database/sql"
	"fmt"
	"time"

	"staccato/pkg/models"

	_ "github.com/mattn/go-sqlite3"
	"github.com/sirupsen/logrus"
)

// Database wraps a *sql.DB providing higher-level helper methods for
// interacting with the application's persistent store. It is safe for
// concurrent use because the underlying *sql.DB is concurrency-safe.
type Database struct {
	conn   *sql.DB
	logger *logrus.Logger

	// Track if owner column exists (for handling migrations)
	hasOwnerColumn bool

	// Prepared statements for better performance
	insertTrackStmt  *sql.Stmt
	updateTrackStmt  *sql.Stmt
	getTrackByIDStmt *sql.Stmt
	trackExistsStmt  *sql.Stmt
	removeTrackStmt  *sql.Stmt
	searchTracksStmt *sql.Stmt
}

// NewDatabase opens (or creates) a SQLite database at the provided path and
// ensures all required tables and indices exist. It also applies lightweight
// performance-oriented pragmas (WAL, cache sizing). Caller should Close() it
// when finished.
func NewDatabase(dbPath string) (*Database, error) {
	// Initialize logger
	logger := logrus.New()
	logger.SetFormatter(&logrus.JSONFormatter{})

	conn, err := sql.Open("sqlite3", dbPath+"?cache=shared&mode=rwc")
	if err != nil {
		return nil, fmt.Errorf("failed to open database: %w", err)
	}

	// Configure connection pool - adjusted for SQLite
	conn.SetMaxOpenConns(5) // SQLite works better with fewer connections
	conn.SetMaxIdleConns(2)
	conn.SetConnMaxLifetime(15 * time.Minute)

	// Enable WAL mode for better concurrency
	pragmas := []string{
		"PRAGMA journal_mode=WAL;",
		"PRAGMA synchronous=NORMAL;",
		"PRAGMA cache_size=2000;", // Increased cache size
		"PRAGMA temp_store=memory;",
		"PRAGMA foreign_keys=ON;",         // Enable foreign key constraints
		"PRAGMA auto_vacuum=INCREMENTAL;", // Better space management
	}

	for _, pragma := range pragmas {
		if _, err := conn.Exec(pragma); err != nil {
			logger.WithError(err).WithField("pragma", pragma).Warn("Failed to set pragma")
		}
	}

	db := &Database{
		conn:   conn,
		logger: logger,
	}

	if err := db.createTables(); err != nil {
		conn.Close()
		return nil, fmt.Errorf("failed to create tables: %w", err)
	}

	if err := db.prepareStatements(); err != nil {
		conn.Close()
		return nil, fmt.Errorf("failed to prepare statements: %w", err)
	}

	logger.WithField("db_path", dbPath).Info("Database initialized successfully")
	return db, nil
}

// createTables creates tables and indices if they do not already exist, then
// executes any migrations. This is idempotent and safe to call multiple times.
func (db *Database) createTables() error {
	// Create tracks table
	tracksTable := `
	CREATE TABLE IF NOT EXISTS tracks (
		id INTEGER PRIMARY KEY AUTOINCREMENT,
		title TEXT NOT NULL,
		artist TEXT NOT NULL,
		album TEXT NOT NULL,
		track_number INTEGER DEFAULT 0,
		duration INTEGER DEFAULT 0,
		file_path TEXT NOT NULL UNIQUE,
		file_size INTEGER NOT NULL,
		has_album_art BOOLEAN DEFAULT FALSE,
		album_art_id TEXT,
		created_at DATETIME DEFAULT CURRENT_TIMESTAMP
	);`

	// Create playlists table
	playlistsTable := `
	CREATE TABLE IF NOT EXISTS playlists (
		id INTEGER PRIMARY KEY AUTOINCREMENT,
		name TEXT NOT NULL,
		description TEXT,
		cover_path TEXT,
		created_at DATETIME DEFAULT CURRENT_TIMESTAMP
	);`

	// Create playlist_tracks junction table
	playlistTracksTable := `
	CREATE TABLE IF NOT EXISTS playlist_tracks (
		playlist_id INTEGER,
		track_id INTEGER,
		position INTEGER,
		added_at DATETIME DEFAULT CURRENT_TIMESTAMP,
		FOREIGN KEY (playlist_id) REFERENCES playlists(id) ON DELETE CASCADE,
		FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE,
		PRIMARY KEY (playlist_id, track_id)
	);`

	// Create download_jobs table (for persistence of downloads)
	downloadJobsTable := `
	CREATE TABLE IF NOT EXISTS download_jobs (
		id TEXT PRIMARY KEY,
		url TEXT NOT NULL,
		title TEXT,
		artist TEXT,
		status TEXT,
		progress INTEGER,
		error TEXT,
		output_path TEXT,
		speed TEXT,
		eta_seconds INTEGER,
		created_at DATETIME,
		completed_at DATETIME
	);`

	// Create indices for better performance
	indices := []string{
		"CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);",
		"CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album);",
		"CREATE INDEX IF NOT EXISTS idx_tracks_artist_album ON tracks(artist, album, track_number);", // Composite index
		"CREATE INDEX IF NOT EXISTS idx_tracks_search ON tracks(title, artist, album);",              // Search optimization
		"CREATE INDEX IF NOT EXISTS idx_tracks_file_path ON tracks(file_path);",                      // Unique lookups
		// "CREATE INDEX IF NOT EXISTS idx_tracks_owner ON tracks(owner);",                              // User filtering - created after migration
		"CREATE INDEX IF NOT EXISTS idx_playlist_tracks_playlist ON playlist_tracks(playlist_id);",
		"CREATE INDEX IF NOT EXISTS idx_playlist_tracks_position ON playlist_tracks(playlist_id, position);",
		"CREATE INDEX IF NOT EXISTS idx_download_jobs_status ON download_jobs(status);",      // Status queries
		"CREATE INDEX IF NOT EXISTS idx_download_jobs_created ON download_jobs(created_at);", // Time-based queries
	}

	tables := []string{tracksTable, playlistsTable, playlistTracksTable, downloadJobsTable}
	for _, table := range tables {
		if _, err := db.conn.Exec(table); err != nil {
			return err
		}
	}

	for _, index := range indices {
		if _, err := db.conn.Exec(index); err != nil {
			return err
		}
	}

	// Run migrations
	if err := db.runMigrations(); err != nil {
		return err
	}

	return nil
}

// runMigrations performs incremental schema updates in-place. Each migration
// should be idempotent and safe to re-run; keep them lightweight.
func (db *Database) runMigrations() error {
	// Migration 1: Add cover_path column to playlists table if it doesn't exist
	var columnExists bool
	err := db.conn.QueryRow(`
		SELECT COUNT(*) > 0 
		FROM pragma_table_info('playlists') 
		WHERE name = 'cover_path'`).Scan(&columnExists)

	if err != nil {
		return err
	}

	if !columnExists {
		_, err = db.conn.Exec("ALTER TABLE playlists ADD COLUMN cover_path TEXT")
		if err != nil {
			return err
		}
	}

	// Migration 2: Add owner column to tracks table if it doesn't exist
	var ownerColumnExists bool
	err = db.conn.QueryRow(`
		SELECT COUNT(*) > 0 
		FROM pragma_table_info('tracks') 
		WHERE name = 'owner'`).Scan(&ownerColumnExists)

	if err != nil {
		return err
	}

	if !ownerColumnExists {
		_, err = db.conn.Exec("ALTER TABLE tracks ADD COLUMN owner TEXT")
		if err != nil {
			return err
		}

		// Create index for the new owner column
		_, err = db.conn.Exec("CREATE INDEX IF NOT EXISTS idx_tracks_owner ON tracks(owner)")
		if err != nil {
			return err
		}

		db.logger.Info("Added owner column and index to tracks table")
	}

	// Migration 3: Add owner and shared columns to playlists table if they don't exist
	var playlistOwnerExists bool
	err = db.conn.QueryRow(`
		SELECT COUNT(*) > 0 
		FROM pragma_table_info('playlists') 
		WHERE name = 'owner'`).Scan(&playlistOwnerExists)

	if err != nil {
		return err
	}

	if !playlistOwnerExists {
		_, err = db.conn.Exec("ALTER TABLE playlists ADD COLUMN owner TEXT")
		if err != nil {
			return err
		}

		// Create index for the new owner column
		_, err = db.conn.Exec("CREATE INDEX IF NOT EXISTS idx_playlists_owner ON playlists(owner)")
		if err != nil {
			return err
		}

		db.logger.Info("Added owner column and index to playlists table")
	}

	var playlistSharedExists bool
	err = db.conn.QueryRow(`
		SELECT COUNT(*) > 0 
		FROM pragma_table_info('playlists') 
		WHERE name = 'shared'`).Scan(&playlistSharedExists)

	if err != nil {
		return err
	}

	if !playlistSharedExists {
		_, err = db.conn.Exec("ALTER TABLE playlists ADD COLUMN shared BOOLEAN DEFAULT FALSE")
		if err != nil {
			return err
		}

		// Create index for the new shared column
		_, err = db.conn.Exec("CREATE INDEX IF NOT EXISTS idx_playlists_shared ON playlists(shared)")
		if err != nil {
			return err
		}

		db.logger.Info("Added shared column and index to playlists table")
	}

	return nil
}

// prepareStatements prepares commonly used SQL statements for better performance
func (db *Database) prepareStatements() error {
	var err error

	// Check if owner column exists before preparing statements with it
	err = db.conn.QueryRow(`
		SELECT COUNT(*) > 0 
		FROM pragma_table_info('tracks') 
		WHERE name = 'owner'`).Scan(&db.hasOwnerColumn)
	if err != nil {
		return fmt.Errorf("failed to check for owner column: %w", err)
	}

	// Insert track statement
	if db.hasOwnerColumn {
		db.insertTrackStmt, err = db.conn.Prepare(`
			INSERT INTO tracks (title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, owner)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`)
	} else {
		db.insertTrackStmt, err = db.conn.Prepare(`
			INSERT INTO tracks (title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`)
	}
	if err != nil {
		return fmt.Errorf("failed to prepare insert track statement: %w", err)
	}

	// Update track statement
	if db.hasOwnerColumn {
		db.updateTrackStmt, err = db.conn.Prepare(`
			UPDATE tracks SET title = ?, artist = ?, album = ?, track_number = ?, duration = ?, file_size = ?, has_album_art = ?, album_art_id = ?, owner = ?
			WHERE id = ?`)
	} else {
		db.updateTrackStmt, err = db.conn.Prepare(`
			UPDATE tracks SET title = ?, artist = ?, album = ?, track_number = ?, duration = ?, file_size = ?, has_album_art = ?, album_art_id = ?
			WHERE id = ?`)
	}
	if err != nil {
		return fmt.Errorf("failed to prepare update track statement: %w", err)
	}

	// Get track by ID statement
	if db.hasOwnerColumn {
		db.getTrackByIDStmt, err = db.conn.Prepare(`
			SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, owner
			FROM tracks WHERE id = ?`)
	} else {
		db.getTrackByIDStmt, err = db.conn.Prepare(`
			SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id
			FROM tracks WHERE id = ?`)
	}
	if err != nil {
		return fmt.Errorf("failed to prepare get track by ID statement: %w", err)
	}

	// Track exists statement
	db.trackExistsStmt, err = db.conn.Prepare(`
		SELECT COUNT(*) FROM tracks WHERE file_path = ?`)
	if err != nil {
		return fmt.Errorf("failed to prepare track exists statement: %w", err)
	}

	// Remove track statement
	db.removeTrackStmt, err = db.conn.Prepare(`
		DELETE FROM tracks WHERE file_path = ?`)
	if err != nil {
		return fmt.Errorf("failed to prepare remove track statement: %w", err)
	}

	// Search tracks statement
	if db.hasOwnerColumn {
		db.searchTracksStmt, err = db.conn.Prepare(`
			SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, owner
			FROM tracks
			WHERE title LIKE ? OR artist LIKE ? OR album LIKE ?
			ORDER BY artist, album, track_number, title`)
	} else {
		db.searchTracksStmt, err = db.conn.Prepare(`
			SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id
			FROM tracks
			WHERE title LIKE ? OR artist LIKE ? OR album LIKE ?
			ORDER BY artist, album, track_number, title`)
	}
	if err != nil {
		return fmt.Errorf("failed to prepare search tracks statement: %w", err)
	}

	return nil
}

// InsertTrack inserts a new track or updates an existing track (matched by
// file_path) returning the track's database ID.
func (db *Database) InsertTrack(track models.Track) (int, error) {
	// Check if track already exists using prepared statement
	var existingID int
	err := db.conn.QueryRow("SELECT id FROM tracks WHERE file_path = ?", track.FilePath).Scan(&existingID)
	if err == nil {
		// Track exists, update it using prepared statement
		if db.hasOwnerColumn {
			_, err = db.updateTrackStmt.Exec(
				track.Title, track.Artist, track.Album, track.TrackNumber,
				track.Duration, track.FileSize, track.HasAlbumArt, track.AlbumArtID, track.Owner,
				existingID)
		} else {
			_, err = db.updateTrackStmt.Exec(
				track.Title, track.Artist, track.Album, track.TrackNumber,
				track.Duration, track.FileSize, track.HasAlbumArt, track.AlbumArtID,
				existingID)
		}
		if err != nil {
			db.logger.WithError(err).WithField("track_id", existingID).Error("Failed to update existing track")
		}
		return existingID, err
	}

	// Insert new track using prepared statement
	var result sql.Result
	if db.hasOwnerColumn {
		result, err = db.insertTrackStmt.Exec(
			track.Title, track.Artist, track.Album, track.TrackNumber,
			track.Duration, track.FilePath, track.FileSize, track.HasAlbumArt, track.AlbumArtID, track.Owner)
	} else {
		result, err = db.insertTrackStmt.Exec(
			track.Title, track.Artist, track.Album, track.TrackNumber,
			track.Duration, track.FilePath, track.FileSize, track.HasAlbumArt, track.AlbumArtID)
	}

	if err != nil {
		db.logger.WithError(err).WithField("file_path", track.FilePath).Error("Failed to insert new track")
		return 0, err
	}

	id, err := result.LastInsertId()
	if err != nil {
		db.logger.WithError(err).Error("Failed to get last insert ID")
		return 0, err
	}

	return int(id), nil
}

// GetAllTracks returns all tracks ordered by artist/album/track/title.
func (db *Database) GetAllTracks() ([]models.Track, error) {
	var query string
	if db.hasOwnerColumn {
		query = `
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, COALESCE(owner, '') as owner
		FROM tracks
		ORDER BY artist, album, track_number, title`
	} else {
		query = `
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id
		FROM tracks
		ORDER BY artist, album, track_number, title`
	}

	rows, err := db.conn.Query(query)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanTrackRows(rows)
}

// GetMainLibraryTracks returns only tracks from the main library (with empty/null owner) ordered by artist/album/track/title.
func (db *Database) GetMainLibraryTracks() ([]models.Track, error) {
	var query string
	if db.hasOwnerColumn {
		query = `
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, COALESCE(owner, '') as owner
		FROM tracks
		WHERE owner IS NULL OR owner = ''
		ORDER BY artist, album, track_number, title`
	} else {
		query = `
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id
		FROM tracks
		ORDER BY artist, album, track_number, title`
	}

	rows, err := db.conn.Query(query)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanTrackRows(rows)
}

// GetTracksByOwner returns all tracks for a specific user ordered by artist/album/track/title.
func (db *Database) GetTracksByOwner(owner string) ([]models.Track, error) {
	if !db.hasOwnerColumn {
		// If no owner column, return empty result for user-specific queries
		return []models.Track{}, nil
	}

	rows, err := db.conn.Query(`
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, COALESCE(owner, '') as owner
		FROM tracks
		WHERE owner = ?
		ORDER BY artist, album, track_number, title`, owner)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanTrackRows(rows)
}

// GetTracksSortedByAlbum returns all tracks ordered by album/track/title.
func (db *Database) GetTracksSortedByAlbum() ([]models.Track, error) {
	var query string
	if db.hasOwnerColumn {
		query = `
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, COALESCE(owner, '') as owner
		FROM tracks
		ORDER BY album, track_number, title`
	} else {
		query = `
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id
		FROM tracks
		ORDER BY album, track_number, title`
	}

	rows, err := db.conn.Query(query)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanTrackRows(rows)
}

// GetMainLibraryTracksSortedByAlbum returns only tracks from the main library (with empty/null owner) ordered by album/track/title.
func (db *Database) GetMainLibraryTracksSortedByAlbum() ([]models.Track, error) {
	var query string
	if db.hasOwnerColumn {
		query = `
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, COALESCE(owner, '') as owner
		FROM tracks
		WHERE owner IS NULL OR owner = ''
		ORDER BY album, track_number, title`
	} else {
		query = `
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id
		FROM tracks
		ORDER BY album, track_number, title`
	}

	rows, err := db.conn.Query(query)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanTrackRows(rows)
}

// GetTracksSortedByAlbumForOwner returns tracks for a specific user ordered by album/track/title.
func (db *Database) GetTracksSortedByAlbumForOwner(owner string) ([]models.Track, error) {
	if !db.hasOwnerColumn {
		// If no owner column, return empty result for user-specific queries
		return []models.Track{}, nil
	}

	rows, err := db.conn.Query(`
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, COALESCE(owner, '') as owner
		FROM tracks
		WHERE owner = ?
		ORDER BY album, track_number, title`, owner)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanTrackRows(rows)
}

// GetTrackByID returns a single track by its ID.
func (db *Database) GetTrackByID(id int) (*models.Track, error) {
	var track models.Track
	var albumArtID sql.NullString

	if db.hasOwnerColumn {
		err := db.getTrackByIDStmt.QueryRow(id).Scan(
			&track.ID, &track.Title, &track.Artist, &track.Album,
			&track.TrackNumber, &track.Duration, &track.FilePath,
			&track.FileSize, &track.HasAlbumArt, &albumArtID, &track.Owner)

		if err != nil {
			if err == sql.ErrNoRows {
				return nil, fmt.Errorf("track with ID %d not found", id)
			}
			db.logger.WithError(err).WithField("track_id", id).Error("Failed to get track by ID")
			return nil, err
		}
	} else {
		err := db.getTrackByIDStmt.QueryRow(id).Scan(
			&track.ID, &track.Title, &track.Artist, &track.Album,
			&track.TrackNumber, &track.Duration, &track.FilePath,
			&track.FileSize, &track.HasAlbumArt, &albumArtID)

		if err != nil {
			if err == sql.ErrNoRows {
				return nil, fmt.Errorf("track with ID %d not found", id)
			}
			db.logger.WithError(err).WithField("track_id", id).Error("Failed to get track by ID")
			return nil, err
		}
		track.Owner = "" // Set empty owner for backward compatibility
	}

	if albumArtID.Valid {
		track.AlbumArtID = albumArtID.String
	}
	return &track, nil
}

// GetTrackByIDForOwner returns a track only if it belongs to the specified owner.
func (db *Database) GetTrackByIDForOwner(id int, owner string) (*models.Track, error) {
	if !db.hasOwnerColumn {
		// If no owner column, fall back to regular GetTrackByID
		return db.GetTrackByID(id)
	}

	var track models.Track
	var albumArtID sql.NullString

	err := db.conn.QueryRow(`
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, COALESCE(owner, '') as owner
		FROM tracks WHERE id = ? AND owner = ?`, id, owner).Scan(
		&track.ID, &track.Title, &track.Artist, &track.Album,
		&track.TrackNumber, &track.Duration, &track.FilePath,
		&track.FileSize, &track.HasAlbumArt, &albumArtID, &track.Owner)

	if err != nil {
		if err == sql.ErrNoRows {
			return nil, fmt.Errorf("track with ID %d not found for user %s", id, owner)
		}
		db.logger.WithError(err).WithField("track_id", id).WithField("owner", owner).Error("Failed to get track by ID for owner")
		return nil, err
	}

	if albumArtID.Valid {
		track.AlbumArtID = albumArtID.String
	}
	return &track, nil
}

// GetMainLibraryTrackByID returns a track only if it belongs to the main library (empty/null owner).
func (db *Database) GetMainLibraryTrackByID(id int) (*models.Track, error) {
	var track models.Track
	var albumArtID sql.NullString

	if db.hasOwnerColumn {
		err := db.conn.QueryRow(`
			SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, COALESCE(owner, '') as owner
			FROM tracks WHERE id = ? AND (owner IS NULL OR owner = '')`, id).Scan(
			&track.ID, &track.Title, &track.Artist, &track.Album,
			&track.TrackNumber, &track.Duration, &track.FilePath,
			&track.FileSize, &track.HasAlbumArt, &albumArtID, &track.Owner)

		if err != nil {
			if err == sql.ErrNoRows {
				return nil, fmt.Errorf("track with ID %d not found in main library", id)
			}
			db.logger.WithError(err).WithField("track_id", id).Error("Failed to get main library track by ID")
			return nil, err
		}
	} else {
		// If no owner column exists, all tracks are main library tracks
		return db.GetTrackByID(id)
	}

	if albumArtID.Valid {
		track.AlbumArtID = albumArtID.String
	}
	return &track, nil
}

// CreatePlaylist inserts a new playlist and returns its ID.
func (db *Database) CreatePlaylist(name, description, owner string, shared bool) (int, error) {
	result, err := db.conn.Exec(`
		INSERT INTO playlists (name, description, owner, shared)
		VALUES (?, ?, ?, ?)`, name, description, owner, shared)

	if err != nil {
		return 0, err
	}

	id, err := result.LastInsertId()
	return int(id), err
}

// GetAllPlaylists returns all playlists along with derived track counts.
func (db *Database) GetAllPlaylists() ([]models.Playlist, error) {
	rows, err := db.conn.Query(`
		SELECT p.id, p.name, p.description, p.cover_path, p.created_at,
			   COALESCE(COUNT(pt.track_id), 0) as track_count, COALESCE(p.owner, '') as owner, COALESCE(p.shared, FALSE) as shared
		FROM playlists p
		LEFT JOIN playlist_tracks pt ON p.id = pt.playlist_id
		GROUP BY p.id, p.name, p.description, p.cover_path, p.created_at, p.owner, p.shared
		ORDER BY p.created_at DESC`)

	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var playlists []models.Playlist
	for rows.Next() {
		var playlist models.Playlist
		var coverPath sql.NullString
		err := rows.Scan(&playlist.ID, &playlist.Name, &playlist.Description,
			&coverPath, &playlist.CreatedAt, &playlist.TrackCount, &playlist.Owner, &playlist.Shared)
		if err != nil {
			return nil, err
		}
		if coverPath.Valid {
			playlist.CoverPath = coverPath.String
		}
		playlists = append(playlists, playlist)
	}

	return playlists, nil
}

// GetPlaylistsForUser returns playlists accessible to a specific user (owned by them or shared).
func (db *Database) GetPlaylistsForUser(owner string) ([]models.Playlist, error) {
	rows, err := db.conn.Query(`
		SELECT p.id, p.name, p.description, p.cover_path, p.created_at,
			   COALESCE(COUNT(pt.track_id), 0) as track_count, COALESCE(p.owner, '') as owner, COALESCE(p.shared, FALSE) as shared
		FROM playlists p
		LEFT JOIN playlist_tracks pt ON p.id = pt.playlist_id
		WHERE p.owner = ? OR p.shared = TRUE
		GROUP BY p.id, p.name, p.description, p.cover_path, p.created_at, p.owner, p.shared
		ORDER BY p.created_at DESC`, owner)

	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var playlists []models.Playlist
	for rows.Next() {
		var playlist models.Playlist
		var coverPath sql.NullString
		err := rows.Scan(&playlist.ID, &playlist.Name, &playlist.Description,
			&coverPath, &playlist.CreatedAt, &playlist.TrackCount, &playlist.Owner, &playlist.Shared)
		if err != nil {
			return nil, err
		}
		if coverPath.Valid {
			playlist.CoverPath = coverPath.String
		}
		playlists = append(playlists, playlist)
	}

	return playlists, nil
}

// GetMainLibraryPlaylists returns only playlists from the main library (with empty/null owner) or shared playlists.
func (db *Database) GetMainLibraryPlaylists() ([]models.Playlist, error) {
	rows, err := db.conn.Query(`
		SELECT p.id, p.name, p.description, p.cover_path, p.created_at,
			   COALESCE(COUNT(pt.track_id), 0) as track_count, COALESCE(p.owner, '') as owner, COALESCE(p.shared, FALSE) as shared
		FROM playlists p
		LEFT JOIN playlist_tracks pt ON p.id = pt.playlist_id
		WHERE (p.owner IS NULL OR p.owner = '') OR p.shared = TRUE
		GROUP BY p.id, p.name, p.description, p.cover_path, p.created_at, p.owner, p.shared
		ORDER BY p.created_at DESC`)

	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var playlists []models.Playlist
	for rows.Next() {
		var playlist models.Playlist
		var coverPath sql.NullString
		err := rows.Scan(&playlist.ID, &playlist.Name, &playlist.Description,
			&coverPath, &playlist.CreatedAt, &playlist.TrackCount, &playlist.Owner, &playlist.Shared)
		if err != nil {
			return nil, err
		}
		if coverPath.Valid {
			playlist.CoverPath = coverPath.String
		}
		playlists = append(playlists, playlist)
	}

	return playlists, nil
}

// GetPlaylistTracks returns tracks for a playlist ordered by stored position.
func (db *Database) GetPlaylistTracks(playlistID int) ([]models.Track, error) {
	var query string
	if db.hasOwnerColumn {
		query = `
		SELECT t.id, t.title, t.artist, t.album, t.track_number, t.duration, t.file_path, t.file_size, t.has_album_art, t.album_art_id, COALESCE(t.owner, '') as owner
		FROM tracks t
		JOIN playlist_tracks pt ON t.id = pt.track_id
		WHERE pt.playlist_id = ?
		ORDER BY pt.position`
	} else {
		query = `
		SELECT t.id, t.title, t.artist, t.album, t.track_number, t.duration, t.file_path, t.file_size, t.has_album_art, t.album_art_id
		FROM tracks t
		JOIN playlist_tracks pt ON t.id = pt.track_id
		WHERE pt.playlist_id = ?
		ORDER BY pt.position`
	}

	rows, err := db.conn.Query(query, playlistID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanTrackRows(rows)
}

// AddTrackToPlaylist appends a track to the end of a playlist (if not already present).
func (db *Database) AddTrackToPlaylist(playlistID, trackID int) error {
	// Get the next position
	var maxPosition sql.NullInt64
	err := db.conn.QueryRow(`
		SELECT MAX(position) FROM playlist_tracks WHERE playlist_id = ?`,
		playlistID).Scan(&maxPosition)

	if err != nil && err != sql.ErrNoRows {
		return err
	}

	position := 1
	if maxPosition.Valid {
		position = int(maxPosition.Int64) + 1
	}

	_, err = db.conn.Exec(`
		INSERT INTO playlist_tracks (playlist_id, track_id, position)
		VALUES (?, ?, ?)
		ON CONFLICT(playlist_id, track_id) DO NOTHING`,
		playlistID, trackID, position)

	return err
}

// RemoveTrackFromPlaylist removes a specific track from the given playlist.
func (db *Database) RemoveTrackFromPlaylist(playlistID, trackID int) error {
	_, err := db.conn.Exec(`
		DELETE FROM playlist_tracks 
		WHERE playlist_id = ? AND track_id = ?`,
		playlistID, trackID)

	return err
}

// DeletePlaylist deletes the playlist and any playlist_tracks entries referencing it.
func (db *Database) DeletePlaylist(playlistID int) error {
	_, err := db.conn.Exec("DELETE FROM playlists WHERE id = ?", playlistID)
	return err
}

// UpdatePlaylist updates playlist metadata (name, description, cover path).
func (db *Database) UpdatePlaylist(playlistID int, name, description, coverPath string) error {
	_, err := db.conn.Exec(`
		UPDATE playlists 
		SET name = ?, description = ?, cover_path = ?
		WHERE id = ?`,
		name, description, coverPath, playlistID)
	return err
}

// UpdatePlaylistSharing updates the shared status of a playlist.
func (db *Database) UpdatePlaylistSharing(playlistID int, shared bool) error {
	_, err := db.conn.Exec(`
		UPDATE playlists 
		SET shared = ?
		WHERE id = ?`,
		shared, playlistID)
	return err
}

// GetPlaylistByID returns a playlist by its ID with ownership information.
func (db *Database) GetPlaylistByID(playlistID int) (*models.Playlist, error) {
	var playlist models.Playlist
	var coverPath sql.NullString

	err := db.conn.QueryRow(`
		SELECT p.id, p.name, p.description, p.cover_path, p.created_at,
			   COALESCE(COUNT(pt.track_id), 0) as track_count, COALESCE(p.owner, '') as owner, COALESCE(p.shared, FALSE) as shared
		FROM playlists p
		LEFT JOIN playlist_tracks pt ON p.id = pt.playlist_id
		WHERE p.id = ?
		GROUP BY p.id, p.name, p.description, p.cover_path, p.created_at, p.owner, p.shared`, playlistID).Scan(
		&playlist.ID, &playlist.Name, &playlist.Description,
		&coverPath, &playlist.CreatedAt, &playlist.TrackCount, &playlist.Owner, &playlist.Shared)

	if err != nil {
		if err == sql.ErrNoRows {
			return nil, fmt.Errorf("playlist with ID %d not found", playlistID)
		}
		return nil, err
	}

	if coverPath.Valid {
		playlist.CoverPath = coverPath.String
	}

	return &playlist, nil
}

// CheckPlaylistOwnership returns true if the user owns the playlist.
func (db *Database) CheckPlaylistOwnership(playlistID int, owner string) (bool, error) {
	var count int
	err := db.conn.QueryRow(`
		SELECT COUNT(*) FROM playlists WHERE id = ? AND owner = ?`,
		playlistID, owner).Scan(&count)

	if err != nil {
		return false, err
	}

	return count > 0, nil
}

// CheckPlaylistAccess returns true if the user can access the playlist (owns it or it's shared).
func (db *Database) CheckPlaylistAccess(playlistID int, owner string) (bool, error) {
	var count int
	err := db.conn.QueryRow(`
		SELECT COUNT(*) FROM playlists WHERE id = ? AND (owner = ? OR shared = TRUE OR owner IS NULL OR owner = '')`,
		playlistID, owner).Scan(&count)

	if err != nil {
		return false, err
	}

	return count > 0, nil
}

// CheckTrackAccessViaSharedPlaylists returns true if the track is accessible to the user via shared playlists.
func (db *Database) CheckTrackAccessViaSharedPlaylists(trackID int, username string) (bool, error) {
	var count int
	err := db.conn.QueryRow(`
		SELECT COUNT(DISTINCT p.id) 
		FROM playlists p
		JOIN playlist_tracks pt ON p.id = pt.playlist_id
		WHERE pt.track_id = ? AND (p.shared = TRUE OR p.owner = ? OR p.owner IS NULL OR p.owner = '')`,
		trackID, username).Scan(&count)

	if err != nil {
		return false, err
	}

	return count > 0, nil
}

// SearchTracks performs a simple LIKE-based search over title, artist and album.
func (db *Database) SearchTracks(query string) ([]models.Track, error) {
	searchQuery := "%" + query + "%"
	rows, err := db.searchTracksStmt.Query(searchQuery, searchQuery, searchQuery)
	if err != nil {
		db.logger.WithError(err).WithField("query", query).Error("Failed to search tracks")
		return nil, err
	}
	defer rows.Close()
	return scanTrackRows(rows)
}

// SearchMainLibraryTracks performs a search only on tracks from the main library (with empty/null owner).
func (db *Database) SearchMainLibraryTracks(query string) ([]models.Track, error) {
	searchQuery := "%" + query + "%"
	var querySQL string
	if db.hasOwnerColumn {
		querySQL = `
			SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, COALESCE(owner, '') as owner
			FROM tracks
			WHERE (title LIKE ? OR artist LIKE ? OR album LIKE ?) AND (owner IS NULL OR owner = '')
			ORDER BY artist, album, track_number, title`
	} else {
		querySQL = `
			SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id
			FROM tracks
			WHERE title LIKE ? OR artist LIKE ? OR album LIKE ?
			ORDER BY artist, album, track_number, title`
	}

	rows, err := db.conn.Query(querySQL, searchQuery, searchQuery, searchQuery)
	if err != nil {
		db.logger.WithError(err).WithField("query", query).Error("Failed to search main library tracks")
		return nil, err
	}
	defer rows.Close()
	return scanTrackRows(rows)
}

// SearchTracksForOwner performs a search for tracks belonging to a specific user.
func (db *Database) SearchTracksForOwner(query, owner string) ([]models.Track, error) {
	if !db.hasOwnerColumn {
		// If no owner column, return empty result for user-specific queries
		return []models.Track{}, nil
	}

	searchQuery := "%" + query + "%"
	rows, err := db.conn.Query(`
		SELECT id, title, artist, album, track_number, duration, file_path, file_size, has_album_art, album_art_id, COALESCE(owner, '') as owner
		FROM tracks
		WHERE (title LIKE ? OR artist LIKE ? OR album LIKE ?) AND owner = ?
		ORDER BY artist, album, track_number, title`, searchQuery, searchQuery, searchQuery, owner)
	if err != nil {
		db.logger.WithError(err).WithField("query", query).WithField("owner", owner).Error("Failed to search tracks for owner")
		return nil, err
	}
	defer rows.Close()
	return scanTrackRows(rows)
}

// RemoveTrackByPath deletes a track row identified by its file path.
func (db *Database) RemoveTrackByPath(filePath string) error {
	_, err := db.removeTrackStmt.Exec(filePath)
	if err != nil {
		db.logger.WithError(err).WithField("file_path", filePath).Error("Failed to remove track by path")
	}
	return err
}

// TrackExists returns true if a track exists with the given file path.
func (db *Database) TrackExists(filePath string) (bool, error) {
	var count int
	err := db.trackExistsStmt.QueryRow(filePath).Scan(&count)
	if err != nil {
		db.logger.WithError(err).WithField("file_path", filePath).Error("Failed to check if track exists")
		return false, err
	}
	return count > 0, nil
}

// DeleteTracksByOwner removes all tracks belonging to a specific user
func (db *Database) DeleteTracksByOwner(owner string) error {
	if !db.hasOwnerColumn {
		// If no owner column exists, nothing to delete
		return nil
	}

	result, err := db.conn.Exec("DELETE FROM tracks WHERE owner = ?", owner)
	if err != nil {
		db.logger.WithError(err).WithField("owner", owner).Error("Failed to delete tracks by owner")
		return err
	}

	rowsAffected, err := result.RowsAffected()
	if err != nil {
		db.logger.WithError(err).WithField("owner", owner).Error("Failed to get rows affected for delete tracks by owner")
		return err
	}

	db.logger.WithField("owner", owner).WithField("tracks_deleted", rowsAffected).Info("Deleted tracks for user")
	return nil
}

// Close closes the underlying database connection and prepared statements.
func (db *Database) Close() error {
	// Close prepared statements
	statements := []*sql.Stmt{
		db.insertTrackStmt,
		db.updateTrackStmt,
		db.getTrackByIDStmt,
		db.trackExistsStmt,
		db.removeTrackStmt,
		db.searchTracksStmt,
	}

	for _, stmt := range statements {
		if stmt != nil {
			if err := stmt.Close(); err != nil {
				db.logger.WithError(err).Error("Failed to close prepared statement")
			}
		}
	}

	// Close database connection
	if db.conn != nil {
		return db.conn.Close()
	}
	return nil
}

// UpsertDownloadJob inserts or updates a download job record by ID.
func (db *Database) UpsertDownloadJob(jobID, url, title, artist, status string, progress int, errMsg, outputPath, speed string, etaSeconds int, createdAt, completedAt *time.Time) error {
	_, err := db.conn.Exec(`
		INSERT INTO download_jobs (id, url, title, artist, status, progress, error, output_path, speed, eta_seconds, created_at, completed_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(id) DO UPDATE SET
			url=excluded.url,
			title=excluded.title,
			artist=excluded.artist,
			status=excluded.status,
			progress=excluded.progress,
			error=excluded.error,
			output_path=excluded.output_path,
			speed=excluded.speed,
			eta_seconds=excluded.eta_seconds,
			completed_at=excluded.completed_at
	`, jobID, url, title, artist, status, progress, errMsg, outputPath, speed, etaSeconds, createdAt, completedAt)
	return err
}

// GetAllDownloadJobs returns all persisted download jobs ordered by creation time.
func (db *Database) GetAllDownloadJobs() ([]map[string]interface{}, error) {
	rows, err := db.conn.Query(`SELECT id, url, title, artist, status, progress, error, output_path, speed, eta_seconds, created_at, completed_at FROM download_jobs ORDER BY created_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var jobs []map[string]interface{}
	for rows.Next() {
		var id, url, title, artist, status, errorMsg, outputPath, speed sql.NullString
		var progress sql.NullInt64
		var eta sql.NullInt64
		var createdAt, completedAt sql.NullString
		if err := rows.Scan(&id, &url, &title, &artist, &status, &progress, &errorMsg, &outputPath, &speed, &eta, &createdAt, &completedAt); err != nil {
			return nil, err
		}
		job := map[string]interface{}{
			"id": id.String, "url": url.String, "title": title.String, "artist": artist.String,
			"status": status.String, "progress": int(progress.Int64), "error": errorMsg.String,
			"output_path": outputPath.String, "speed": speed.String, "eta_seconds": int(eta.Int64),
			"created_at": createdAt.String, "completed_at": completedAt.String,
		}
		jobs = append(jobs, job)
	}
	return jobs, nil
}

// scanTrackRows scans standard track result sets into a slice of models.Track.
// It centralizes row iteration logic to reduce duplication across query
// helpers. Callers must have already deferred rows.Close().
func scanTrackRows(rows *sql.Rows) ([]models.Track, error) {
	var tracks []models.Track
	for rows.Next() {
		var track models.Track
		var albumArtID sql.NullString

		// Get column names to determine if owner column exists
		columns, err := rows.Columns()
		if err != nil {
			return nil, err
		}

		hasOwner := false
		for _, col := range columns {
			if col == "owner" {
				hasOwner = true
				break
			}
		}

		if hasOwner {
			if err := rows.Scan(&track.ID, &track.Title, &track.Artist, &track.Album,
				&track.TrackNumber, &track.Duration, &track.FilePath, &track.FileSize, &track.HasAlbumArt, &albumArtID, &track.Owner); err != nil {
				return nil, err
			}
		} else {
			if err := rows.Scan(&track.ID, &track.Title, &track.Artist, &track.Album,
				&track.TrackNumber, &track.Duration, &track.FilePath, &track.FileSize, &track.HasAlbumArt, &albumArtID); err != nil {
				return nil, err
			}
			track.Owner = "" // Set empty owner for backward compatibility
		}

		if albumArtID.Valid {
			track.AlbumArtID = albumArtID.String
		}
		tracks = append(tracks, track)
	}
	return tracks, nil
}

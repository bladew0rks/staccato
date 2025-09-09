package models

import "time"

// Track represents a music track in the system.
type Track struct {
	ID          int    `json:"id"`
	Title       string `json:"title"`
	Artist      string `json:"artist"`
	Album       string `json:"album"`
	TrackNumber int    `json:"trackNumber"`
	Duration    int    `json:"duration"` // in seconds
	FilePath    string `json:"-"`        // don't expose file path to client
	FileSize    int64  `json:"fileSize"`
	HasAlbumArt bool   `json:"hasAlbumArt"`
	AlbumArtID  string `json:"albumArtId,omitempty"` // For caching album art
	Owner       string `json:"-"`                    // don't expose owner to client, used for filtering
}

// Playlist represents a user-created playlist.
type Playlist struct {
	ID          int       `json:"id"`
	Name        string    `json:"name"`
	Description string    `json:"description,omitempty"`
	CoverPath   string    `json:"coverPath,omitempty"`
	CreatedAt   time.Time `json:"createdAt"`
	TrackCount  int       `json:"trackCount"`
	Owner       string    `json:"-"`       // don't expose owner to client, used for filtering
	Shared      bool      `json:"shared"`  // whether playlist is shared with all users
	IsOwner     bool      `json:"isOwner"` // whether current user owns this playlist (computed field)
}

// PlaylistTrack represents the relationship between playlists and tracks.
type PlaylistTrack struct {
	PlaylistID int `json:"playlistId"`
	TrackID    int `json:"trackId"`
	Position   int `json:"position"`
}

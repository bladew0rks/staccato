# Staccato

One terminal music player to rule them all.

## Features

* **Wide Format Support:** Plays MP3, FLAC, WAV, Ogg Vorbis, AAC, M4A, and ALAC files
* **Playback Controls:** Native gapless playback, ReplayGain, and all the other stuff you'd expect from something like this
* **UI & Visuals:** Multi-tab playlists, background folder scanning, embedded or folder album art, and a live spectrum display
* **Network & Search:** Direct Soulseek integration for search and downloads, plus local network library sharing without transcoding
* **Input Flexibility:** Complete keyboard navigation alongside full mouse support for controls, tabs, and columns

## Requirements

* **Rust:** Current stable toolchain
* **Linux:** ALSA development header files:
  `sudo apt install libasound2-dev pkg-config`
* **Terminal:** Minimum dimensions of 70 columns by 20 rows

## Quick Start

```sh
# Run from source
cargo run --release

# Run and load immediate files or folders
cargo run --release -- ~/Music album.flac

# Build standalone binary (found in target/release/)
cargo build --release
```

### Essential Keybindings

| Key | Action |
| --- | --- |
| `Ctrl+O` / `Ctrl+Shift+O` | Add files / add folder |
| `Space` | Play or pause |
| `Enter` | Play selection or add album |
| `Up` / `Down` | Navigate list |
| `Left` / `Right` | Seek backward / forward 5 seconds |
| `+` / `-` | Adjust volume |
| `Ctrl+F` | Filter active list |
| `Delete` | Remove selected item |
| `F1` | Display all keyboard shortcuts |

## Soulseek Integration

Open **Library > Search Soulseek** in the app, or pass credentials via environment variables:

```sh
export STACCATO_SOULSEEK_USER="your-username"
export STACCATO_SOULSEEK_PASSWORD="your-password"
cargo run --release
```

Downloaded tracks save automatically to your primary library folder and trigger an instant background rescan

## Local Network Sharing

Share your library across your LAN without configuring complex router settings:

```sh
# Host a library (prints pairing code, defaults to UDP 1744)
target/release/staccato serve ~/Music

# Connect from a remote client
target/release/staccato connect <HOST_IP>:1744 --code <PAIRING_CODE>
```

## Development & Verification

Run these standard quality checks before submitting pull requests:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

Distributed under the MIT License

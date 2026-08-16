use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::{
    action::Action,
    app::{App, Overlay, PickerMode},
    model::Focus,
    soulseek::SoulseekFormat,
    ui::UiRegions,
};

#[derive(Default)]
pub struct InputMapper {
    last_click: Option<(u16, u16, Instant)>,
}

impl InputMapper {
    pub fn map(&mut self, event: Event, app: &App, regions: &UiRegions) -> Action {
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                self.key(key, app)
            }
            Event::Mouse(mouse) => self.mouse(mouse, app, regions),
            Event::Paste(text) => paste_action(&text, app),
            _ => Action::None,
        }
    }

    fn key(&mut self, key: KeyEvent, app: &App) -> Action {
        match &app.overlay {
            Overlay::Help | Overlay::Properties { .. } => {
                return match key.code {
                    KeyCode::Esc | KeyCode::F(1) => Action::CloseOverlay,
                    _ => Action::None,
                };
            }
            Overlay::Menu { menu, .. } => {
                return match key.code {
                    KeyCode::Esc | KeyCode::F(10) => Action::CloseOverlay,
                    KeyCode::Up => Action::OverlayMove(-1),
                    KeyCode::Down => Action::OverlayMove(1),
                    KeyCode::Enter => Action::OverlayActivate,
                    KeyCode::Left => Action::OpenMenu((menu + 5) % 6),
                    KeyCode::Right => Action::OpenMenu((menu + 1) % 6),
                    _ => Action::None,
                };
            }
            Overlay::ContextMenu { .. } => {
                return match key.code {
                    KeyCode::Esc | KeyCode::F(10) => Action::CloseOverlay,
                    KeyCode::Up => Action::OverlayMove(-1),
                    KeyCode::Down => Action::OverlayMove(1),
                    KeyCode::Enter => Action::OverlayActivate,
                    _ => Action::None,
                };
            }
            Overlay::PathPicker(picker) => {
                return match key.code {
                    KeyCode::Esc => Action::CloseOverlay,
                    KeyCode::Up => Action::OverlayMove(-1),
                    KeyCode::Down => Action::OverlayMove(1),
                    KeyCode::PageUp => Action::OverlayMove(-10),
                    KeyCode::PageDown => Action::OverlayMove(10),
                    KeyCode::Enter => Action::OverlayActivate,
                    KeyCode::Char('a' | 'A') if picker.mode == PickerMode::Folder => {
                        Action::PickerChooseCurrent
                    }
                    _ => Action::None,
                };
            }
            Overlay::Rename { .. } | Overlay::Connect { .. } | Overlay::Pair { .. } => {
                return match key.code {
                    KeyCode::Esc => Action::CloseOverlay,
                    KeyCode::Enter => Action::OverlayActivate,
                    KeyCode::Backspace => Action::TextBackspace,
                    KeyCode::Char(character) => Action::TextInput(character),
                    _ => Action::None,
                };
            }
            Overlay::None => {}
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if app.captures_text() {
            return match key.code {
                KeyCode::Char('q') if ctrl => Action::Quit,
                KeyCode::Esc => Action::ClearFilter,
                KeyCode::Enter => Action::ActivateSelection,
                KeyCode::Backspace => Action::TextBackspace,
                KeyCode::Tab => Action::FocusNext(shift),
                KeyCode::BackTab => Action::FocusNext(true),
                KeyCode::Down => Action::MoveSelection(1),
                KeyCode::Up => Action::MoveSelection(-1),
                KeyCode::PageUp => Action::PageSelection(-1),
                KeyCode::PageDown => Action::PageSelection(1),
                KeyCode::Char(character) => Action::TextInput(character),
                _ => Action::None,
            };
        }

        match key.code {
            KeyCode::Esc
                if matches!(app.focus, Focus::AlbumList | Focus::Playlist)
                    && (!app.album_filter.is_empty() || !app.playlist_filter.is_empty()) =>
            {
                Action::ClearFilter
            }
            KeyCode::Char('q') if ctrl => Action::Quit,
            KeyCode::Char('f') if ctrl => Action::BeginFilter,
            KeyCode::Char('o') if ctrl && shift => Action::OpenFolder,
            KeyCode::Char('o') if ctrl => Action::OpenFiles,
            KeyCode::Char(',') if ctrl => Action::OpenSettings,
            KeyCode::Char('n') if ctrl => Action::NewPlaylist,
            KeyCode::Char('w') if ctrl => Action::ClosePlaylist,
            KeyCode::F(1) => Action::ToggleHelp,
            KeyCode::F(2) => Action::BeginRenamePlaylist,
            KeyCode::F(10) if shift => Action::OpenListContext {
                row: None,
                x: None,
                y: None,
            },
            KeyCode::Menu => Action::OpenListContext {
                row: None,
                x: None,
                y: None,
            },
            KeyCode::F(10) => Action::OpenMenu(0),
            KeyCode::Tab => Action::FocusNext(shift),
            KeyCode::BackTab => Action::FocusNext(true),
            KeyCode::Char(' ') if ctrl => Action::ToggleMark,
            KeyCode::Insert => Action::ToggleMark,
            KeyCode::Char(' ') => Action::TogglePlay,
            KeyCode::Enter => Action::ActivateSelection,
            KeyCode::Delete => Action::RemoveSelection,
            KeyCode::Up if alt => Action::MovePlaylistItems(-1),
            KeyCode::Down if alt => Action::MovePlaylistItems(1),
            KeyCode::Up if shift => Action::ExtendSelection(-1),
            KeyCode::Down if shift => Action::ExtendSelection(1),
            KeyCode::Up => Action::MoveSelection(-1),
            KeyCode::Down => Action::MoveSelection(1),
            KeyCode::PageUp => Action::PageSelection(-1),
            KeyCode::PageDown => Action::PageSelection(1),
            KeyCode::Left | KeyCode::Char('-')
                if app.settings_open && app.focus == Focus::Settings =>
            {
                Action::SettingsAdjust(-1)
            }
            KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('=')
                if app.settings_open && app.focus == Focus::Settings =>
            {
                Action::SettingsAdjust(1)
            }
            KeyCode::Left if app.soulseek_open && app.focus == Focus::SoulseekFilter => {
                Action::SoulseekCycleFormat(-1)
            }
            KeyCode::Right if app.soulseek_open && app.focus == Focus::SoulseekFilter => {
                Action::SoulseekCycleFormat(1)
            }
            KeyCode::Left if app.soulseek_open && app.focus == Focus::Playlist => {
                Action::SoulseekFold(false)
            }
            KeyCode::Right if app.soulseek_open && app.focus == Focus::Playlist => {
                Action::SoulseekFold(true)
            }
            KeyCode::Left if app.focus == Focus::PlaylistTabs => {
                let count = app.tab_count();
                Action::SelectPlaylist((app.selected_tab() + count - 1) % count)
            }
            KeyCode::Right if app.focus == Focus::PlaylistTabs => {
                let count = app.tab_count();
                Action::SelectPlaylist((app.selected_tab() + 1) % count)
            }
            KeyCode::Left => Action::SeekRelative(-5),
            KeyCode::Right => Action::SeekRelative(5),
            KeyCode::Char('+') | KeyCode::Char('=') => Action::VolumeRelative(0.05),
            KeyCode::Char('-') => Action::VolumeRelative(-0.05),
            _ => Action::None,
        }
    }

    fn mouse(&mut self, mouse: MouseEvent, app: &App, regions: &UiRegions) -> Action {
        let point = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => return Action::MoveSelection(-3),
            MouseEventKind::ScrollDown => return Action::MoveSelection(3),
            MouseEventKind::Down(MouseButton::Left) => {}
            MouseEventKind::Down(MouseButton::Right) => {
                return self.right_click(mouse, app, regions, point);
            }
            _ => return Action::None,
        }

        if let Overlay::ContextMenu { .. } = app.overlay {
            if let Some(area) = regions.overlay
                && contains(area, point)
            {
                let row = mouse.row.saturating_sub(area.y + 1) as usize;
                return Action::ActivateMenuItem(row);
            }
            return Action::CloseOverlay;
        }

        if let Overlay::Menu { .. } = app.overlay {
            if let Some(area) = regions.overlay
                && contains(area, point)
            {
                let row = mouse.row.saturating_sub(area.y + 1) as usize;
                return Action::ActivateMenuItem(row);
            }
            if let Some((index, _)) = regions
                .menu
                .iter()
                .enumerate()
                .find(|(_, rect)| contains(**rect, point))
            {
                return Action::OpenMenu(index);
            }
            return Action::CloseOverlay;
        } else if matches!(app.overlay, Overlay::PathPicker(_)) {
            if let Some(area) = regions.overlay
                && contains(area, point)
            {
                let row = mouse.row.saturating_sub(area.y + 3) as usize + regions.overlay_offset;
                let selected = match &app.overlay {
                    Overlay::PathPicker(picker) => picker.selected,
                    _ => 0,
                };
                let double = self.last_click.is_some_and(|(x, y, at)| {
                    x == mouse.column
                        && y == mouse.row
                        && at.elapsed() <= Duration::from_millis(450)
                });
                self.last_click = Some((mouse.column, mouse.row, Instant::now()));
                return if double && row == selected {
                    Action::OverlayActivate
                } else {
                    Action::OverlayMove(row as i32 - selected as i32)
                };
            }
            return Action::None;
        } else if !matches!(app.overlay, Overlay::None) {
            return Action::None;
        }

        if let Some((index, _)) = regions
            .menu
            .iter()
            .enumerate()
            .find(|(_, rect)| contains(**rect, point))
        {
            return Action::OpenMenu(index);
        }
        if contains(regions.previous, point) {
            return Action::Previous;
        }
        if contains(regions.play_pause, point) {
            return Action::TogglePlay;
        }
        if contains(regions.stop, point) {
            return Action::Stop;
        }
        if contains(regions.next, point) {
            return Action::Next;
        }
        if contains(regions.order, point) {
            return Action::CyclePlaybackOrder;
        }
        if contains(regions.seek, point) && regions.seek.width > 0 {
            return Action::SeekFraction(
                f64::from(mouse.column.saturating_sub(regions.seek.x))
                    / f64::from(regions.seek.width),
            );
        }
        if contains(regions.volume, point) && regions.volume.width > 0 {
            return Action::SetVolume(
                f32::from(mouse.column.saturating_sub(regions.volume.x))
                    / f32::from(regions.volume.width),
            );
        }
        if let Some((index, _)) = regions
            .tabs
            .iter()
            .enumerate()
            .find(|(_, rect)| contains(**rect, point))
        {
            return Action::SelectPlaylist(index);
        }
        if contains(regions.album_filter, point) {
            return Action::BeginFilter;
        }
        if contains(regions.playlist_filter, point) {
            return Action::BeginFilter;
        }
        if let Some((column, _)) = regions
            .playlist_headers
            .iter()
            .find(|(_, rect)| contains(*rect, point))
        {
            return Action::SortPlaylist(*column);
        }
        if contains(regions.soulseek_query, point) && app.soulseek_open {
            return Action::BeginSoulseek;
        }
        if app.soulseek_open {
            if let Some((index, _)) = regions
                .soulseek_formats
                .iter()
                .enumerate()
                .find(|(_, rect)| contains(**rect, point))
            {
                return Action::SoulseekSetFormat(SoulseekFormat::ALL[index]);
            }
            if contains(regions.soulseek_slots, point) {
                return Action::SoulseekToggleFreeSlot;
            }
            if contains(regions.soulseek_filter, point) {
                return Action::SoulseekCycleFormat(0);
            }
        }
        if contains(regions.album, point) {
            let row = mouse.row.saturating_sub(regions.album.y + 1) as usize + regions.album_offset;
            if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                let delta = row as i32 - app.album_selection as i32;
                return Action::ExtendSelection(delta);
            }
            return Action::SelectAlbumRow(row);
        }
        if contains(regions.playlist, point) {
            let header = if app.settings_open { 1 } else { 2 };
            let row = mouse.row.saturating_sub(regions.playlist.y + header) as usize
                + regions.playlist_offset;
            let double = self.last_click.is_some_and(|(x, y, at)| {
                x == mouse.column && y == mouse.row && at.elapsed() <= Duration::from_millis(450)
            });
            self.last_click = Some((mouse.column, mouse.row, Instant::now()));
            if double {
                return Action::ActivateSelection;
            }
            if mouse.modifiers.contains(KeyModifiers::CONTROL)
                && !app.soulseek_open
                && !app.queue_open
                && !app.settings_open
            {
                return Action::TogglePlaylistRow(row);
            }
            if mouse.modifiers.contains(KeyModifiers::SHIFT)
                && !app.soulseek_open
                && !app.queue_open
                && !app.settings_open
            {
                let visible = app.visible_playlist_indices();
                let current = visible
                    .iter()
                    .position(|&index| index == app.playlist_selection)
                    .unwrap_or(0);
                return Action::ExtendSelection(row as i32 - current as i32);
            }
            return Action::SelectPlaylistRow(row);
        }
        Action::None
    }

    fn right_click(
        &self,
        mouse: MouseEvent,
        app: &App,
        regions: &UiRegions,
        point: (u16, u16),
    ) -> Action {
        if let Overlay::ContextMenu { .. } = app.overlay {
            return Action::CloseOverlay;
        }
        if !matches!(app.overlay, Overlay::None) {
            return Action::None;
        }
        if app.settings_open {
            return Action::None;
        }
        if contains(regions.playlist, point) {
            let row =
                mouse.row.saturating_sub(regions.playlist.y + 2) as usize + regions.playlist_offset;
            return Action::OpenListContext {
                row: Some(row),
                x: Some(mouse.column),
                y: Some(mouse.row),
            };
        }
        if contains(regions.album, point) {
            let row = mouse.row.saturating_sub(regions.album.y + 1) as usize + regions.album_offset;
            return Action::OpenListContext {
                row: Some(row),
                x: Some(mouse.column),
                y: Some(mouse.row),
            };
        }
        Action::None
    }
}

fn contains(rect: ratatui::layout::Rect, point: (u16, u16)) -> bool {
    point.0 >= rect.x && point.0 < rect.right() && point.1 >= rect.y && point.1 < rect.bottom()
}

pub(crate) fn paste_action(text: &str, app: &App) -> Action {
    match &app.overlay {
        Overlay::Rename { .. } | Overlay::Connect { .. } | Overlay::Pair { .. } => {
            Action::PasteText(text.to_owned())
        }
        Overlay::Help
        | Overlay::Menu { .. }
        | Overlay::ContextMenu { .. }
        | Overlay::Properties { .. }
        | Overlay::PathPicker(_)
        | Overlay::None => {
            let paths = crate::drop::parse_dropped_paths(text);
            if !paths.is_empty() {
                tracing::info!(count = paths.len(), "paste contained file paths");
                Action::AddPaths(paths)
            } else if app.captures_text() {
                Action::PasteText(text.to_owned())
            } else {
                Action::None
            }
        }
    }
}

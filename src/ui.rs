use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
        Tabs, Wrap,
    },
};
use throbber_widgets_tui::{ASCII, Throbber, WhichUse};
use tui_equalizer::{Band, Equalizer};

use crate::{
    app::{App, Overlay, PickerMode, menu_actions},
    cover,
    model::{Focus, PlaybackState, PlaylistColumn, format_duration},
    soulseek::{SoulseekFormat, SoulseekPhase, SoulseekRowKind},
};

pub const MIN_WIDTH: u16 = 70;
pub const MIN_HEIGHT: u16 = 20;
const MENU_LABELS: [&str; 6] = ["File", "Edit", "View", "Playback", "Library", "Help"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconSet {
    #[default]
    Unicode,
    NerdFont,
}

impl IconSet {
    fn transport(self, playing: bool) -> [&'static str; 4] {
        match self {
            Self::Unicode => ["⏮", if playing { "⏸" } else { "▶" }, "⏹", "⏭"],
            Self::NerdFont => [
                "\u{f04ae}",
                if playing { "\u{f03e4}" } else { "\u{f040a}" },
                "\u{f04db}",
                "\u{f04ad}",
            ],
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct UiRegions {
    pub menu: Vec<Rect>,
    pub previous: Rect,
    pub play_pause: Rect,
    pub stop: Rect,
    pub next: Rect,
    pub seek: Rect,
    pub order: Rect,
    pub volume: Rect,
    pub album: Rect,
    pub album_offset: usize,
    pub tabs: Vec<Rect>,
    pub playlist: Rect,
    pub playlist_offset: usize,
    pub equalizer: Rect,
    pub soulseek_query: Rect,
    pub soulseek_filter: Rect,
    pub soulseek_formats: Vec<Rect>,
    pub soulseek_slots: Rect,
    pub album_filter: Rect,
    pub playlist_filter: Rect,
    pub playlist_headers: Vec<(PlaylistColumn, Rect)>,
    pub overlay_offset: usize,
    pub overlay: Option<Rect>,
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App, icons: IconSet) -> UiRegions {
    let area = frame.area();
    frame.render_widget(Block::default().style(base_style()), area);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let message = format!(
            "Staccato needs at least {MIN_WIDTH}×{MIN_HEIGHT}\nCurrent terminal: {}×{}\n\nPlayback continues while you resize.",
            area.width, area.height
        );
        frame.render_widget(
            Paragraph::new(message)
                .alignment(Alignment::Center)
                .block(Block::bordered().title(" Staccato "))
                .style(base_style()),
            centered(area, 52, 8),
        );
        return UiRegions::default();
    }

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .split(area);
    let mut regions = UiRegions::default();
    render_menu(frame, vertical[0], app, &mut regions);
    render_toolbar(frame, vertical[1], app, icons, &mut regions);
    render_content(frame, vertical[2], app, &mut regions);
    render_status(frame, vertical[3], app);
    render_overlay(frame, app, &mut regions);
    regions
}

fn render_menu(frame: &mut Frame<'_>, area: Rect, app: &App, regions: &mut UiRegions) {
    frame.render_widget(Block::default().style(chrome_style()), area);
    let mut x = area.x;
    for (index, label) in MENU_LABELS.iter().enumerate() {
        let width = label.len() as u16 + 2;
        let rect = Rect::new(x, area.y, width, 1);
        let active = matches!(app.overlay, Overlay::Menu { menu, .. } if menu == index);
        frame.render_widget(
            Paragraph::new(format!(" {label} ")).style(if active {
                selected_style()
            } else {
                chrome_style()
            }),
            rect,
        );
        regions.menu.push(rect);
        x += width;
    }
}

fn render_toolbar(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    icons: IconSet,
    regions: &mut UiRegions,
) {
    frame.render_widget(Block::default().style(chrome_style()), area);

    const CONTROL_WIDTH: u16 = 5;
    const MIN_SEEK: u16 = 4;
    let controls_width = CONTROL_WIDTH * 4;
    let controls = Layout::horizontal([Constraint::Length(CONTROL_WIDTH); 4]).split(Rect::new(
        area.x,
        area.y,
        controls_width.min(area.width),
        1,
    ));
    regions.previous = controls[0];
    regions.play_pause = controls[1];
    regions.stop = controls[2];
    regions.next = controls[3];
    let playing = app.audio_snapshot.state == PlaybackState::Playing;
    let buttons = if app.focus == Focus::Toolbar {
        button_style().fg(accent())
    } else {
        button_style()
    };
    for (rect, icon) in controls.iter().zip(icons.transport(playing)) {
        frame.render_widget(
            Paragraph::new(icon)
                .alignment(Alignment::Center)
                .style(buttons),
            *rect,
        );
    }

    let volume_label = format!("Vol: {:>3.0}%", app.audio_snapshot.volume * 100.0);
    let order_label = format!("Order: {}", app.playback_order.label());
    let elapsed = format_duration(app.audio_snapshot.position);
    let total = format_duration(app.audio_snapshot.duration);
    let volume_width = text_width(&volume_label);
    let order_natural = text_width(&order_label);
    let elapsed_width = text_width(&elapsed);
    let total_width = text_width(&total);

    regions.volume = Rect::new(
        area.right().saturating_sub(volume_width),
        area.y,
        volume_width.min(area.width),
        1,
    );
    frame.render_widget(
        Paragraph::new(volume_label)
            .style(chrome_style())
            .alignment(Alignment::Right),
        regions.volume,
    );

    let after_buttons = area.x + controls_width.min(area.width);
    let available = regions.volume.x.saturating_sub(after_buttons);
    let reserved = 5 + elapsed_width + MIN_SEEK + total_width;
    let order_width = if available > reserved {
        order_natural.min(available - reserved)
    } else {
        0
    };
    if order_width > 0 {
        regions.order = Rect::new(
            regions.volume.x.saturating_sub(order_width + 1),
            area.y,
            order_width,
            1,
        );
        frame.render_widget(
            Paragraph::new(order_label).style(chrome_style()),
            regions.order,
        );
    }

    let start = after_buttons.saturating_add(1);
    let end = if regions.order.width > 0 {
        regions.order.x
    } else {
        regions.volume.x
    }
    .saturating_sub(1);
    if end > start {
        let width = end - start;
        if width >= elapsed_width + total_width + MIN_SEEK + 2 {
            frame.render_widget(
                Paragraph::new(elapsed).style(chrome_style()),
                Rect::new(start, area.y, elapsed_width, 1),
            );
            frame.render_widget(
                Paragraph::new(total)
                    .style(chrome_style())
                    .alignment(Alignment::Right),
                Rect::new(end.saturating_sub(total_width), area.y, total_width, 1),
            );
            regions.seek = Rect::new(
                start + elapsed_width + 1,
                area.y,
                end.saturating_sub(total_width + 1)
                    .saturating_sub(start + elapsed_width + 1),
                1,
            );
        } else {
            regions.seek = Rect::new(start, area.y, width, 1);
        }
        render_seek_bar(frame, regions.seek, seek_ratio(app));
    }
}

fn seek_ratio(app: &App) -> f64 {
    if app.audio_snapshot.duration.is_zero() {
        0.0
    } else {
        (app.audio_snapshot.position.as_secs_f64() / app.audio_snapshot.duration.as_secs_f64())
            .clamp(0.0, 1.0)
    }
}

fn render_seek_bar(frame: &mut Frame<'_>, area: Rect, ratio: f64) {
    if area.width == 0 {
        return;
    }
    let filled = ((f64::from(area.width) * ratio).round() as u16).min(area.width);
    let empty = area.width - filled;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "█".repeat(filled as usize),
                Style::default().fg(Color::Green),
            ),
            Span::styled("░".repeat(empty as usize), unavailable_style()),
        ])),
        area,
    );
}

fn text_width(text: &str) -> u16 {
    text.chars().count() as u16
}

fn render_content(frame: &mut Frame<'_>, area: Rect, app: &mut App, regions: &mut UiRegions) {
    let album_width = ((area.width as f32 * 0.30) as u16).clamp(24, 40);
    let horizontal =
        Layout::horizontal([Constraint::Length(album_width), Constraint::Min(30)]).split(area);

    let art_height = if app.show_album_art {
        cover::art_panel_height(horizontal[0].width, area.height)
    } else {
        0
    };
    let left = if art_height >= 6 {
        Layout::vertical([Constraint::Min(6), Constraint::Length(art_height)]).split(horizontal[0])
    } else {
        Layout::vertical([Constraint::Min(6)]).split(horizontal[0])
    };
    regions.album_offset = render_album_list(frame, left[0], app, regions);
    if left.len() > 1 {
        render_album_art(frame, left[1], app);
    }

    let right = Layout::vertical([Constraint::Length(3), Constraint::Min(5)]).split(horizontal[1]);
    render_tabs(frame, right[0], app, regions);
    if app.soulseek_open {
        regions.playlist_offset = render_soulseek(frame, right[1], app, regions);
    } else if app.queue_open {
        regions.playlist = right[1];
        regions.playlist_offset = render_queue(frame, right[1], app);
    } else if app.settings_open {
        regions.playlist = right[1];
        regions.playlist_offset = render_settings(frame, right[1], app);
    } else {
        let eq_height = if app.show_spectrum {
            equalizer_height(right[1].height)
        } else {
            0
        };
        if eq_height > 0 {
            let split = Layout::vertical([Constraint::Min(5), Constraint::Length(eq_height)])
                .split(right[1]);
            regions.playlist_offset = render_playlist(frame, split[0], app, regions);
            regions.equalizer = split[1];
            render_equalizer(frame, split[1], app);
        } else {
            regions.playlist_offset = render_playlist(frame, right[1], app, regions);
        }
    }
}

fn equalizer_height(body_height: u16) -> u16 {
    const PLAYLIST_MIN: u16 = 5;
    const PREFERRED: u16 = 8;
    const COMPACT: u16 = 6;
    if body_height >= PLAYLIST_MIN + PREFERRED {
        PREFERRED
    } else if body_height >= PLAYLIST_MIN + COMPACT {
        COMPACT
    } else {
        0
    }
}

fn render_equalizer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let block = focused_block(" Equalizer ", false);
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(block, area);
    if inner.width < 2 || inner.height == 0 {
        return;
    }
    let count = usize::from(inner.width / 2);
    let bands = crate::spectrum::resample_spectrum(&app.spectrum(), count)
        .into_iter()
        .map(Band::from)
        .collect();
    frame.render_widget(
        Equalizer {
            bands,
            brightness: 1.0,
        },
        inner,
    );
}

fn render_album_art(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = format!(" Album Art ({}) ", app.covers.protocol_label());
    let block = focused_block(title, false);
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    if app.covers.has_image() {
        app.covers.render(frame, inner);
    } else {
        let label = match app.cover_track() {
            Some(_) => "No embedded or folder artwork",
            None => "No track selected",
        };
        frame.render_widget(
            Paragraph::new(label)
                .alignment(Alignment::Center)
                .style(unavailable_style()),
            inner,
        );
    }
    frame.render_widget(block, area);
}

fn render_album_list(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    regions: &mut UiRegions,
) -> usize {
    let show_filter = app.focus == Focus::AlbumFilter || !app.album_filter.is_empty();
    let (filter_area, list_area) = if show_filter {
        let split = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).split(area);
        (Some(split[0]), split[1])
    } else {
        (None, area)
    };
    if let Some(filter_area) = filter_area {
        regions.album_filter = filter_area;
        let caret = app.focus == Focus::AlbumFilter;
        frame.render_widget(
            Paragraph::new(format!(
                "Filter: {}{}",
                app.album_filter,
                if caret { "█" } else { "" }
            ))
            .block(focused_block(" Filter ", caret)),
            filter_area,
        );
    }
    regions.album = list_area;
    let entries = app.visible_album_entries();
    let items: Vec<ListItem<'_>> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let prefix = if entry.track_id.is_some() {
                match entry.depth {
                    0 | 1 => "      ",
                    2 => "        ",
                    _ => "          ",
                }
            } else {
                match entry.depth {
                    0 => "▾ ",
                    1 => "  ▾ ",
                    _ => "    ▾ ",
                }
            };
            let marker = if entry.unavailable {
                " [unplayable]"
            } else {
                ""
            };
            let marked = app.album_marked.contains(&index);
            ListItem::new(format!("{prefix}{}{marker}", entry.label)).style(if entry.unavailable {
                unavailable_style()
            } else if marked && index != app.album_selection {
                marked_style()
            } else if entry.depth == 0 {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                base_style()
            })
        })
        .collect();
    let title = if app.album_filter.is_empty() {
        " Album List ".to_owned()
    } else {
        format!(" Album List  [{}] ", entries.len())
    };
    let block = focused_block(title, app.focus == Focus::AlbumList);
    let mut state = ListState::default()
        .with_selected((!items.is_empty()).then_some(app.album_selection))
        .with_offset(app.album_scroll_offset);
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(selected_style()),
        list_area,
        &mut state,
    );
    app.album_scroll_offset = state.offset();
    state.offset()
}

fn render_tabs(frame: &mut Frame<'_>, area: Rect, app: &App, regions: &mut UiRegions) {
    let queue_label = if app.queue.is_empty() {
        "Queue".to_owned()
    } else {
        format!("Queue ({})", app.queue.len())
    };
    let labels: Vec<String> = app
        .playlists
        .iter()
        .map(|playlist| playlist.name.clone())
        .chain([queue_label, "Soulseek".into(), "Preferences".into()])
        .collect();
    let titles = labels.iter().map(|label| Line::from(label.as_str()));
    let selected = app.selected_tab();
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .block(focused_block(
                " Playlists ",
                app.focus == Focus::PlaylistTabs,
            ))
            .style(base_style())
            .highlight_style(selected_style()),
        area,
    );
    let mut x = area.x + 1;
    for label in &labels {
        let width =
            (Line::from(label.as_str()).width() as u16 + 2).min(area.right().saturating_sub(x));
        if width == 0 {
            break;
        }
        regions.tabs.push(Rect::new(x, area.y + 1, width, 1));
        x += width + 1;
    }
}

fn render_settings(frame: &mut Frame<'_>, area: Rect, app: &mut App) -> usize {
    let items = crate::settings::ROWS.iter().map(|row| match row {
        crate::settings::SettingRow::Header(title) => ListItem::new(*title).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        crate::settings::SettingRow::Item(id) => {
            let value = app.setting_value(*id);
            let pad = 28usize.saturating_sub(id.label().chars().count());
            ListItem::new(format!("  {}{} {}", id.label(), " ".repeat(pad), value))
        }
    });
    let list = List::new(items)
        .block(focused_block(" Preferences ", app.focus == Focus::Settings))
        .style(base_style())
        .highlight_style(selected_style());
    let mut state = ListState::default()
        .with_selected(Some(app.settings_selected))
        .with_offset(app.settings_scroll_offset);
    frame.render_stateful_widget(list, area, &mut state);
    app.settings_scroll_offset = state.offset();
    state.offset()
}

fn render_soulseek(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    regions: &mut UiRegions,
) -> usize {
    let login = matches!(
        app.soulseek_ui.phase,
        SoulseekPhase::Username | SoulseekPhase::Password
    );
    let split = if login {
        Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).split(area)
    } else {
        Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(3),
        ])
        .split(area)
    };
    regions.soulseek_query = split[0];
    let results = if login {
        regions.playlist = split[1];
        split[1]
    } else {
        render_soulseek_filters(frame, split[1], app, regions);
        regions.playlist = split[2];
        split[2]
    };

    let caret = app.focus == Focus::SoulseekQuery || login;
    let title = if login { " Sign in " } else { " Search " };
    frame.render_widget(
        Paragraph::new(app.soulseek_ui.prompt(caret)).block(focused_block(
            title,
            app.focus == Focus::SoulseekQuery || (login && app.focus != Focus::Playlist),
        )),
        split[0],
    );

    let rows = app.soulseek_ui.visible_rows();
    if rows.is_empty() {
        let hint = match app.soulseek_ui.phase {
            SoulseekPhase::Username => {
                "Sign in with your Soulseek account.\nCredentials stay in the data directory."
            }
            SoulseekPhase::Password => "Enter password, then press Enter.",
            SoulseekPhase::Ready => {
                if app.soulseek_ui.filter.is_active() && !app.soulseek_ui.hits.is_empty() {
                    "No files match the current filters."
                } else {
                    app.soulseek_ui.status.as_str()
                }
            }
        };
        frame.render_widget(
            Paragraph::new(hint)
                .block(focused_block(
                    format!(" {} ", app.soulseek_ui.status),
                    app.focus == Focus::Playlist,
                ))
                .style(base_style()),
            results,
        );
        return 0;
    }

    let table_rows = rows.iter().map(|row| {
        let style = match row.kind {
            SoulseekRowKind::User => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            SoulseekRowKind::Folder | SoulseekRowKind::File => base_style(),
        };
        Row::new(vec![row.label.clone(), row.detail.clone()]).style(style)
    });
    let table = Table::new(table_rows, [Constraint::Min(24), Constraint::Length(22)])
        .header(
            Row::new(["User / Folder / File", "Info"])
                .style(chrome_style().add_modifier(Modifier::BOLD)),
        )
        .block(focused_block(
            format!(" {} ", app.soulseek_ui.status),
            app.focus == Focus::Playlist,
        ))
        .row_highlight_style(selected_style());
    let mut state = TableState::default()
        .with_selected(Some(app.soulseek_ui.selected))
        .with_offset(app.soulseek_scroll_offset);
    frame.render_stateful_widget(table, results, &mut state);
    app.soulseek_scroll_offset = state.offset();
    state.offset()
}

fn render_soulseek_filters(frame: &mut Frame<'_>, area: Rect, app: &App, regions: &mut UiRegions) {
    regions.soulseek_filter = area;
    let focused = app.focus == Focus::SoulseekFilter;
    frame.render_widget(
        focused_block(
            " Filters  ·  Left/Right format  ·  Enter free slots ",
            focused,
        ),
        area,
    );
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let mut x = inner.x;
    for format in SoulseekFormat::ALL {
        let label = format!(" {} ", format.label());
        let width = (label.chars().count() as u16).min(inner.right().saturating_sub(x));
        if width == 0 {
            break;
        }
        let rect = Rect::new(x, inner.y, width, 1);
        let active = app.soulseek_ui.filter.format == format;
        frame.render_widget(
            Paragraph::new(label).style(if active {
                selected_style()
            } else {
                base_style()
            }),
            rect,
        );
        regions.soulseek_formats.push(rect);
        x = x.saturating_add(width);
    }

    let slots = if app.soulseek_ui.filter.free_slot {
        " Slots: free "
    } else {
        " Slots: any "
    };
    let width = slots.chars().count() as u16;
    if inner.right().saturating_sub(x) > width {
        let rect = Rect::new(inner.right().saturating_sub(width), inner.y, width, 1);
        frame.render_widget(
            Paragraph::new(slots).style(if app.soulseek_ui.filter.free_slot {
                selected_style()
            } else {
                chrome_style()
            }),
            rect,
        );
        regions.soulseek_slots = rect;
    }
}

fn render_playlist(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut App,
    regions: &mut UiRegions,
) -> usize {
    let show_filter = app.focus == Focus::PlaylistFilter || !app.playlist_filter.is_empty();
    let (filter_area, list_area) = if show_filter {
        let split = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).split(area);
        (Some(split[0]), split[1])
    } else {
        (None, area)
    };
    if let Some(filter_area) = filter_area {
        regions.playlist_filter = filter_area;
        let caret = app.focus == Focus::PlaylistFilter;
        frame.render_widget(
            Paragraph::new(format!(
                "Filter: {}{}",
                app.playlist_filter,
                if caret { "█" } else { "" }
            ))
            .block(focused_block(" Filter ", caret)),
            filter_area,
        );
    }
    regions.playlist = list_area;
    let compact = list_area.width < 90;
    let visible = app.visible_playlist_indices();
    let rows = visible.iter().map(|&index| {
        let id = app.active_playlist().items[index];
        playlist_row(app, index, id, compact)
    });
    let (headers, widths) = playlist_columns(compact, app.playlist_sort);
    let title = match app.playlist_sort {
        Some((column, ascending)) => format!(
            " Playlist View  ·  {} {} ",
            column.label(),
            if ascending { "↑" } else { "↓" }
        ),
        None if !app.playlist_filter.is_empty() => {
            format!(" Playlist View  [{}] ", visible.len())
        }
        None => " Playlist View ".into(),
    };
    let table = Table::new(rows, widths)
        .header(Row::new(headers).style(chrome_style().add_modifier(Modifier::BOLD)))
        .block(focused_block(title, app.focus == Focus::Playlist))
        .row_highlight_style(selected_style());
    let selected = visible
        .iter()
        .position(|&index| index == app.playlist_selection);
    let mut state = TableState::default()
        .with_selected(selected)
        .with_offset(app.playlist_scroll_offset);
    frame.render_stateful_widget(table, list_area, &mut state);
    app.playlist_scroll_offset = state.offset();
    regions.playlist_headers = header_rects(list_area, compact);
    state.offset()
}

fn render_queue(frame: &mut Frame<'_>, area: Rect, app: &mut App) -> usize {
    let compact = area.width < 90;
    let rows = app
        .queue
        .iter()
        .enumerate()
        .map(|(index, id)| playlist_row(app, index, *id, compact));
    let (headers, widths) = playlist_columns(compact, None);
    let table = Table::new(rows, widths)
        .header(Row::new(headers).style(chrome_style().add_modifier(Modifier::BOLD)))
        .block(focused_block(
            format!(" Playback Queue  ({}) ", app.queue.len()),
            app.focus == Focus::Queue,
        ))
        .row_highlight_style(selected_style());
    let mut state = TableState::default()
        .with_selected((!app.queue.is_empty()).then_some(app.queue_selection))
        .with_offset(app.queue_scroll_offset);
    frame.render_stateful_widget(table, area, &mut state);
    app.queue_scroll_offset = state.offset();
    state.offset()
}

fn playlist_row(app: &App, index: usize, id: crate::model::TrackId, compact: bool) -> Row<'static> {
    let Some(track) = app.tracks.get(&id) else {
        return Row::new(vec!["!", "", "Missing track", "", "", ""]).style(error_style());
    };
    let is_playing = app.audio_snapshot.track_id == Some(id);
    let marker = if is_playing {
        match app.audio_snapshot.state {
            PlaybackState::Playing => "▶",
            PlaybackState::Paused => "Ⅱ",
            PlaybackState::Loading | PlaybackState::Buffering => "…",
            PlaybackState::Stopped => "",
        }
    } else if track.unavailable || track.scan_error.is_some() {
        "!"
    } else if app.playlist_marked.contains(&id) && !app.queue_open {
        "•"
    } else {
        ""
    };
    let number = track
        .track_number
        .map(|n| n.to_string())
        .unwrap_or_else(|| (index + 1).to_string());
    let style = if track.unavailable || track.scan_error.is_some() {
        unavailable_style()
    } else if app.playlist_marked.contains(&id)
        && Some(index) != Some(app.playlist_selection)
        && !app.queue_open
    {
        marked_style()
    } else {
        base_style()
    };
    let marker = Cell::from(marker).style(if is_playing {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        style
    });
    if compact {
        Row::new(vec![
            marker,
            Cell::from(number),
            Cell::from(track.artist.clone()),
            Cell::from(track.title.clone()),
            Cell::from(format_duration(track.duration)),
        ])
        .style(style)
    } else {
        Row::new(vec![
            marker,
            Cell::from(number),
            Cell::from(track.artist.clone()),
            Cell::from(track.title.clone()),
            Cell::from(track.album.clone()),
            Cell::from(track.date.map(|d| d.to_string()).unwrap_or_default()),
            Cell::from(format_duration(track.duration)),
        ])
        .style(style)
    }
}

fn playlist_columns(
    compact: bool,
    sort: Option<(PlaylistColumn, bool)>,
) -> (Vec<String>, Vec<Constraint>) {
    let decorate = |column: PlaylistColumn| {
        let mut label = column.label().to_owned();
        if let Some((current, ascending)) = sort
            && current == column
        {
            label.push(if ascending { '↑' } else { '↓' });
        }
        label
    };
    if compact {
        (
            vec![
                String::new(),
                decorate(PlaylistColumn::Number),
                decorate(PlaylistColumn::Artist),
                decorate(PlaylistColumn::Title),
                decorate(PlaylistColumn::Time),
            ],
            vec![
                Constraint::Length(2),
                Constraint::Length(4),
                Constraint::Percentage(28),
                Constraint::Percentage(52),
                Constraint::Length(8),
            ],
        )
    } else {
        (
            vec![
                String::new(),
                decorate(PlaylistColumn::Number),
                decorate(PlaylistColumn::Artist),
                decorate(PlaylistColumn::Title),
                decorate(PlaylistColumn::Album),
                decorate(PlaylistColumn::Date),
                decorate(PlaylistColumn::Time),
            ],
            vec![
                Constraint::Length(2),
                Constraint::Length(4),
                Constraint::Percentage(19),
                Constraint::Percentage(29),
                Constraint::Percentage(25),
                Constraint::Length(6),
                Constraint::Length(8),
            ],
        )
    }
}

fn header_rects(area: Rect, compact: bool) -> Vec<(PlaylistColumn, Rect)> {
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    if inner.width == 0 {
        return Vec::new();
    }
    let constraints = if compact {
        vec![
            Constraint::Length(2),
            Constraint::Length(4),
            Constraint::Percentage(28),
            Constraint::Percentage(52),
            Constraint::Length(8),
        ]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Length(4),
            Constraint::Percentage(19),
            Constraint::Percentage(29),
            Constraint::Percentage(25),
            Constraint::Length(6),
            Constraint::Length(8),
        ]
    };
    let columns: Vec<Option<PlaylistColumn>> = if compact {
        vec![
            None,
            Some(PlaylistColumn::Number),
            Some(PlaylistColumn::Artist),
            Some(PlaylistColumn::Title),
            Some(PlaylistColumn::Time),
        ]
    } else {
        vec![
            None,
            Some(PlaylistColumn::Number),
            Some(PlaylistColumn::Artist),
            Some(PlaylistColumn::Title),
            Some(PlaylistColumn::Album),
            Some(PlaylistColumn::Date),
            Some(PlaylistColumn::Time),
        ]
    };
    // Layout::horizontal needs a known array; split manually by percentages-ish.
    let widths = layout_widths(inner.width, &constraints);
    let mut x = inner.x;
    let mut rects = Vec::new();
    for (column, width) in columns.into_iter().zip(widths) {
        if let Some(column) = column {
            rects.push((column, Rect::new(x, inner.y, width, 1)));
        }
        x = x.saturating_add(width);
    }
    rects
}

fn layout_widths(total: u16, constraints: &[Constraint]) -> Vec<u16> {
    let mut fixed = 0u16;
    let mut percent = 0u16;
    for constraint in constraints {
        match constraint {
            Constraint::Length(width) => fixed = fixed.saturating_add(*width),
            Constraint::Percentage(value) => percent = percent.saturating_add(*value),
            _ => {}
        }
    }
    let leftover = total.saturating_sub(fixed);
    constraints
        .iter()
        .map(|constraint| match constraint {
            Constraint::Length(width) => *width,
            Constraint::Percentage(value) if percent > 0 => leftover * *value / percent,
            _ => 0,
        })
        .collect()
}

fn render_status(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let right = if let Some(track) = app
        .tracks
        .get(&app.audio_snapshot.track_id.unwrap_or_default())
    {
        format!(
            "{} | {} Hz | {} ch | {}",
            track.codec,
            track
                .sample_rate
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".into()),
            track
                .channels
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".into()),
            format_duration(track.duration)
        )
    } else {
        format!("{} items", app.active_playlist().items.len())
    };
    let mut right_spans = Vec::new();
    let mut right_text = String::new();
    if !app.queue.is_empty() {
        let queue = format!("Q:{}", app.queue.len());
        right_spans.push(Span::styled(
            queue.clone(),
            Style::default().fg(Color::Cyan),
        ));
        right_spans.push(Span::raw(" | "));
        right_text.push_str(&queue);
        right_text.push_str(" | ");
    }
    if app.stop_after_current {
        right_spans.push(Span::styled("SAC", Style::default().fg(Color::Yellow)));
        right_spans.push(Span::raw(" | "));
        right_text.push_str("SAC | ");
    }
    if let Some(rg) = app.replay_gain_status() {
        right_spans.push(Span::styled(rg.clone(), Style::default().fg(Color::Green)));
        right_spans.push(Span::raw(" | "));
        right_text.push_str(&rg);
        right_text.push_str(" | ");
    }
    right_spans.push(Span::raw(right.clone()));
    right_text.push_str(&right);
    let downloading = app.soulseek_downloads_active > 0;
    let (status, status_style) = if downloading {
        let mut status = app.soulseek_ui.status.clone();
        if app.soulseek_downloads_active > 1 {
            status.push_str(&format!(" · {} downloads", app.soulseek_downloads_active));
        }
        (status, Style::default().fg(Color::Cyan))
    } else if let Some((done, total)) = app.scan_progress {
        (
            format!("Scanning {done}/{total} — {}", app.status),
            Style::default().fg(Color::Yellow),
        )
    } else if let Some(error) = &app.audio_error {
        (
            format!("Audio unavailable: {error} — Playback > Retry Audio"),
            error_style(),
        )
    } else {
        (app.status.clone(), chrome_style())
    };
    let available = area
        .width
        .saturating_sub(right_text.chars().count() as u16 + 2)
        .saturating_sub(if downloading { 2 } else { 0 }) as usize;
    let mut status = status;
    if status.chars().count() > available {
        status = status
            .chars()
            .take(available.saturating_sub(1))
            .collect::<String>()
            + "…";
    }
    if downloading {
        frame.render_stateful_widget(
            Throbber::default()
                .label(status)
                .style(status_style)
                .throbber_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .throbber_set(ASCII)
                .use_type(WhichUse::Spin),
            area,
            &mut app.soulseek_throbber,
        );
    } else {
        frame.render_widget(Paragraph::new(status).style(status_style), area);
    }
    let right_area = Rect::new(
        area.right().saturating_sub(right_text.len() as u16 + 1),
        area.y,
        (right_text.len() as u16 + 1).min(area.width),
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(right_spans))
            .alignment(Alignment::Right)
            .style(chrome_style()),
        right_area,
    );
}

fn render_overlay(frame: &mut Frame<'_>, app: &App, regions: &mut UiRegions) {
    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => {
            let area = centered(frame.area(), 68, 24);
            regions.overlay = Some(area);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(
                    "Keyboard shortcuts\n\n  Ctrl+O       Add files        Ctrl+Shift+O  Add folder\n  Space        Play / pause     Enter         Play / add album\n  Ctrl+F       Filter list      Esc           Clear filter\n  Shift+Up/Dn  Extend select    Insert        Toggle mark\n  Alt+Up/Down  Move tracks      Click headers Sort playlist\n  Left/Right   Seek ±5 sec      Delete        Remove / dequeue\n  Ctrl+N       New playlist     F2            Rename playlist\n  Ctrl+W       Close playlist   Ctrl+Q        Exit\n\nEnter on an artist or album adds every matching track.\nRight-click or Shift+F10: play, queue, remove, properties.\nThe Queue tab plays next, before playlist order. Playback\norder includes Shuffle (albums). The Preferences tab (Ctrl+,) holds ReplayGain, album art,\nspectrum, and icons.\nScan ReplayGain from the context menu or Playback menu.\n\nSoulseek: Enter expands a user or folder, or downloads a file.\nFilters: Left/Right format, Enter free slots.\n\nStaccato 0.1.0 — foobar2000-inspired terminal player",
                )
                .block(Block::bordered().title(" Help "))
                .style(base_style())
                .wrap(Wrap { trim: false }),
                area,
            );
        }
        Overlay::Menu { menu, selected } => {
            let items = menu_actions(*menu);
            let width = items
                .iter()
                .map(|(label, _)| label.len())
                .max()
                .unwrap_or(10) as u16
                + 2;
            let x = regions
                .menu
                .get(*menu)
                .map_or(frame.area().x, |rect| rect.x);
            let area = Rect::new(
                x,
                frame.area().y + 1,
                width.min(frame.area().width),
                items.len() as u16 + 2,
            );
            regions.overlay = Some(area);
            frame.render_widget(Clear, area);
            let list = List::new(items.iter().map(|(label, _)| ListItem::new(*label)))
                .block(Block::bordered())
                .style(base_style())
                .highlight_style(selected_style());
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(list, area, &mut state);
        }
        Overlay::ContextMenu {
            selected,
            items,
            at,
        } => {
            let width = items
                .iter()
                .map(|(label, _)| label.chars().count())
                .max()
                .unwrap_or(10) as u16
                + 2;
            let height = items.len() as u16 + 2;
            let (x, y) = match *at {
                Some(point) => point,
                None => {
                    let row = app
                        .soulseek_ui
                        .selected
                        .saturating_sub(regions.playlist_offset);
                    (
                        regions.playlist.x.saturating_add(2),
                        regions
                            .playlist
                            .y
                            .saturating_add(2)
                            .saturating_add(row as u16),
                    )
                }
            };
            let area = place_popup(frame.area(), x, y, width, height);
            regions.overlay = Some(area);
            frame.render_widget(Clear, area);
            let list = List::new(items.iter().map(|(label, _)| ListItem::new(label.as_str())))
                .block(Block::bordered().title(" Actions "))
                .style(base_style())
                .highlight_style(selected_style());
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(list, area, &mut state);
        }
        Overlay::PathPicker(picker) => {
            let area = centered(
                frame.area(),
                frame.area().width.saturating_sub(10).min(86),
                frame.area().height.saturating_sub(6).min(28),
            );
            regions.overlay = Some(area);
            frame.render_widget(Clear, area);
            let inner = area.inner(Margin {
                horizontal: 1,
                vertical: 1,
            });
            let split = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(2),
            ])
            .split(inner);
            frame.render_widget(
                Paragraph::new(picker.directory.to_string_lossy())
                    .block(Block::default().borders(Borders::BOTTOM).title(
                        if picker.mode == PickerMode::Files {
                            " Add file "
                        } else {
                            " Add folder "
                        },
                    ))
                    .style(base_style()),
                split[0],
            );
            let items = picker.entries.iter().map(|entry| {
                let name = entry
                    .path
                    .file_name()
                    .unwrap_or(entry.path.as_os_str())
                    .to_string_lossy();
                ListItem::new(format!("{} {name}", if entry.is_dir { "▸" } else { " " }))
            });
            let mut state = ListState::default()
                .with_selected((!picker.entries.is_empty()).then_some(picker.selected));
            frame.render_stateful_widget(
                List::new(items).highlight_style(selected_style()),
                split[1],
                &mut state,
            );
            regions.overlay_offset = state.offset();
            let help = if picker.mode == PickerMode::Folder {
                "Enter: open directory   A: add current folder   Esc: cancel"
            } else {
                "Enter: open directory or add file   Esc: cancel"
            };
            frame.render_widget(Paragraph::new(help).style(chrome_style()), split[2]);
            frame.render_widget(Block::bordered().style(base_style()), area);
        }
        Overlay::Connect { text, discovered } => {
            let area = centered(frame.area(), 56, 10.max(5 + discovered.len() as u16));
            regions.overlay = Some(area);
            frame.render_widget(Clear, area);
            let mut body = String::from("Host:port (Enter to connect, Esc to cancel)\n");
            if discovered.is_empty() {
                body.push_str("Searching the LAN…\n");
            } else {
                body.push_str("Found on the LAN:\n");
                for server in discovered {
                    body.push_str(&format!("  {}  {}\n", server.name, server.address));
                }
                body.push_str("Leave the box empty to use the first result.\n");
            }
            body.push_str(&format!("\n{text}█"));
            frame.render_widget(
                Paragraph::new(body)
                    .block(Block::bordered().title(" Connect to server "))
                    .style(base_style()),
                area,
            );
        }
        Overlay::Pair { text } => {
            let area = centered(frame.area(), 48, 6);
            regions.overlay = Some(area);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(format!(
                    "Enter the 6-digit code from the server.\n\n{text}█\n\nEnter: pair    Esc: cancel"
                ))
                .block(Block::bordered().title(" Pair "))
                .style(base_style()),
                area,
            );
        }
        Overlay::Properties { title, body } => {
            let area = centered(frame.area(), 72, 16);
            regions.overlay = Some(area);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(body.as_str())
                    .block(Block::bordered().title(title.as_str()))
                    .style(base_style())
                    .wrap(Wrap { trim: false }),
                area,
            );
        }
        Overlay::Rename { text, .. } => {
            let area = centered(frame.area(), 48, 5);
            regions.overlay = Some(area);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(format!("{text}█\n\nEnter: save    Esc: cancel"))
                    .block(Block::bordered().title(" Rename playlist "))
                    .style(base_style()),
                area,
            );
        }
    }
}

fn place_popup(outer: Rect, x: u16, y: u16, width: u16, height: u16) -> Rect {
    let width = width.min(outer.width);
    let height = height.min(outer.height);
    let x = x.min(outer.right().saturating_sub(width)).max(outer.x);
    let y = y.min(outer.bottom().saturating_sub(height)).max(outer.y);
    Rect::new(x, y, width, height)
}

fn centered(outer: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(outer.width);
    let height = height.min(outer.height);
    Rect::new(
        outer.x + (outer.width - width) / 2,
        outer.y + (outer.height - height) / 2,
        width,
        height,
    )
}

fn focused_block(title: impl Into<Line<'static>>, focused: bool) -> Block<'static> {
    let title: Line<'static> = title.into();
    let title = if focused {
        title.style(accent_style())
    } else {
        title
    };
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if focused {
            accent_style()
        } else {
            Style::default()
        })
        .style(base_style())
}

fn accent() -> Color {
    Color::Blue
}

fn accent_style() -> Style {
    Style::default().fg(accent()).add_modifier(Modifier::BOLD)
}

fn base_style() -> Style {
    Style::default()
}

fn chrome_style() -> Style {
    Style::default()
}

fn selected_style() -> Style {
    Style::default()
        .fg(accent())
        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
}

fn marked_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

fn button_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

fn unavailable_style() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn error_style() -> Style {
    Style::default().fg(Color::Red)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn centered_rect_stays_inside_parent() {
        let outer = Rect::new(10, 5, 20, 10);
        assert_eq!(centered(outer, 100, 100), outer);
        assert_eq!(centered(outer, 10, 4), Rect::new(15, 8, 10, 4));
    }

    #[test]
    fn context_menu_stays_inside_the_frame() {
        let outer = Rect::new(0, 0, 20, 10);
        assert_eq!(place_popup(outer, 18, 8, 8, 5), Rect::new(12, 5, 8, 5));
    }

    #[test]
    fn theme_uses_terminal_defaults_and_attributes() {
        assert_eq!(base_style(), Style::default());
        assert_eq!(chrome_style(), Style::default());
        assert_eq!(accent(), Color::Blue);
        assert_eq!(
            selected_style(),
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        );
        assert_eq!(
            button_style(),
            Style::default().add_modifier(Modifier::REVERSED)
        );
        assert_eq!(
            unavailable_style(),
            Style::default().add_modifier(Modifier::DIM)
        );
        assert_eq!(error_style(), Style::default().fg(Color::Red));
    }

    fn is_terminal_palette(color: Color) -> bool {
        matches!(
            color,
            Color::Reset
                | Color::Black
                | Color::Red
                | Color::Green
                | Color::Yellow
                | Color::Blue
                | Color::Magenta
                | Color::Cyan
                | Color::Gray
                | Color::DarkGray
                | Color::White
        )
    }

    #[test]
    fn default_ui_renders_all_primary_panels() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("ui.db"), true)?;
        let backend = TestBackend::new(110, 30);
        let mut terminal = Terminal::new(backend)?;
        let mut regions = UiRegions::default();
        terminal.draw(|frame| {
            regions = draw(frame, &mut app, IconSet::default());
        })?;
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for expected in [
            "File",
            "Playback",
            "Album List",
            "Album Art",
            "Playlists",
            "Queue",
            "Soulseek",
            "Preferences",
            "Playlist View",
            "Equalizer",
            "Ready",
            "⏮",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?} from rendered UI"
            );
        }
        assert_eq!(regions.previous.y, regions.menu[0].y + 1);
        assert_eq!(regions.previous.x, 0);
        assert_eq!(regions.previous.width, 5);
        assert_eq!(regions.next.right(), 20);
        assert_eq!(regions.seek.y, regions.previous.y);
        assert_eq!(regions.order.y, regions.previous.y);
        assert_eq!(regions.volume.y, regions.previous.y);
        assert!(regions.previous.right() <= regions.seek.x);
        assert!(regions.seek.right() <= regions.order.x);
        assert!(regions.order.right() <= regions.volume.x);
        assert_eq!(regions.album.y, regions.seek.y + 1);
        assert_eq!(regions.equalizer.x, regions.playlist.x);
        assert_eq!(regions.equalizer.y, regions.playlist.bottom());
        assert_eq!(regions.equalizer.width, regions.playlist.width);
        assert!(regions.equalizer.height >= 6);
        assert!(rendered.contains("Order:"));
        assert!(rendered.contains("Vol:"));
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .all(|cell| { is_terminal_palette(cell.fg) && is_terminal_palette(cell.bg) }),
            "UI should use the terminal palette, not hardcoded RGB"
        );
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.fg == Color::Blue),
            "the focused panel should use the terminal blue"
        );
        Ok(())
    }

    #[test]
    fn playlist_tab_hitboxes_cover_the_rendered_labels() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("tab-hitboxes.db"), true)?;
        let backend = TestBackend::new(110, 30);
        let mut terminal = Terminal::new(backend)?;
        let mut regions = UiRegions::default();
        terminal.draw(|frame| {
            regions = draw(frame, &mut app, IconSet::default());
        })?;

        assert_eq!(regions.tabs.len(), 4);
        for (region, label) in
            regions
                .tabs
                .iter()
                .zip(["Default", "Queue", "Soulseek", "Preferences"])
        {
            assert_eq!(region.width, label.len() as u16 + 2);
            let rendered: String = (region.x + 1..region.x + 1 + label.len() as u16)
                .map(|x| terminal.backend().buffer()[(x, region.y)].symbol())
                .collect();
            assert_eq!(rendered, label);
        }
        Ok(())
    }

    #[test]
    fn soulseek_download_status_uses_the_ascii_throbber() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("download-status.db"), true)?;
        app.soulseek_downloads_active = 2;
        app.soulseek_ui.status = "track.flac  50%  100 KB/s".into();
        app.soulseek_throbber.calc_next();
        let backend = TestBackend::new(110, 30);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| {
            draw(frame, &mut app, IconSet::default());
        })?;
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("/ track.flac  50%  100 KB/s · 2 downloads"));
        Ok(())
    }

    #[test]
    fn toolbar_fits_transport_seek_and_status_at_minimum_width() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("toolbar.db"), true)?;
        let backend = TestBackend::new(MIN_WIDTH, MIN_HEIGHT);
        let mut terminal = Terminal::new(backend)?;
        let mut regions = UiRegions::default();
        terminal.draw(|frame| {
            regions = draw(frame, &mut app, IconSet::default());
        })?;

        assert_eq!(regions.previous.y, 1);
        assert_eq!(regions.seek.y, 1);
        assert_eq!(regions.volume.y, 1);
        assert!(regions.previous.right() <= regions.seek.x);
        assert!(regions.seek.width >= 4);
        if regions.order.width > 0 {
            assert!(regions.seek.right() <= regions.order.x);
            assert!(regions.order.right() <= regions.volume.x);
        } else {
            assert!(regions.seek.right() <= regions.volume.x);
        }
        assert_eq!(regions.album.y, 2);
        Ok(())
    }

    #[test]
    fn undersized_terminal_shows_resize_message() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("small.db"), true)?;
        let backend = TestBackend::new(50, 12);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| {
            draw(frame, &mut app, IconSet::default());
        })?;
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("needs at least"));
        Ok(())
    }

    #[test]
    fn soulseek_tab_renders_search_and_cascaded_tree() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("soulseek.db"), true)?;
        app.soulseek_open = true;
        app.soulseek_ui = crate::soulseek::SoulseekUi::ready();
        app.soulseek_ui.query = "radiohead".into();
        app.soulseek_ui.set_hits(vec![crate::soulseek::SoulseekHit {
            username: "alice".into(),
            name: r"album\track.flac".into(),
            size: 1_000_000,
            slots: 1,
            speed: 2_000_000,
            bitrate: Some(320),
        }]);
        app.focus = Focus::SoulseekQuery;
        let backend = TestBackend::new(110, 30);
        let mut terminal = Terminal::new(backend)?;
        let mut regions = UiRegions::default();
        terminal.draw(|frame| {
            regions = draw(frame, &mut app, IconSet::default());
        })?;
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for expected in [
            "Soulseek",
            "Search:",
            "Filters",
            "FLAC",
            "radiohead",
            "alice",
            "album",
            "track.flac",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?} from Soulseek tab"
            );
        }
        assert!(regions.soulseek_query.width > 0);
        assert!(regions.soulseek_filter.y > regions.soulseek_query.y);
        assert!(regions.playlist.y > regions.soulseek_filter.y);
        assert_eq!(regions.soulseek_formats.len(), SoulseekFormat::ALL.len());
        assert_eq!(regions.equalizer, Rect::default());
        Ok(())
    }

    #[test]
    fn list_scroll_offset_survives_between_frames() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("scroll-offset.db"), true)?;
        app.soulseek_open = true;
        app.soulseek_ui = crate::soulseek::SoulseekUi::ready();
        app.soulseek_ui.set_hits(
            (0..12)
                .map(|index| crate::soulseek::SoulseekHit {
                    username: format!("user-{index}"),
                    name: format!(r"album-{index}\track-{index}.flac"),
                    size: 1_000_000,
                    slots: 1,
                    speed: 2_000_000,
                    bitrate: Some(320),
                })
                .collect(),
        );
        app.soulseek_ui.selected = 20;

        let backend = TestBackend::new(110, MIN_HEIGHT);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| {
            draw(frame, &mut app, IconSet::default());
        })?;
        let scrolled_offset = app.soulseek_scroll_offset;
        assert!(scrolled_offset > 0);

        app.soulseek_ui.selected -= 1;
        terminal.draw(|frame| {
            draw(frame, &mut app, IconSet::default());
        })?;
        assert_eq!(app.soulseek_scroll_offset, scrolled_offset);
        Ok(())
    }

    #[test]
    fn equalizer_sits_under_the_playlist_view() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("eq.db"), true)?;
        let backend = TestBackend::new(110, 30);
        let mut terminal = Terminal::new(backend)?;
        let mut regions = UiRegions::default();
        terminal.draw(|frame| {
            regions = draw(frame, &mut app, IconSet::default());
        })?;
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("Equalizer"));
        assert_eq!(regions.equalizer.y, regions.playlist.bottom());
        assert!(regions.equalizer.y > regions.playlist.y);
        assert!(regions.playlist.height >= 5);
        Ok(())
    }
}

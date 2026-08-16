#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingId {
    OutputDevice,
    ReplayGainMode,
    ReplayGainPreamp,
    ReplayGainClip,
    CursorFollow,
    AlbumArt,
    Spectrum,
    NerdFont,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingRow {
    Header(&'static str),
    Item(SettingId),
}

pub const ROWS: [SettingRow; 11] = [
    SettingRow::Header("Playback"),
    SettingRow::Item(SettingId::OutputDevice),
    SettingRow::Item(SettingId::ReplayGainMode),
    SettingRow::Item(SettingId::ReplayGainPreamp),
    SettingRow::Item(SettingId::ReplayGainClip),
    SettingRow::Item(SettingId::CursorFollow),
    SettingRow::Header("Display"),
    SettingRow::Item(SettingId::AlbumArt),
    SettingRow::Item(SettingId::Spectrum),
    SettingRow::Item(SettingId::NerdFont),
    SettingRow::Header("Enter or Left/Right changes a value."),
];

impl SettingId {
    pub fn label(self) -> &'static str {
        match self {
            Self::OutputDevice => "Output device",
            Self::ReplayGainMode => "ReplayGain",
            Self::ReplayGainPreamp => "ReplayGain preamp",
            Self::ReplayGainClip => "Prevent clipping",
            Self::CursorFollow => "Cursor follows playback",
            Self::AlbumArt => "Album art",
            Self::Spectrum => "Spectrum",
            Self::NerdFont => "Nerd Font icons",
        }
    }
}

impl SettingRow {
    pub fn is_item(self) -> bool {
        matches!(self, Self::Item(_))
    }

    pub fn id(self) -> Option<SettingId> {
        match self {
            Self::Item(id) => Some(id),
            Self::Header(_) => None,
        }
    }
}

pub fn first_item() -> usize {
    ROWS.iter().position(|row| row.is_item()).unwrap_or(0)
}

pub fn step(from: usize, delta: i32) -> usize {
    if ROWS.is_empty() {
        return 0;
    }
    let len = ROWS.len() as i32;
    let mut at = from as i32;
    for _ in 0..ROWS.len() {
        at = (at + delta).rem_euclid(len);
        if ROWS[at as usize].is_item() {
            return at as usize;
        }
    }
    from
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_skips_section_headers() {
        assert_eq!(
            ROWS[first_item()],
            SettingRow::Item(SettingId::OutputDevice)
        );
        let after_device = step(first_item(), 1);
        assert_eq!(
            ROWS[after_device],
            SettingRow::Item(SettingId::ReplayGainMode)
        );
        let wrap = step(ROWS.len() - 1, 1);
        assert!(ROWS[wrap].is_item());
    }
}

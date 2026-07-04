use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;

use crate::config::PrivacyConfig;

pub const DEFAULT_PATTERNS_FILE: &str = "~/.config/herdr/privacy-patterns.txt";
pub const DEFAULT_REPLACEMENT: &str = "█";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacyModeState {
    pub enabled: bool,
    pub patterns: Vec<String>,
    pub replacement: String,
}

impl Default for PrivacyModeState {
    fn default() -> Self {
        Self {
            enabled: false,
            patterns: Vec::new(),
            replacement: DEFAULT_REPLACEMENT.to_string(),
        }
    }
}

impl PrivacyModeState {
    pub fn from_config(config: &PrivacyConfig) -> Self {
        let mut patterns = Vec::new();
        patterns.extend(config.patterns.iter().cloned());
        patterns.extend(load_patterns_file(&config.patterns_file));
        patterns = normalize_patterns(patterns);

        let replacement = replacement_symbol(&config.replacement);

        Self {
            enabled: config.enabled,
            patterns,
            replacement,
        }
    }

    pub fn should_redact(&self) -> bool {
        self.enabled && !self.patterns.is_empty()
    }

    pub fn replacement_symbol(&self) -> &str {
        if self.replacement.trim().is_empty() {
            DEFAULT_REPLACEMENT
        } else {
            self.replacement.as_str()
        }
    }
}

fn replacement_symbol(value: &str) -> String {
    value
        .chars()
        .find(|ch| !ch.is_control() && !ch.is_whitespace())
        .map(|ch| ch.to_string())
        .unwrap_or_else(|| DEFAULT_REPLACEMENT.to_string())
}

fn normalize_patterns(patterns: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = patterns
        .into_iter()
        .filter_map(|pattern| {
            let trimmed = pattern.trim();
            let key = normalized_match_text(trimmed).0;
            if key.chars().count() < 2 {
                return None;
            }
            if !seen.insert(key) {
                return None;
            }
            Some(trimmed.to_string())
        })
        .collect::<Vec<_>>();

    normalized.sort_by_key(|pattern| std::cmp::Reverse(pattern.chars().count()));
    normalized
}

fn load_patterns_file(path: &str) -> Vec<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let path = expand_tilde(trimmed);
    if !path.exists() {
        return Vec::new();
    }

    let Ok(content) = std::fs::read_to_string(&path) else {
        tracing::warn!(path = %path.display(), "privacy patterns file could not be read");
        return Vec::new();
    };

    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect()
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(path)
}

pub fn redact_buffer(buffer: &mut Buffer, state: &PrivacyModeState) {
    if !state.should_redact() {
        return;
    }

    let area = buffer.area;
    let replacement = state.replacement_symbol().to_string();
    for y in area.y..area.y.saturating_add(area.height) {
        let mut row = String::new();
        let mut cell_ranges = Vec::with_capacity(area.width as usize);
        for x in area.x..area.x.saturating_add(area.width) {
            let start = row.len();
            row.push_str(buffer[(x, y)].symbol());
            let end = row.len();
            cell_ranges.push((start, end, x));
        }

        let redacted_cells = matched_cells(&row, &cell_ranges, &state.patterns);
        if redacted_cells.is_empty() {
            continue;
        }
        for x in redacted_cells {
            let cell = &mut buffer[(x, y)];
            cell.set_symbol(&replacement);
            cell.skip = false;
            cell.set_style(cell.style().add_modifier(ratatui::style::Modifier::BOLD));
        }
    }
}

fn matched_cells(row: &str, cell_ranges: &[(usize, usize, u16)], patterns: &[String]) -> Vec<u16> {
    if row.is_empty() || patterns.is_empty() {
        return Vec::new();
    }

    let (haystack, haystack_to_row_byte) = normalized_match_text(row);
    let mut redacted = HashSet::new();
    for pattern in patterns {
        let needle = normalized_match_text(pattern).0;
        if needle.is_empty() {
            continue;
        }
        let mut search_from = 0;
        while let Some(offset) = haystack[search_from..].find(&needle) {
            let normalized_start = search_from + offset;
            let normalized_end = normalized_start + needle.len();
            let start = haystack_to_row_byte[normalized_start];
            let end = haystack_to_row_byte[normalized_end];
            for (cell_start, cell_end, x) in cell_ranges {
                if ranges_overlap(start, end, *cell_start, *cell_end) {
                    redacted.insert(*x);
                }
            }
            search_from = normalized_end;
            if search_from >= haystack.len() {
                break;
            }
        }
    }

    let mut cells = redacted.into_iter().collect::<Vec<_>>();
    cells.sort_unstable();
    cells
}

fn normalized_match_text(value: &str) -> (String, Vec<usize>) {
    let mut normalized = String::new();
    let mut normalized_to_original_byte = vec![0];

    for (idx, ch) in value.char_indices() {
        if is_optional_apostrophe(ch) {
            continue;
        }

        let original_end = idx + ch.len_utf8();
        let canonical = match ch {
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2033}' => '"',
            '\u{00A0}' if ch.is_whitespace() => ' ',
            _ if ch.is_whitespace() => ' ',
            _ => ch,
        };

        for lower in canonical.to_lowercase() {
            normalized.push(lower);
            for _ in 0..lower.len_utf8() {
                normalized_to_original_byte.push(original_end);
            }
        }
    }

    (normalized, normalized_to_original_byte)
}

fn is_optional_apostrophe(ch: char) -> bool {
    matches!(
        ch,
        '\'' | '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{2032}' | '\u{02BC}'
    )
}

fn ranges_overlap(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    a_start < b_end && b_start < a_end
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn buffer_text(buffer: &Buffer) -> String {
        let area = buffer.area;
        let mut out = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn privacy_redaction_replaces_matching_client_name_cells() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 24, 1));
        buffer.set_string(
            0,
            0,
            "Acme Movers project",
            ratatui::style::Style::default(),
        );
        let state = PrivacyModeState {
            enabled: true,
            patterns: vec!["Acme Movers".to_string()],
            replacement: DEFAULT_REPLACEMENT.to_string(),
        };

        redact_buffer(&mut buffer, &state);

        assert!(buffer_text(&buffer).starts_with("███████████ project"));
    }

    #[test]
    fn privacy_redaction_is_case_insensitive() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 18, 1));
        buffer.set_string(0, 0, "acme movers", ratatui::style::Style::default());
        let state = PrivacyModeState {
            enabled: true,
            patterns: vec!["ACME MOVERS".to_string()],
            replacement: "*".to_string(),
        };

        redact_buffer(&mut buffer, &state);

        assert!(buffer_text(&buffer).starts_with("***********"));
    }

    #[test]
    fn privacy_redaction_matches_apostrophe_variants() {
        let state = PrivacyModeState {
            enabled: true,
            patterns: vec!["Can't Stop Moving".to_string()],
            replacement: DEFAULT_REPLACEMENT.to_string(),
        };

        for visible_text in ["Can't Stop Moving", "Can’t Stop Moving", "Cant Stop Moving"] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 1));
            buffer.set_string(
                0,
                0,
                format!("{visible_text} quote"),
                ratatui::style::Style::default(),
            );

            redact_buffer(&mut buffer, &state);

            let redacted = buffer_text(&buffer);
            assert!(
                !redacted.contains(visible_text),
                "visible variant was not redacted: {visible_text:?}"
            );
            assert!(redacted.contains(" quote"));
        }
    }

    #[test]
    fn privacy_redaction_noops_when_disabled() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 18, 1));
        buffer.set_string(0, 0, "Acme Movers", ratatui::style::Style::default());
        let state = PrivacyModeState {
            enabled: false,
            patterns: vec!["Acme Movers".to_string()],
            replacement: DEFAULT_REPLACEMENT.to_string(),
        };

        redact_buffer(&mut buffer, &state);

        assert!(buffer_text(&buffer).starts_with("Acme Movers"));
    }

    #[test]
    fn privacy_config_normalizes_duplicates_and_short_values() {
        let patterns = normalize_patterns(vec![
            " Acme Movers ".to_string(),
            "acme movers".to_string(),
            "Can’t Stop Moving".to_string(),
            "Can't Stop Moving".to_string(),
            "x".to_string(),
            "Beta".to_string(),
        ]);

        assert_eq!(
            patterns,
            vec![
                "Can’t Stop Moving".to_string(),
                "Acme Movers".to_string(),
                "Beta".to_string()
            ]
        );
    }
}

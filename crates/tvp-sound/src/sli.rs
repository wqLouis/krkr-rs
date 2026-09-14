//! `<storagename>.sli` loop-information parser.
//!
//! KiriKiri BGM files ship a side-car `.sli` ("sound loop information")
//! entry next to the audio (`bgm/bgm01.ogg.sli`). The reference
//! [`tTVPWaveLoopManager::ReadInformation`] reads it at `Open` time and the
//! loop manager jumps from each link's `From` sample to its `To` sample, so
//! BGM plays an intro once and then loops the tail instead of repeating the
//! whole file.
//!
//! This module ports the parser (both the old `LoopStart=`/`LoopLength=`
//! format and the `#2.00` block format) and exposes the links/labels. The
//! game's 23 BGM `.sli` files are all the simple form
//! `Link { From=…; To=…; Smooth=False; Condition=no; … }`, so
//! [`SliInfo::active_link`] returns the first unconditional, non-empty
//! link; conditional links/flags and label events are parsed but not acted
//! on (documented limitation).
//!
//! Reference: `reference/cpp/core/sound/WaveLoopManager.cpp`
//! (`ReadInformation`, `ReadLinkInformation`, `ReadLabelInformation`,
//! `GetCondition`), `WaveLoopManager.h` (`tTVPWaveLoopLink`).

/// Number of conditional flags the reference tracks (reference
/// `TVP_WL_MAX_FLAGS`).
pub const TVP_WL_MAX_FLAGS: usize = 16;

/// Maximum flag value (reference `TVP_WL_MAX_FLAG_VALUE`).
pub const TVP_WL_MAX_FLAG_VALUE: i32 = 9999;

/// A loop-link condition (reference `tTVPWaveLoopLinkCondition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopCondition {
    /// `Condition=no` — always taken.
    None,
    /// `Condition=eq` — taken when flag `CondVar == RefValue`.
    Equal,
    /// `Condition=ne`.
    NotEqual,
    /// `Condition=gt`.
    Greater,
    /// `Condition=ge`.
    GreaterOrEqual,
    /// `Condition=lt`.
    Lesser,
    /// `Condition=le`.
    LesserOrEqual,
}

impl LoopLink {
    /// Whether this link's condition holds for the given flag values
    /// (reference `tTVPWaveLoopManager::GetNearestEvent` condition test).
    pub fn matches(&self, flags: &[i32]) -> bool {
        let value = if self.cond_var >= 0 {
            flags.get(self.cond_var as usize).copied().unwrap_or(0)
        } else {
            0
        };
        let value = i64::from(value);
        match self.condition {
            LoopCondition::None => true,
            LoopCondition::Equal => value == self.ref_value,
            LoopCondition::NotEqual => value != self.ref_value,
            LoopCondition::Greater => value > self.ref_value,
            LoopCondition::GreaterOrEqual => value >= self.ref_value,
            LoopCondition::Lesser => value < self.ref_value,
            LoopCondition::LesserOrEqual => value <= self.ref_value,
        }
    }
}

impl LoopCondition {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "no" | "" => Some(LoopCondition::None),
            "eq" => Some(LoopCondition::Equal),
            "ne" => Some(LoopCondition::NotEqual),
            "gt" => Some(LoopCondition::Greater),
            "ge" => Some(LoopCondition::GreaterOrEqual),
            "lt" => Some(LoopCondition::Lesser),
            "le" => Some(LoopCondition::LesserOrEqual),
            _ => None,
        }
    }
}

/// One `Link { … }` entry: playback jumps from sample `from` to sample `to`
/// once it reaches `from` (reference `tTVPWaveLoopLink`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopLink {
    /// Sample granule the loop jumps *from* (the loop end).
    pub from: u64,
    /// Sample granule the loop jumps *to* (the loop start).
    pub to: u64,
    /// Whether the reference would crossfade the seam (`Smooth=True`).
    pub smooth: bool,
    /// Flag condition guarding the link.
    pub condition: LoopCondition,
    /// Right-hand value compared against flag `cond_var`.
    pub ref_value: i64,
    /// Flag index the condition reads.
    pub cond_var: i64,
}

/// One `Label { … }` entry (a named sample position that fires `onLabel`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveLabel {
    /// Sample granule the label sits at.
    pub position: u64,
    /// Label name.
    pub name: String,
}

/// Parsed loop information for one audio entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SliInfo {
    /// All `Link` entries, in file order.
    pub links: Vec<LoopLink>,
    /// All `Label` entries, in file order.
    pub labels: Vec<WaveLabel>,
}

impl SliInfo {
    /// The first unconditional, non-degenerate loop link — used when no
    /// flag state is available. `None` when the file only has conditional
    /// links or no link at all.
    pub fn active_link(&self) -> Option<&LoopLink> {
        self.links
            .iter()
            .find(|l| l.condition == LoopCondition::None && l.from > l.to)
    }

    /// The first non-degenerate link whose condition holds for `flags`
    /// (reference `GetNearestEvent` picks the nearest matching link; the
    /// game's files have a single link, so file order is enough here).
    pub fn active_link_with_flags(&self, flags: &[i32]) -> Option<&LoopLink> {
        self.links
            .iter()
            .find(|l| l.from > l.to && l.matches(flags))
    }
}

/// Parse `.sli` bytes. A leading UTF-8 BOM is tolerated.
///
/// Returns an error only when the text cannot be recognised as either
/// supported format; an empty/whitespace file yields an empty [`SliInfo`].
pub fn parse(bytes: &[u8]) -> Result<SliInfo, String> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Ok(SliInfo::default());
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("#2.") {
        // A comment-only file is still valid (no links).
        return Ok(SliInfo::default());
    }
    if !trimmed.starts_with('#') {
        return parse_old(trimmed);
    }
    parse_v2(trimmed)
}

/// Old format: `LoopStart=<n>` / `LoopLength=<n>` (reference old branch).
fn parse_old(text: &str) -> Result<SliInfo, String> {
    let find = |key: &str| -> Option<&str> {
        text.find(key).map(|i| {
            let rest = &text[i + key.len()..];
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            &rest[..end]
        })
    };
    let length: u64 = find("LoopLength=")
        .ok_or_else(|| "sli: missing LoopLength".to_string())?
        .parse()
        .map_err(|_| "sli: invalid LoopLength".to_string())?;
    let start: u64 = find("LoopStart=")
        .ok_or_else(|| "sli: missing LoopStart".to_string())?
        .parse()
        .map_err(|_| "sli: invalid LoopStart".to_string())?;
    Ok(SliInfo {
        links: vec![LoopLink {
            from: start.saturating_add(length),
            to: start,
            smooth: false,
            condition: LoopCondition::None,
            ref_value: 0,
            cond_var: 0,
        }],
        labels: Vec::new(),
    })
}

/// v2.00 format: `Link { … }` / `Label { … }` blocks (reference v2 branch).
fn parse_v2(text: &str) -> Result<SliInfo, String> {
    let mut info = SliInfo::default();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '#' {
            // Comment to end of line.
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Word at i (Link / Label).
        let start = i;
        while i < bytes.len() && (bytes[i] as char).is_ascii_alphabetic() {
            i += 1;
        }
        let word = &text[start..i];
        if word.eq_ignore_ascii_case("Link") || word.eq_ignore_ascii_case("Label") {
            // Skip whitespace up to the block.
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'{' {
                return Err(format!("sli: expected '{{' after {word}"));
            }
            let body_start = i + 1;
            let Some(rel_end) = text[body_start..].find('}') else {
                return Err(format!("sli: unterminated {word} block"));
            };
            let body = &text[body_start..body_start + rel_end];
            i = body_start + rel_end + 1;
            if word.eq_ignore_ascii_case("Link") {
                info.links.push(parse_link(body)?);
            } else {
                info.labels.push(parse_label(body)?);
            }
        } else {
            return Err(format!("sli: unexpected token '{word}'"));
        }
    }
    Ok(info)
}

/// Parse one `key=value;` block body into a [`LoopLink`].
fn parse_link(body: &str) -> Result<LoopLink, String> {
    let mut link = LoopLink {
        from: 0,
        to: 0,
        smooth: false,
        condition: LoopCondition::None,
        ref_value: 0,
        cond_var: 0,
    };
    for (key, value) in entities(body) {
        match key.to_ascii_lowercase().as_str() {
            "from" => link.from = value.trim().parse().unwrap_or(0),
            "to" => link.to = value.trim().parse().unwrap_or(0),
            "smooth" => link.smooth = value.trim().eq_ignore_ascii_case("true"),
            "condition" => {
                link.condition = LoopCondition::parse(&value)
                    .ok_or_else(|| format!("sli: unknown Condition '{value}'"))?
            }
            "refvalue" => link.ref_value = value.trim().parse().unwrap_or(0),
            "condvar" => link.cond_var = value.trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    Ok(link)
}

/// Parse one `key=value;` block body into a [`WaveLabel`].
fn parse_label(body: &str) -> Result<WaveLabel, String> {
    let mut label = WaveLabel {
        position: 0,
        name: String::new(),
    };
    for (key, value) in entities(body) {
        match key.to_ascii_lowercase().as_str() {
            "position" => label.position = value.trim().parse().unwrap_or(0),
            "name" => {
                let v = value.trim();
                label.name = v.trim_matches(|c| c == '\'' || c == '"').to_string();
            }
            _ => {}
        }
    }
    Ok(label)
}

/// Split a `key=value; key=value;` body into pairs (reference
/// `GetEntityToken`).
fn entities(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in body.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(eq) = part.find('=') {
            let key = part[..eq].trim().to_string();
            let value = part[eq + 1..].trim().to_string();
            if !key.is_empty() {
                out.push((key, value));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v2_link() {
        let text = b"#2.00\n# Sound Loop Information (utf-8)\n# Generated by WaveLoopManager.cpp\nLink { From=6821039;          To=1242931;           Smooth=False; Condition=no; RefValue=0;          CondVar=0;  }\n";
        let info = parse(text).expect("parse");
        assert_eq!(info.links.len(), 1);
        let link = info.active_link().expect("active link");
        assert_eq!(link.from, 6_821_039);
        assert_eq!(link.to, 1_242_931);
        assert!(!link.smooth);
        assert_eq!(link.condition, LoopCondition::None);
    }

    #[test]
    fn parses_old_format() {
        let info = parse(b"LoopStart=1000\nLoopLength=500\n").expect("parse");
        let link = info.active_link().unwrap();
        assert_eq!(link.from, 1500);
        assert_eq!(link.to, 1000);
    }

    #[test]
    fn parses_labels_and_conditions() {
        let text = b"#2.00\nLabel { Position=42; Name='chorus'; }\nLink { From=100; To=10; Smooth=True; Condition=eq; RefValue=1; CondVar=2; }\n";
        let info = parse(text).expect("parse");
        assert_eq!(info.labels.len(), 1);
        assert_eq!(info.labels[0].position, 42);
        assert_eq!(info.labels[0].name, "chorus");
        assert_eq!(info.links[0].condition, LoopCondition::Equal);
        assert!(info.links[0].smooth);
        // A conditional link is not the "active" one.
        assert!(info.active_link().is_none());
    }

    #[test]
    fn empty_and_comment_only_are_empty() {
        assert!(parse(b"").unwrap().links.is_empty());
        assert!(parse(b"# just a comment\n").unwrap().links.is_empty());
    }

    #[test]
    fn conditional_link_selection_uses_flags() {
        // Two links: a conditional one (flag 3 == 1) listed first and an
        // unconditional fallback. The conditional wins only when its flag
        // is set (file order, like the single-link game files).
        let text = b"#2.00\nLink { From=300; To=20; Condition=eq; RefValue=1; CondVar=3; }\nLink { From=200; To=10; Condition=no; }\n";
        let info = parse(text).expect("parse");
        let mut flags = [0i32; TVP_WL_MAX_FLAGS];
        let fallback = info.active_link_with_flags(&flags).expect("fallback");
        assert_eq!(fallback.to, 10);
        flags[3] = 1;
        let conditional = info.active_link_with_flags(&flags).expect("conditional");
        assert_eq!(conditional.to, 20);
    }
}

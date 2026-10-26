//! Reference `TextRenderBase` parser and line-breaking layout.
//!
//! This is a faithful port of `TextRenderBase::render` and the helpers it
//! calls (`readchar`, `read_integer`, `pushCharacter`, `pushGraphicalCharacter`,
//! `performLinebreak`, `flush`, `clear`, `resetFont`, `resetStyle`). The only
//! deliberate substitution is the metric source: the reference asks the active
//! `FontRasterizer` for `GetTextExtent`/`GetAscentHeight`, while this port asks
//! the L2 `engine_api::TextEngine` (see [`super::metrics`]).

use crate::engine_api::TextStyle;

use super::TextRenderInst;
use super::metrics::{self, TextRenderEngine};
use super::state::{CharacterInfo, TextRenderState};
use crate::CompatCtx;

/// `kTextRenderModeLeading`.
const MODE_LEADING: u32 = 0;
/// `kTextRenderModeNormal`.
const MODE_NORMAL: u32 = 1;
/// `kTextRenderModeFollowing`.
const MODE_FOLLOWING: u32 = 2;

impl TextRenderInst {
    /// Reference `TextRenderBase::render`.
    ///
    /// Parses the KAG text markup into positioned [`CharacterInfo`]s and
    /// returns `!overflow` (whether the layout stayed inside the render box).
    /// `auto_indent`, `diff`, `all` and `same` are accepted for signature
    /// compatibility; the reference ignores all of them (it never assigns the
    /// `m_autoIndent` field).
    pub(super) fn render(
        &mut self,
        ctx: &CompatCtx,
        engine: &mut TextRenderEngine,
        text: &str,
    ) -> Result<bool, String> {
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = 0usize;
        while i < len {
            let ch = chars[i];
            match ch {
                '%' => {
                    let format = read_char(&chars, &mut i)
                        .ok_or_else(|| parse_error("expected character, found EOF"))?;
                    self.handle_percent(&chars, &mut i, format)?;
                }
                '\\' => {
                    let escaped = read_char(&chars, &mut i)
                        .ok_or_else(|| parse_error("expected character, found EOF"))?;
                    match escaped {
                        'n' => {
                            self.flush(ctx, engine, false)?;
                            self.perform_linebreak(ctx, engine)?;
                        }
                        't' => self.push_character(ctx, engine, '\t')?,
                        'i' => self.indent = self.x,
                        'r' => self.indent = 0,
                        'w' => self.push_character(ctx, engine, ' ')?,
                        'k' | 'x' => {}
                        other => self.push_character(ctx, engine, other)?,
                    }
                }
                '[' => {
                    let _ruby = read_until(&chars, &mut i, ']')?;
                }
                '#' => self.parse_color(&chars, &mut i)?,
                '&' => {
                    let graph = read_until(&chars, &mut i, ';')?;
                    self.push_graphical_character(&graph);
                }
                '$' => {
                    let _variable = read_until(&chars, &mut i, ';')?;
                }
                _ => self.push_character(ctx, engine, ch)?,
            }
            i += 1;
        }

        self.state.render_over = self.y > self.box_height;
        Ok(!self.state.render_over)
    }

    /// The body of a `%` escape (reference `switch(ch)` after `readchar`).
    fn handle_percent(
        &mut self,
        chars: &[char],
        i: &mut usize,
        format: char,
    ) -> Result<(), String> {
        match format {
            't' => {
                // The reference builds a face name and discards it.
                let _ = read_until(chars, i, ';')?;
            }
            'f' => {
                let face = read_until(chars, i, ';')?;
                self.state.face = face;
            }
            // The reference guards these with `if(!readchar(...) && (ch ==
            // '0' || ch == '1'))`, which can never throw: on EOF `ch` still
            // holds the format character. `read_flag` reproduces the observed
            // behaviour (`flag = ch == '1'`, false at EOF).
            'b' => self.state.bold = read_flag(chars, i),
            'i' => self.state.italic = read_flag(chars, i),
            's' => self.state.shadow = read_flag(chars, i),
            'e' => self.state.edge = read_flag(chars, i),
            // The shipped build's parser selects the big and the small size
            // for these two escapes. The alignment escapes change nothing
            // there: the alignment is kept in the style and no code path
            // reads it back.
            'B' => self.state.font_size = self.default.big_font_size,
            'S' => self.state.font_size = self.default.small_font_size,
            'C' | 'R' | 'L' => {
                log::debug!("TextRenderBase.render: %{format} changes nothing");
            }
            'r' => self.reset_font(),
            'l' => {
                let _ = read_until(chars, i, ';')?;
            }
            'p' => self.state.pitch = read_integer(chars, i, 0)?,
            'd' | 'w' => {
                let _ = read_integer(chars, i, 0)?;
            }
            // The reference's `%D` branch tests `ch == '$'` while `ch` still
            // holds `'D'`, so the label branch is dead and it always reads an
            // integer. Reproduced verbatim.
            'D' => {
                let _ = read_integer(chars, i, 0)?;
            }
            '0'..='9' => {
                let start = (format as u8 - b'0') as i32;
                let value = read_integer(chars, i, start)?;
                self.state.font_size =
                    (i64::from(self.default.font_size) * i64::from(value) / 100) as i32;
            }
            other => {
                return Err(parse_error(&format!(
                    "expected any of 'fbiseBSrCRLpdwD0123456789', found '{other}'"
                )));
            }
        }
        Ok(())
    }

    /// Reference `pushCharacter`.
    fn push_character(
        &mut self,
        ctx: &CompatCtx,
        engine: &mut TextRenderEngine,
        ch: char,
    ) -> Result<(), String> {
        if (ch as u32) > 0xFFFF {
            return Err("unexpected character: surrogate pair".to_string());
        }

        let is_leading = self.options.leading.contains(ch);
        let is_following = self.options.following.contains(ch);
        let is_indent = self.options.begin.contains(ch);
        let is_indent_decr = self.options.end.contains(ch);

        let current = if is_leading {
            MODE_LEADING
        } else if is_following {
            MODE_FOLLOWING
        } else {
            MODE_NORMAL
        };

        if self.mode == MODE_FOLLOWING || self.mode != MODE_LEADING {
            self.flush(ctx, engine, false)?;
        }

        let font = metrics::resolve_face(ctx, engine, &self.state.face)?;
        let size = font_size(&self.state);
        let mut style = TextStyle::new(size);
        style.bold = self.state.bold;
        style.italic = self.state.italic;
        style.color = [255, 255, 255, 255];
        let advance = metrics::advance(engine, font, &style, ch)?;
        let ascent = metrics::ascent(engine, font, size)?;

        self.buffer.push(CharacterInfo {
            bold: self.state.bold,
            italic: self.state.italic,
            graph: false,
            vertical: false,
            face: self.state.face.clone(),
            x: 0,
            y: 0,
            cw: advance,
            size: ascent,
            color: self.state.ch_color,
            edge: if self.state.edge {
                self.state.edge_color
            } else {
                0
            },
            shadow: if self.state.shadow {
                self.state.shadow_color
            } else {
                0
            },
            text: ch.to_string(),
        });

        if self.auto_indent != 0 {
            // Pre-indent: the reference subtracts the first character's
            // advance when the requested auto-indent is negative.
            if self.is_beginning_of_line && self.auto_indent < 0 {
                self.x = self.x.wrapping_sub(advance);
            }
            if is_indent {
                self.indent = self.x.wrapping_add(advance);
            }
            if is_indent_decr && self.indent > 0 {
                self.flush(ctx, engine, false)?;
                self.indent = 0;
            }
        }

        self.mode = current;
        self.is_beginning_of_line = false;
        Ok(())
    }

    /// Reference `pushGraphicalCharacter`: an empty stub in the reference.
    fn push_graphical_character(&mut self, _graph: &str) {}

    /// Reference `performLinebreak`.
    fn perform_linebreak(
        &mut self,
        ctx: &CompatCtx,
        engine: &mut TextRenderEngine,
    ) -> Result<(), String> {
        let font = metrics::resolve_face(ctx, engine, &self.state.face)?;
        let size = font_size(&self.state);
        let ascent = metrics::ascent(engine, font, size)?;
        self.x = self.indent;
        self.is_beginning_of_line = true;
        self.y = self
            .y
            .wrapping_add(ascent)
            .wrapping_add(self.state.line_spacing);
        Ok(())
    }

    /// Reference `flush(force)`.
    pub(super) fn flush(
        &mut self,
        ctx: &CompatCtx,
        engine: &mut TextRenderEngine,
        force: bool,
    ) -> Result<(), String> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let mut x = self.x;
        let mut index = 0usize;
        while index < self.buffer.len() {
            let advance = self.buffer[index].cw;
            let mut new_x = advance.wrapping_add(x).wrapping_add(self.state.pitch);
            if self.box_width < new_x {
                if force {
                    self.perform_linebreak(ctx, engine)?;
                    x = self.x;
                    new_x = advance.wrapping_add(x).wrapping_add(self.state.pitch);
                } else {
                    self.perform_linebreak(ctx, engine)?;
                    return self.flush(ctx, engine, true);
                }
            }
            self.buffer[index].x = x;
            self.buffer[index].y = self.y;
            x = new_x;
            index += 1;
        }

        self.x = x;
        for info in &self.buffer {
            self.state.render_text.push_str(&info.text);
        }
        self.characters.append(&mut self.buffer);
        Ok(())
    }

    /// Reference `clear`: reset the character list and style to the default.
    ///
    /// The reference deliberately leaves the pending `m_buffer` and `m_mode`
    /// untouched.
    pub(super) fn clear(&mut self) {
        self.characters.clear();
        self.state = self.default.clone();
        self.x = 0;
        self.y = 0;
        self.indent = 0;
        self.is_beginning_of_line = true;
    }

    /// Reference `resetFont`.
    pub(super) fn reset_font(&mut self) {
        self.state = self.default.clone();
    }

    /// Reference `resetStyle` (identical to [`Self::reset_font`]).
    pub(super) fn reset_style(&mut self) {
        self.state = self.default.clone();
    }

    /// Reference `#rrggbb;` colour escape.
    fn parse_color(&mut self, chars: &[char], i: &mut usize) -> Result<(), String> {
        let mut colour: u32 = 0;
        let mut is_null = true;
        loop {
            let c =
                read_char(chars, i).ok_or_else(|| parse_error("expected character, found EOF"))?;
            if c == ';' {
                break;
            }
            is_null = false;
            let digit = match c {
                '0'..='9' => c as u32 - '0' as u32,
                'A'..='F' => 0x0a + (c as u32 - 'A' as u32),
                'a'..='f' => 0x0a + (c as u32 - 'a' as u32),
                _ => {
                    return Err(parse_error(&format!(
                        "expected hexadecimal number, found '{c}'"
                    )));
                }
            };
            colour = (colour << 4) | digit;
        }
        self.state.ch_color = if is_null { 0x00ff_ffff } else { colour };
        Ok(())
    }
}

/// The reference `font.Height` handling: FreeType takes the absolute value and
/// never a zero em.
fn font_size(state: &TextRenderState) -> f32 {
    state.font_size.unsigned_abs().max(1) as f32
}

/// Reference `readchar`: advance the cursor and read the next character, or
/// `None` past the end.
fn read_char(chars: &[char], i: &mut usize) -> Option<char> {
    *i += 1;
    chars.get(*i).copied()
}

/// Reference `readchar` in the boolean-flag form used by `%b`/`%i`/`%s`/`%e`.
fn read_flag(chars: &[char], i: &mut usize) -> bool {
    matches!(read_char(chars, i), Some('1'))
}

/// Read characters up to (and consuming) `terminator`.
fn read_until(chars: &[char], i: &mut usize, terminator: char) -> Result<String, String> {
    let mut out = String::new();
    loop {
        let c = read_char(chars, i).ok_or_else(|| parse_error("expected character, found EOF"))?;
        if c == terminator {
            return Ok(out);
        }
        out.push(c);
    }
}

/// Reference `read_integer`: read a (possibly signed) integer terminated by
/// `;`, seeded with `value`.
fn read_integer(chars: &[char], i: &mut usize, mut value: i32) -> Result<i32, String> {
    let mut negative = false;
    loop {
        let c = read_char(chars, i)
            .ok_or_else(|| parse_error("expected either integer or ';', found EOF"))?;
        if c.is_ascii_digit() {
            value = value
                .wrapping_mul(10)
                .wrapping_add(i32::from(c as u8 - b'0'));
            continue;
        }
        if c == '-' {
            negative = !negative;
            continue;
        }
        if c == ';' {
            return Ok(if negative { -value } else { value });
        }
        return Err(parse_error(&format!(
            "expected either integer or ';', found '{c}'"
        )));
    }
}

/// The reference's thrown parse-error prefix.
fn parse_error(detail: &str) -> String {
    format!("TextRenderBase::render() failed to parse: {detail}")
}

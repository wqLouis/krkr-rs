//! Layout state for the `TextRenderBase` plugin.
//!
//! Ports the reference `TextRenderState`, `TextRenderOptions` and
//! `CharacterInfo` structures. `TextRenderState::deserialize` fills the
//! style from a TJS Dictionary (`setDefault`) and `CharacterInfo::serialize`
//! writes one (`getCharacters`).

use tjs::{DetachedValue, RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue, VAL_OBJECT, Value};

/// One parsed graphical character (reference `CharacterInfo`).
///
/// `edge`/`shadow` are `0` when absent (the reference uses `NULL` and writes
/// a void member in that case). `color` is a packed `0xRRGGBB` value.
#[derive(Clone, Debug)]
pub(crate) struct CharacterInfo {
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) graph: bool,
    pub(super) vertical: bool,
    pub(super) face: String,
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) cw: i32,
    pub(super) size: i32,
    pub(super) color: u32,
    pub(super) edge: u32,
    pub(super) shadow: u32,
    pub(super) text: String,
}

impl CharacterInfo {
    /// Reference `CharacterInfo::serialize`: a TJS Dictionary with the
    /// character's style, position, metrics and colours.
    pub(super) fn serialize(&self, host: &Tjs2Engine) -> Result<DetachedValue, String> {
        let dict = retained_object(host, "%[]")?;
        let id = dict.raw_id();
        host.set_member(id, "bold", &TjsValue::Integer(i64::from(self.bold)))?;
        host.set_member(id, "italic", &TjsValue::Integer(i64::from(self.italic)))?;
        host.set_member(id, "graph", &TjsValue::Integer(i64::from(self.graph)))?;
        host.set_member(id, "vertical", &TjsValue::Integer(i64::from(self.vertical)))?;
        host.set_member(id, "x", &TjsValue::Integer(i64::from(self.x)))?;
        host.set_member(id, "y", &TjsValue::Integer(i64::from(self.y)))?;
        host.set_member(id, "cw", &TjsValue::Integer(i64::from(self.cw)))?;
        host.set_member(id, "size", &TjsValue::Integer(i64::from(self.size)))?;
        host.set_member(id, "face", &TjsValue::String(self.face.clone()))?;
        host.set_member(id, "color", &TjsValue::Integer(i64::from(self.color)))?;
        host.set_member(id, "edge", &opt_color(self.edge))?;
        host.set_member(id, "shadow", &opt_color(self.shadow))?;
        host.set_member(id, "text", &TjsValue::String(self.text.clone()))?;
        Ok(dict)
    }
}

/// The reference `TextRenderState`: the mutable text style/state.
#[derive(Clone, Debug)]
pub(crate) struct TextRenderState {
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) face: String,
    pub(super) font_size: i32,
    /// The big size the `%B` escape selects.
    pub(super) big_font_size: i32,
    /// The small size the `%S` escape selects.
    pub(super) small_font_size: i32,
    pub(super) font_scale: f64,
    pub(super) ch_color: u32,
    pub(super) ruby_size: i32,
    pub(super) ruby_offset: i32,
    pub(super) shadow: bool,
    pub(super) shadow_color: u32,
    pub(super) edge: bool,
    pub(super) edge_color: u32,
    pub(super) line_spacing: i32,
    pub(super) pitch: i32,
    pub(super) line_size: i32,
    pub(super) render_over: bool,
    pub(super) render_delay: i32,
    pub(super) render_text: String,
}

impl Default for TextRenderState {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            face: "user".to_string(),
            font_size: 24,
            big_font_size: 48,
            small_font_size: 12,
            font_scale: 1.0,
            ch_color: 0x00ff_ffff,
            ruby_size: 10,
            ruby_offset: -2,
            shadow: true,
            shadow_color: 0x0000_0000,
            edge: false,
            edge_color: 0x0000_80ff,
            line_spacing: 6,
            pitch: 0,
            line_size: 0,
            render_over: false,
            render_delay: 1000,
            render_text: String::new(),
        }
    }
}

impl TextRenderState {
    /// Reference `TextRenderState::deserialize`: only the style fields are
    /// read; `renderOver`/`renderDelay`/`renderText` are deliberately left
    /// untouched (the reference's `deserialize` omits them).
    ///
    /// Every style member has two spellings: the lower-case one the shipped
    /// `textrender.dll` reads and the camel-case one the class properties
    /// use. Both are accepted, the lower-case one first. The members only the
    /// shipped build reads are taken under its spelling alone: `bigfontsize`,
    /// `smallfontsize`, `shadowdiff`, `align` and `valign`.
    pub(super) fn deserialize(&mut self, host: &Tjs2Engine, value: &Value) -> Result<(), String> {
        let Some(id) = object_id(host, value)? else {
            return Ok(());
        };
        if let Some(v) = style_member(host, id, &["bold"]) {
            self.bold = tjs_bool(&v);
        }
        if let Some(v) = style_member(host, id, &["italic"]) {
            self.italic = tjs_bool(&v);
        }
        let base_size = style_member(host, id, &["fontsize", "fontSize"]);
        if let Some(v) = &base_size {
            self.font_size = tjs_i64(v) as i32;
        }
        // The shipped build keeps the big and the small size beside the base
        // one and derives them from it when the dictionary does not name
        // them: the big size is twice the base size, the small size half of
        // it.
        match style_member(host, id, &["bigfontsize"]) {
            Some(v) => self.big_font_size = tjs_i64(&v) as i32,
            None if base_size.is_some() => {
                self.big_font_size = self.font_size.saturating_mul(2);
            }
            None => {}
        }
        match style_member(host, id, &["smallfontsize"]) {
            Some(v) => self.small_font_size = tjs_i64(&v) as i32,
            None if base_size.is_some() => {
                self.small_font_size = self.font_size / 2;
            }
            None => {}
        }
        if let Some(v) = style_member(host, id, &["fontScale"]) {
            self.font_scale = tjs_f64(&v);
        }
        if let Some(TjsValue::String(face)) = style_member(host, id, &["face"]) {
            self.face = face;
        }
        if let Some(v) = style_member(host, id, &["color", "chColor"]) {
            self.ch_color = tjs_i64(&v) as u32;
        }
        if let Some(v) = style_member(host, id, &["rubysize", "rubySize"]) {
            self.ruby_size = tjs_i64(&v) as i32;
        }
        if let Some(v) = style_member(host, id, &["rubyoffset", "rubyOffset"]) {
            self.ruby_offset = tjs_i64(&v) as i32;
        }
        if let Some(v) = style_member(host, id, &["shadow"]) {
            self.shadow = tjs_bool(&v);
        }
        if let Some(v) = style_member(host, id, &["shadowcolor", "shadowColor"]) {
            self.shadow_color = tjs_i64(&v) as u32;
        }
        if let Some(v) = style_member(host, id, &["edge"]) {
            self.edge = tjs_bool(&v);
        }
        if let Some(v) = style_member(host, id, &["edgecolor", "edgeColor"]) {
            self.edge_color = tjs_i64(&v) as u32;
        }
        if let Some(v) = style_member(host, id, &["linespacing", "lineSpacing"]) {
            self.line_spacing = tjs_i64(&v) as i32;
        }
        if let Some(v) = style_member(host, id, &["pitch"]) {
            self.pitch = tjs_i64(&v) as i32;
        }
        if let Some(v) = style_member(host, id, &["linesize", "lineSize"]) {
            self.line_size = tjs_i64(&v) as i32;
        }
        // The shipped build keeps the shadow difference and the alignment in
        // the style and no code path reads them back, so the port takes them
        // out of the dictionary and applies the same nil effect.
        let _inert = (
            style_member(host, id, &["shadowdiff"]),
            style_member(host, id, &["align"]),
            style_member(host, id, &["valign"]),
        );
        Ok(())
    }
}

/// The reference `TextRenderOptions`: the leading/following/indent character
/// sets used by the line-breaking heuristic.
///
/// The reference `deserialize` is a no-op, so `setOption` never changes these
/// values; the defaults below are the reference's compiled-in sets.
pub(crate) struct TextRenderOptions {
    pub(super) following: String,
    pub(super) leading: String,
    pub(super) begin: String,
    pub(super) end: String,
}

impl Default for TextRenderOptions {
    fn default() -> Self {
        Self {
            following: "%),:;]}????。，、．：；゛゜ヽヾゝゞ々’”）〕］｝〉》」』】°′″℃￠％‰　!.?=?????????????？！ーぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮヵヶ".to_string(),
            leading: "\\$([{?‘“（〔［｛〈《「『【￥＄￡".to_string(),
            begin: "「『（‘“〔［｛〈《".to_string(),
            end: "」』）’”〕］｝〉》".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// TJS Dictionary helpers
// ---------------------------------------------------------------------------

/// Evaluate a small object expression (`%[]`) and retain the resulting object.
pub(super) fn retained_object(
    host: &Tjs2Engine,
    expression: &str,
) -> Result<DetachedValue, String> {
    match host
        .eval_retained(expression, "TextRenderBase")
        .map_err(|error| error.to_string())?
    {
        RetainedValue::Object(object) => Ok(object),
        RetainedValue::Value(value) => Err(format!(
            "TextRenderBase: expression {expression:?} did not yield an object ({value:?})"
        )),
    }
}

/// Retain a `VAL_OBJECT` argument and return its id, or `None` when the value
/// is not an object (the reference's `AsObjectNoAddRef()` returning null).
fn object_id(host: &Tjs2Engine, value: &Value) -> Result<Option<Tjs2ValueId>, String> {
    if value.ty != VAL_OBJECT || value.object_handle().is_null() {
        return Ok(None);
    }
    let object = host
        .retain_object_arg(value)
        .map_err(|error| error.to_string())?;
    let id = object.raw_id();
    // The object is retained for the duration of the callback; forget the
    // RAII wrapper so the id stays valid while the caller reads its members.
    std::mem::forget(object);
    Ok(Some(id))
}

/// Read a Dictionary member, treating a missing member and TJS `void` alike
/// (the reference `getprop` macro).
fn member(host: &Tjs2Engine, id: Tjs2ValueId, name: &str) -> Option<TjsValue> {
    match host.member(id, name) {
        Ok(TjsValue::Void) | Err(_) => None,
        Ok(value) => Some(value),
    }
}

/// Read a style member under the first spelling the dictionary carries.
///
/// The style members have two spellings: the lower-case one the shipped
/// `textrender.dll` reads (`color`, `fontsize`, `linespacing`, ...) and the
/// camel-case one the class properties use (`chColor`, `fontSize`,
/// `lineSpacing`, ...). A dictionary that carries both takes the lower-case
/// value.
fn style_member(host: &Tjs2Engine, id: Tjs2ValueId, names: &[&str]) -> Option<TjsValue> {
    names.iter().find_map(|name| member(host, id, *name))
}

/// TJS `operator bool` for a member value.
fn tjs_bool(v: &TjsValue) -> bool {
    match v {
        TjsValue::Void => false,
        TjsValue::Integer(i) => *i != 0,
        TjsValue::Real(r) => *r != 0.0,
        TjsValue::String(s) => !s.is_empty(),
        TjsValue::Object | TjsValue::Retained(_) => true,
    }
}

/// TJS `AsInteger` for a member value.
fn tjs_i64(v: &TjsValue) -> i64 {
    match v {
        TjsValue::Integer(i) => *i,
        TjsValue::Real(r) => *r as i64,
        TjsValue::String(s) => crate::natives::system::tjs_string_to_integer(s),
        _ => 0,
    }
}

/// TJS `AsReal` for a member value.
fn tjs_f64(v: &TjsValue) -> f64 {
    match v {
        TjsValue::Integer(i) => *i as f64,
        TjsValue::Real(r) => *r,
        TjsValue::String(s) => crate::natives::system::tjs_string_to_real(s),
        _ => 0.0,
    }
}

/// The reference `setprop_opt_t`: an absent colour (0) is written as void.
fn opt_color(color: u32) -> TjsValue {
    if color == 0 {
        TjsValue::Void
    } else {
        TjsValue::Integer(i64::from(color))
    }
}

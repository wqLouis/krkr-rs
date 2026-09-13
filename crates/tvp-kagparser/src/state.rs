//! The KAGParser native instance state machine.
//!
//! This is a stateful *pull* wrapper around the `kag` crate's *push*
//! parser: [`KagParserState::load`] parses a scenario once into events,
//! and [`KagParserState::next_tag`] walks them one tag at a time, exactly
//! like the reference `tTJSNI_KAGParser::_GetNextTag`
//! (`reference/cpp/core/base/KAGParser.cpp`) pulls one tag per call.
//!
//! # Divergences from the reference (documented)
//!
//! * The reference emits one `ch` tag per **character** of text and an
//!   `r eol=true` tag at every line end (when `ignoreCR` is false); we
//!   keep that (the `kag` crate gives whole text runs, which we split
//!   char-by-char). Empty lines, which the `kag` crate drops, are
//!   recovered from the line numbers and emit `r eol=true` the same way.
//! * The owner-object event callbacks (`onScenarioLoad`, `onScenarioLoaded`,
//!   `onLabel`, `onScript`, `onJump`, `onCall`, `onReturn`, `onAfterReturn`)
//!   cannot be fired: the ABI carries no handle to the owning TJS object.
//!   Jumps/calls/returns therefore always process (no veto), labels are
//!   skipped silently, and `[iscript]` blocks are returned as ordinary
//!   tags.
//! * `if`/`elsif`/`ignore` conditions, `emb` expressions, `&entity` values
//!   and `cond` attributes are evaluated by calling back into the TJS VM
//!   through the [`Environ`] `eval` hook (globally, not in the owner's
//!   context).
//! * Macro *recording* reconstructs tag text from parsed events
//!   (`[name key=value ...]`, values re-quoted when needed) instead of
//!   copying raw source text; the recorded body re-parses to the same
//!   events, which is all the expansion path needs. The `*` macro-entity
//!   marker is lost (the parser consumes it) and `&`/`%` value prefixes
//!   survive in values.
//! * `%`-directive lines are skipped silently (the `kag` crate parses
//!   them as control lines; the reference would emit them as text).

use std::collections::{BTreeMap, BTreeSet};

use kag::{Bracket, Event};

use crate::kvp;

// Error messages mirroring the reference (KAGParser.cpp).
pub const MSG_NO_LINE: &str = "Readed scenario file %1 is empty.";
pub const MSG_SYNTAX: &str = "Syntax error.'[' match to ']', \" match to \", 'macro' march to 'endmacro'. \
     Notice space and newline.";
pub const MSG_LABEL_NOT_FOUND: &str = "Label %2 not found in scenario file %1.";
pub const MSG_CALL_STACK_UNDERFLOW: &str =
    "'return' is not matched to any 'call' ( 'return' is unexpected )";
pub const MSG_RETURN_LOST_SYNC: &str = "Lost return position due to the scenario file changed.";
pub const MSG_LABEL_IN_MACRO: &str = "Label in macro 'iscript' is illegal.";
pub const MSG_MALFORMED_SAVE: &str = "Malformed savedata, data may damaged.";

/// The result of evaluating a TJS expression through [`Environ::eval`].
#[derive(Debug, Clone, PartialEq)]
pub enum EvalResult {
    Void,
    Integer(i64),
    Real(f64),
    Str(String),
}

impl EvalResult {
    /// TJS `operator bool` semantics: void → false, numbers → non-zero,
    /// strings → non-empty.
    pub fn truthy(&self) -> bool {
        match self {
            EvalResult::Void => false,
            EvalResult::Integer(i) => *i != 0,
            EvalResult::Real(r) => *r != 0.0,
            EvalResult::Str(s) => !s.is_empty(),
        }
    }

    /// The reference's `if(Type() != tvtVoid) ValueVariant.ToString()`;
    /// `None` for void (the attribute is omitted from the tag).
    pub fn to_string_repr(&self) -> Option<String> {
        match self {
            EvalResult::Void => None,
            EvalResult::Integer(i) => Some(i.to_string()),
            EvalResult::Real(r) => Some(format_real(*r)),
            EvalResult::Str(s) => Some(s.clone()),
        }
    }
}

/// Format a real like `ttstr(double)` in the reference: integral values
/// print without a decimal point.
fn format_real(r: f64) -> String {
    if r.fract() == 0.0 && r.abs() < 1e15 {
        format!("{}", r as i64)
    } else {
        format!("{r}")
    }
}

/// The environment a parser walks in: expression evaluation (into the TJS
/// VM) and scenario loading (from storage). Both are injectable so the
/// state machine stays pure and unit-testable.
pub struct Environ<'a> {
    pub eval: &'a mut dyn FnMut(&str) -> Result<EvalResult, String>,
    pub load_storage: &'a mut dyn FnMut(&str) -> Result<String, String>,
}

/// One pending macro/emb expansion spliced ahead of the main event stream
/// (the analog of the reference's `LineBuffer`).
#[derive(Debug, Clone, PartialEq)]
struct Splice {
    /// Parsed events of the expansion (macro content) or a single Text
    /// event (emb result).
    events: Vec<Event>,
    /// Next event / char position within `events`.
    event_pos: usize,
    char_pos: usize,
    /// Whether the tag that started this splice was the last thing on its
    /// line in its own stream and was a `[...]` tag — an `r eol=true`
    /// fires when the outermost expansion is exhausted.
    line_r_after: bool,
}

/// One call-stack entry (the reference's `tCallStackData`).
#[derive(Debug, Clone, Default, PartialEq)]
struct CallStackEntry {
    storage: String,
    label: String,
    offset: usize,
    org_sig: String,
    char_index: usize,
    splice: Vec<Splice>,
    pending_rs: usize,
    macro_args_base: usize,
    macro_args_depth: usize,
    exclude_level: i32,
    if_level: i32,
    exclude_level_stack: Vec<i32>,
    if_level_executed_stack: Vec<bool>,
}

/// Special tags the parser executes itself when `processSpecialTags` is
/// on (the reference's `tSpecialTags`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Special {
    If,
    Ignore,
    Endif,
    EndIgnore,
    Else,
    Elsif,
    Emb,
    Macro,
    EndMacro,
    MacroPop,
    EraseMacro,
    Jump,
    Call,
    Return,
}

fn special_kind(name: &str) -> Option<Special> {
    Some(match name {
        "if" => Special::If,
        "ignore" => Special::Ignore,
        "endif" => Special::Endif,
        "endignore" => Special::EndIgnore,
        "else" => Special::Else,
        "elsif" => Special::Elsif,
        "emb" => Special::Emb,
        "macro" => Special::Macro,
        "endmacro" => Special::EndMacro,
        "macropop" => Special::MacroPop,
        "erasemacro" => Special::EraseMacro,
        "jump" => Special::Jump,
        "call" => Special::Call,
        "return" => Special::Return,
        _ => return None,
    })
}

/// A tag returned by [`KagParserState::next_tag`]: name + ordered params.
pub type TagOutput = (String, Vec<(String, String)>);

/// The KAGParser native instance state: the parsed scenario, the walk
/// position, and the macro/conditional/call machinery.
#[derive(Debug)]
pub struct KagParserState {
    // scenario
    events: Vec<Event>,
    labels: BTreeMap<String, usize>,
    raw_lines: Vec<String>,
    comment_or_label_lines: BTreeSet<usize>,
    storage_name: String,
    storage_short_name: String,

    // walk position
    event_index: usize,
    char_index: usize,
    cur_line: usize,
    pending_rs: usize,
    splice_stack: Vec<Splice>,

    // current label/page
    cur_label: String,
    cur_page: String,

    // macro machinery
    macros: BTreeMap<String, String>,
    macro_args: Vec<BTreeMap<String, String>>,
    macro_args_base: usize,
    recording_macro: bool,
    recording_macro_name: String,
    recording_macro_str: String,

    // conditional machinery
    exclude_level: i32,
    if_level: i32,
    exclude_level_stack: Vec<i32>,
    if_level_executed_stack: Vec<bool>,

    // call stack
    call_stack: Vec<CallStackEntry>,

    // flags
    interrupted: bool,
    ignore_cr: bool,
    process_special_tags: bool,
    debug_level: i32,
}

impl Default for KagParserState {
    /// The reference constructor defaults: special tags are processed, the
    /// debug level is `tkdlSimple`, and `ExcludeLevel` starts at -1 (not
    /// excluded).
    fn default() -> Self {
        KagParserState {
            events: Vec::new(),
            labels: BTreeMap::new(),
            raw_lines: Vec::new(),
            comment_or_label_lines: BTreeSet::new(),
            storage_name: String::new(),
            storage_short_name: String::new(),
            event_index: 0,
            char_index: 0,
            cur_line: 0,
            pending_rs: 0,
            splice_stack: Vec::new(),
            cur_label: String::new(),
            cur_page: String::new(),
            macros: BTreeMap::new(),
            macro_args: Vec::new(),
            macro_args_base: 0,
            recording_macro: false,
            recording_macro_name: String::new(),
            recording_macro_str: String::new(),
            exclude_level: -1,
            if_level: 0,
            exclude_level_stack: Vec::new(),
            if_level_executed_stack: Vec::new(),
            call_stack: Vec::new(),
            interrupted: false,
            ignore_cr: false,
            process_special_tags: true,
            debug_level: 1,
        }
    }
}

/// The source line of an event (all variants carry one).
fn line_of(e: &Event) -> usize {
    match e {
        Event::Label { line, .. }
        | Event::Text { line, .. }
        | Event::Tag { line, .. }
        | Event::Directive { line, .. }
        | Event::Comment { line, .. } => *line,
    }
}

// ---------------------------------------------------------------------------
// loading
// ---------------------------------------------------------------------------

impl KagParserState {
    /// Load a scenario from already-decoded text. The reference throws on
    /// an empty scenario and on syntax errors; we report the same.
    pub fn load_scenario_text(&mut self, name: &str, text: &str) -> Result<(), String> {
        let scenario = kag::parse(text).map_err(|e| match e {
            kag::Error::EmptyScenario => MSG_NO_LINE.replace("%1", name),
            e => format!("{MSG_SYNTAX} ({e})"),
        })?;
        self.events = scenario.events;
        self.labels = scenario.labels;
        self.raw_lines = split_lines(text);
        self.comment_or_label_lines = self
            .events
            .iter()
            .filter(|e| matches!(e, Event::Comment { .. } | Event::Label { .. }))
            .map(line_of)
            .collect();
        self.storage_name = name.to_string();
        self.storage_short_name = extract_storage_name(name);
        self.rewind();
        self.pending_rs = self.initial_gap_lines();
        Ok(())
    }

    /// Load a scenario through the environment (reads + decodes storage).
    /// Loading the *same* storage again just rewinds, like the reference's
    /// scenario cache (`if(StorageName == name) Rewind()`).
    pub fn load_scenario(&mut self, name: &str, env: &mut Environ<'_>) -> Result<(), String> {
        if self.storage_name == name {
            self.rewind();
            self.pending_rs = self.initial_gap_lines();
            return Ok(());
        }
        let text = (env.load_storage)(name)?;
        self.load_scenario_text(name, &text)
    }

    /// Set the walk position to the start.
    fn rewind(&mut self) {
        self.event_index = 0;
        self.char_index = 0;
        self.pending_rs = 0;
        self.splice_stack.clear();
        self.cur_line = 0;
        self.cur_label.clear();
        self.cur_page.clear();
        self.break_condition_and_macro();
    }

    /// `r eol=true` emissions for empty lines before the first event.
    fn initial_gap_lines(&self) -> usize {
        let first_line = self
            .events
            .first()
            .map(line_of)
            .unwrap_or(self.raw_lines.len() + 1);
        (1..first_line)
            .filter(|l| !self.comment_or_label_lines.contains(l))
            .count()
    }

    /// Reset condition state and macro recording/args (reference
    /// `BreakConditionAndMacro`).
    fn break_condition_and_macro(&mut self) {
        self.recording_macro = false;
        self.recording_macro_name.clear();
        self.recording_macro_str.clear();
        self.exclude_level = -1;
        self.exclude_level_stack.clear();
        self.if_level_executed_stack.clear();
        self.if_level = 0;
        self.pop_macro_args_to(self.macro_args_base);
    }

    /// Clear the scenario and every per-scenario state (reference `Clear`
    /// + `ClearBuffer`); macros survive, like the reference.
    pub fn clear(&mut self) {
        self.events.clear();
        self.labels.clear();
        self.raw_lines.clear();
        self.comment_or_label_lines.clear();
        self.storage_name.clear();
        self.storage_short_name.clear();
        self.rewind();
        self.macro_args.clear();
        self.macro_args_base = 0;
        self.call_stack.clear();
    }

    // -------------------------------------------------------------------
    // position
    // -------------------------------------------------------------------

    /// `GoToLabel`: jump to a `*label` (the name must keep the leading
    /// `*`), throwing when the label does not exist.
    pub fn go_to_label(&mut self, name: &str) -> Result<(), String> {
        if name.is_empty() {
            return Ok(());
        }
        let idx = *self.labels.get(name).ok_or_else(|| {
            MSG_LABEL_NOT_FOUND
                .replace("%2", name)
                .replace("%1", &self.storage_name)
        })?;
        self.event_index = idx;
        self.char_index = 0;
        self.pending_rs = 0;
        self.splice_stack.clear();
        self.break_condition_and_macro();
        if let Event::Label {
            name,
            macro_name,
            line,
        } = &self.events[idx]
        {
            self.cur_label = name.clone();
            self.cur_page = macro_name.clone().unwrap_or_default();
            self.cur_line = *line;
        }
        Ok(())
    }

    /// `GoToStorageAndLabel`.
    pub fn go_to_storage_and_label(
        &mut self,
        storage: Option<&str>,
        label: Option<&str>,
        env: &mut Environ<'_>,
    ) -> Result<(), String> {
        let storage = storage.filter(|s| !s.is_empty());
        let label = label.filter(|s| !s.is_empty());
        if let Some(storage) = storage {
            self.load_scenario(storage, env)?;
        }
        if let Some(label) = label {
            self.go_to_label(label)?;
        }
        Ok(())
    }

    /// `CallLabel`: push the current position and jump.
    pub fn call_label(&mut self, name: &str, env: &mut Environ<'_>) -> Result<(), String> {
        self.push_call_stack();
        self.go_to_storage_and_label(None, Some(name), env)
    }

    // -------------------------------------------------------------------
    // the tag walk
    // -------------------------------------------------------------------

    /// Pull the next tag. `None` means end of scenario (the reference
    /// returns void). This is `_GetNextTag` ported onto parsed events.
    pub fn next_tag(&mut self, env: &mut Environ<'_>) -> Result<Option<TagOutput>, String> {
        loop {
            if self.interrupted {
                self.interrupted = false;
                return Ok(Some(("interrupt".to_string(), Vec::new())));
            }
            if self.pending_rs > 0 {
                self.pending_rs -= 1;
                // Line ends encountered while a macro is being recorded are
                // appended to the recorded body as `[r eol=true]` (the
                // reference appends them at parse time; we defer the
                // queue-vs-record decision to emission time so that a tag
                // that *starts* recording (e.g. `[macro]`) affects its own
                // line's trailing r).
                if self.recording_macro {
                    self.recording_macro_str.push_str("[r eol=true]");
                    continue;
                }
                return Ok(Some((
                    "r".to_string(),
                    vec![("eol".to_string(), "true".to_string())],
                )));
            }

            let (ev, from_splice) = self.current_event_cloned();
            let Some(ev) = ev else {
                if from_splice {
                    self.pop_splice();
                    continue;
                }
                return Ok(None);
            };

            match &ev {
                Event::Text { text, .. } => {
                    if self.recording_macro {
                        self.record_text(text);
                        self.advance_current();
                        continue;
                    }
                    let pos = self.current_char_pos();
                    let Some(c) = text.chars().nth(pos) else {
                        // Run already fully emitted (defensive).
                        self.advance_current();
                        continue;
                    };
                    self.bump_char_pos();
                    if pos + 1 == text.chars().count() {
                        self.advance_current();
                    }
                    return Ok(Some((
                        "ch".to_string(),
                        vec![("text".to_string(), c.to_string())],
                    )));
                }
                Event::Tag {
                    name,
                    params,
                    bracket,
                    line,
                } => {
                    if self.recording_macro {
                        if name == "endmacro" {
                            self.finish_recording();
                        } else {
                            if name == "macro" {
                                // Reference quirk: a nested [macro] clears
                                // the recording name.
                                self.recording_macro_name.clear();
                            }
                            self.record_tag(name, params);
                        }
                        self.advance_current();
                        continue;
                    }
                    self.advance_current();
                    if let Some(out) = self.process_tag(name, params, *bracket, *line, env)? {
                        return Ok(Some(out));
                    }
                }
                Event::Label {
                    name,
                    macro_name,
                    line,
                } => {
                    if self.recording_macro {
                        return Err(MSG_LABEL_IN_MACRO.to_string());
                    }
                    // Mirror reference SkipCommentOrLabel: advancing past
                    // a label updates the current label/page/line even
                    // though the label itself emits no tag.
                    let name = name.clone();
                    let macro_name = macro_name.clone();
                    let line = *line;
                    self.cur_label = name;
                    self.cur_page = macro_name.unwrap_or_default();
                    self.cur_line = line;
                    self.advance_current();
                }
                Event::Comment { .. } | Event::Directive { .. } => {
                    self.advance_current();
                }
            }
        }
    }

    /// The next event to process, cloned so the walk can mutate state
    /// while processing it. The bool says whether it came from a splice.
    fn current_event_cloned(&self) -> (Option<Event>, bool) {
        if let Some(top) = self.splice_stack.last() {
            (top.events.get(top.event_pos).cloned(), true)
        } else {
            (self.events.get(self.event_index).cloned(), false)
        }
    }

    fn current_char_pos(&self) -> usize {
        self.splice_stack
            .last()
            .map_or(self.char_index, |top| top.char_pos)
    }

    fn bump_char_pos(&mut self) {
        if let Some(top) = self.splice_stack.last_mut() {
            top.char_pos += 1;
        } else {
            self.char_index += 1;
        }
    }

    /// Advance past the current event (splice or main stream).
    fn advance_current(&mut self) {
        if let Some(top) = self.splice_stack.last_mut() {
            top.event_pos += 1;
            top.char_pos = 0;
        } else {
            self.advance_past_main_event();
        }
    }

    /// Advance the main-stream event cursor, emitting the end-of-line
    /// `r eol=true` for the finished line and for empty lines in any gap
    /// before the next event (reference behavior for line ends).
    fn advance_past_main_event(&mut self) {
        let Some(ev) = self.events.get(self.event_index).cloned() else {
            return;
        };
        let line = line_of(&ev);
        self.cur_line = line;

        let line_ends = match &ev {
            Event::Tag {
                bracket: Bracket::At,
                ..
            } => false,
            Event::Tag { .. } | Event::Text { .. } => self
                .events
                .get(self.event_index + 1)
                .map(|n| line_of(n) != line)
                .unwrap_or(true),
            _ => false,
        };
        if line_ends {
            self.end_of_line_r();
        }

        let next_line = self
            .events
            .get(self.event_index + 1)
            .map(line_of)
            .unwrap_or(self.raw_lines.len() + 1);
        for g in (line + 1)..next_line {
            if !self.comment_or_label_lines.contains(&g) {
                self.end_of_line_r();
            }
        }
        self.event_index += 1;
        // The char cursor only applies to the text run being emitted; the
        // next event starts at its own position.
        self.char_index = 0;
    }

    /// One `r eol=true`: queued as output (the recording-vs-output decision
    /// is made at emission time in [`Self::next_tag`]).
    fn end_of_line_r(&mut self) {
        if !self.ignore_cr {
            self.pending_rs += 1;
        }
    }

    /// Pop the top splice; when the stack becomes empty, an `r eol=true`
    /// queued by the expansion's tag fires.
    fn pop_splice(&mut self) {
        if let Some(splice) = self.splice_stack.pop()
            && self.splice_stack.is_empty()
            && splice.line_r_after
        {
            self.end_of_line_r();
        }
    }

    // -------------------------------------------------------------------
    // tag processing
    // -------------------------------------------------------------------

    /// Resolve, classify and execute one tag. Returns the tag to emit, or
    /// `None` when the tag was consumed (special tags, recording, macro
    /// expansion, excluded regions).
    fn process_tag(
        &mut self,
        name: &str,
        params: &[(String, String)],
        bracket: Bracket,
        line: usize,
        env: &mut Environ<'_>,
    ) -> Result<Option<TagOutput>, String> {
        // --- attribute resolution + cond -------------------------------
        let mut condition = true;
        let mut resolved: Vec<(String, String)> = Vec::new();
        let process_attrs = (!self.recording_macro && self.exclude_level == -1) || name == "elsif";
        for (k, v) in params {
            let mut value: Option<String> = Some(v.clone());
            if process_attrs {
                if let Some(rest) = v.strip_prefix('&') {
                    value = (env.eval)(rest)?.to_string_repr();
                } else if let Some(rest) = v.strip_prefix('%') {
                    value = self.resolve_macro_arg(rest);
                }
            }
            if k == "cond" {
                if process_attrs {
                    let cond_str = value.unwrap_or_default();
                    condition = (env.eval)(&cond_str)?.truthy();
                }
                continue;
            }
            if let Some(value) = value {
                resolved.push((k.clone(), value));
            }
        }

        let special = if self.process_special_tags {
            special_kind(name)
        } else {
            None
        };
        let gated = condition && self.exclude_level == -1;

        // --- endmacro / recording / macro lookup ------------------------
        if gated && special == Some(Special::EndMacro) {
            if !self.recording_macro {
                return Err(MSG_SYNTAX.to_string());
            }
            self.finish_recording();
            return Ok(None);
        }
        let mut is_macro = false;
        let mut macro_content = String::new();
        if gated && let Some(content) = self.macros.get(name) {
            is_macro = true;
            macro_content = content.clone();
        }

        // --- ordinary tags ----------------------------------------------
        if special.is_none() && !is_macro {
            return if gated {
                Ok(Some((name.to_string(), resolved)))
            } else {
                Ok(None)
            };
        }

        // --- if / ignore ------------------------------------------------
        if special == Some(Special::If) || special == Some(Special::Ignore) {
            self.if_level += 1;
            self.if_level_executed_stack.push(false);
            self.exclude_level_stack.push(self.exclude_level);
            if self.exclude_level == -1 {
                let exp = self.exp_attr(&resolved)?;
                let mut cond = (env.eval)(&exp)?.truthy();
                if special == Some(Special::Ignore) {
                    cond = !cond;
                }
                if let Some(last) = self.if_level_executed_stack.last_mut() {
                    *last = cond;
                }
                if !cond {
                    self.exclude_level = self.if_level;
                }
            }
            return Ok(None);
        }

        // --- elsif ------------------------------------------------------
        if special == Some(Special::Elsif) {
            if let Some(last) = self.if_level_executed_stack.last() {
                if *last {
                    self.exclude_level = self.if_level;
                } else if self.if_level == self.exclude_level {
                    let exp = self.exp_attr(&resolved)?;
                    let cond = (env.eval)(&exp)?.truthy();
                    if cond {
                        if let Some(last) = self.if_level_executed_stack.last_mut() {
                            *last = true;
                        }
                        self.exclude_level = -1;
                    }
                }
            }
            return Ok(None);
        }

        // --- else -------------------------------------------------------
        if special == Some(Special::Else) {
            if let Some(last) = self.if_level_executed_stack.last() {
                if *last {
                    self.exclude_level = self.if_level;
                } else if self.if_level == self.exclude_level {
                    if let Some(last) = self.if_level_executed_stack.last_mut() {
                        *last = true;
                    }
                    self.exclude_level = -1;
                }
            }
            return Ok(None);
        }

        // --- endif / endignore ------------------------------------------
        if special == Some(Special::Endif) || special == Some(Special::EndIgnore) {
            if let Some(v) = self.exclude_level_stack.pop() {
                self.exclude_level = v;
            }
            self.if_level_executed_stack.pop();
            self.if_level -= 1;
            if self.if_level < 0 {
                self.if_level = 0;
            }
            return Ok(None);
        }

        // --- gated specials: emb/expand, jump, call, return, macro ops ---
        if gated {
            if special == Some(Special::Emb) || is_macro {
                let line_r_after = !self.ignore_cr
                    && bracket == Bracket::Square
                    && self.last_on_line_after_current(line);
                if is_macro {
                    let events = parse_macro_content(&macro_content)?;
                    self.push_macro_args(&resolved);
                    self.splice_stack.push(Splice {
                        events,
                        event_pos: 0,
                        char_pos: 0,
                        line_r_after,
                    });
                } else {
                    let exp = self.exp_attr(&resolved)?;
                    let text = (env.eval)(&exp)?.to_string_repr().unwrap_or_default();
                    self.splice_stack.push(Splice {
                        events: vec![Event::Text { text, line }],
                        event_pos: 0,
                        char_pos: 0,
                        line_r_after,
                    });
                }
                return Ok(None);
            }
            if special == Some(Special::Jump) {
                let storage = self.attr(&resolved, "storage");
                let target = self.attr(&resolved, "target");
                self.go_to_storage_and_label(storage, target, env)?;
                return Ok(None);
            }
            if special == Some(Special::Call) {
                self.push_call_stack();
                let storage = self.attr(&resolved, "storage");
                let target = self.attr(&resolved, "target");
                self.go_to_storage_and_label(storage, target, env)?;
                return Ok(None);
            }
            if special == Some(Special::Return) {
                let storage = self.attr(&resolved, "storage");
                let target = self.attr(&resolved, "target");
                self.pop_call_stack(storage, target, env)?;
                return Ok(None);
            }
            if special == Some(Special::Macro) {
                let name = self
                    .attr(&resolved, "name")
                    .unwrap_or_default()
                    .to_lowercase();
                if name.is_empty() {
                    return Err(MSG_SYNTAX.to_string());
                }
                self.recording_macro = true;
                self.recording_macro_name = name;
                self.recording_macro_str.clear();
                return Ok(None);
            }
            if special == Some(Special::MacroPop) {
                self.pop_macro_args()?;
                return Ok(None);
            }
            if special == Some(Special::EraseMacro) {
                let name = self.attr(&resolved, "name").unwrap_or_default();
                if self.macros.remove(name).is_none() {
                    return Err(format!("Unknown macro \"{name}\""));
                }
                return Ok(None);
            }
        }
        Ok(None)
    }

    fn exp_attr(&self, resolved: &[(String, String)]) -> Result<String, String> {
        let exp = self.attr(resolved, "exp").unwrap_or_default();
        if exp.is_empty() {
            return Err(MSG_SYNTAX.to_string());
        }
        Ok(exp.to_string())
    }

    fn attr<'a>(&self, resolved: &'a [(String, String)], key: &str) -> Option<&'a str> {
        resolved
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Whether the current event (the one after the tag being processed)
    /// is on a different line, i.e. the tag closed its line.
    fn last_on_line_after_current(&self, line: usize) -> bool {
        if let Some(top) = self.splice_stack.last() {
            top.events
                .get(top.event_pos)
                .map(|e| line_of(e) != line)
                .unwrap_or(true)
        } else {
            self.events
                .get(self.event_index)
                .map(|e| line_of(e) != line)
                .unwrap_or(true)
        }
    }

    // -------------------------------------------------------------------
    // macro machinery
    // -------------------------------------------------------------------

    /// `%name` / `%name|default` attribute value resolution against the
    /// top macro-args dictionary (reference attribute processing). `None`
    /// means "no value" (the attribute is omitted).
    fn resolve_macro_arg(&self, rest: &str) -> Option<String> {
        if self.macro_args.is_empty() {
            // No macro arguments: the value is kept as-is (without `%`).
            return Some(rest.to_string());
        }
        let top = self.macro_args.last().expect("checked non-empty");
        match rest.split_once('|') {
            Some((name, default)) => top.get(name).cloned().or_else(|| Some(default.to_string())),
            None => top.get(rest).cloned(),
        }
    }

    fn push_macro_args(&mut self, params: &[(String, String)]) {
        let mut map = BTreeMap::new();
        for (k, v) in params {
            map.insert(k.clone(), v.clone());
        }
        self.macro_args.push(map);
    }

    fn pop_macro_args(&mut self) -> Result<(), String> {
        if self.macro_args.is_empty() {
            return Err(MSG_SYNTAX.to_string());
        }
        self.macro_args.pop();
        Ok(())
    }

    /// `popMacroArgs` native method: pop one macro-argument level.
    pub fn pop_macro_args_public(&mut self) -> Result<(), String> {
        self.pop_macro_args()
    }

    fn pop_macro_args_to(&mut self, base: usize) {
        self.macro_args.truncate(base);
    }

    fn finish_recording(&mut self) {
        self.recording_macro = false;
        self.recording_macro_str.push_str("[macropop]");
        let name = std::mem::take(&mut self.recording_macro_name);
        let content = std::mem::take(&mut self.recording_macro_str);
        self.macros.insert(name, content);
    }

    fn record_text(&mut self, text: &str) {
        for c in text.chars() {
            if c == '[' {
                self.recording_macro_str.push_str("[[");
            } else {
                self.recording_macro_str.push(c);
            }
        }
    }

    fn record_tag(&mut self, name: &str, params: &[(String, String)]) {
        self.recording_macro_str
            .push_str(&reconstruct_tag(name, params));
    }

    // -------------------------------------------------------------------
    // call stack
    // -------------------------------------------------------------------

    fn push_call_stack(&mut self) {
        let limit = self.event_index.min(self.events.len());
        let mut label_idx = None;
        for (i, ev) in self.events[..limit].iter().enumerate().rev() {
            if let Event::Label { .. } = ev {
                label_idx = Some(i);
                break;
            }
        }
        let (label, offset) = match label_idx {
            Some(i) => {
                let name = match &self.events[i] {
                    Event::Label { name, .. } => name.clone(),
                    _ => unreachable!("label_idx points at a label event"),
                };
                (name, self.event_index - i)
            }
            None => (String::new(), self.event_index),
        };
        let entry = CallStackEntry {
            storage: self.storage_name.clone(),
            label,
            offset,
            org_sig: self.current_line_str(),
            char_index: self.char_index,
            splice: self.splice_stack.clone(),
            pending_rs: self.pending_rs,
            macro_args_base: self.macro_args_base,
            macro_args_depth: self.macro_args.len(),
            exclude_level: self.exclude_level,
            if_level: self.if_level,
            exclude_level_stack: self.exclude_level_stack.clone(),
            if_level_executed_stack: self.if_level_executed_stack.clone(),
        };
        self.call_stack.push(entry);
        self.macro_args_base = self.macro_args.len();
    }

    fn pop_call_stack(
        &mut self,
        storage: Option<&str>,
        label: Option<&str>,
        env: &mut Environ<'_>,
    ) -> Result<(), String> {
        let data = self
            .call_stack
            .last()
            .cloned()
            .ok_or_else(|| MSG_CALL_STACK_UNDERFLOW.to_string())?;

        self.pop_macro_args_to(data.macro_args_depth);

        let to_storage = storage.is_some_and(|s| !s.is_empty());
        let to_label = label.is_some_and(|s| !s.is_empty());
        if to_storage || to_label {
            if to_storage {
                self.load_scenario(storage.expect("checked"), env)?;
            }
            if to_label {
                self.go_to_label(label.expect("checked"))?;
            }
        } else {
            // Return to the previous position: label + offset, with a
            // lost-sync check against the saved line text.
            if data.storage != self.storage_name {
                self.load_scenario(&data.storage, env)?;
            }
            let label_idx = if data.label.is_empty() {
                0
            } else {
                self.labels
                    .get(&data.label)
                    .copied()
                    .ok_or_else(|| MSG_RETURN_LOST_SYNC.to_string())?
            };
            let target = label_idx + data.offset;
            if target > self.events.len() {
                return Err(MSG_RETURN_LOST_SYNC.to_string());
            }
            let restored_sig = if target < self.events.len() {
                self.raw_lines
                    .get(line_of(&self.events[target]) - 1)
                    .cloned()
                    .unwrap_or_default()
            } else {
                String::new()
            };
            if restored_sig != data.org_sig {
                return Err(MSG_RETURN_LOST_SYNC.to_string());
            }
            self.event_index = target;
            self.char_index = data.char_index;
            self.splice_stack = data.splice;
            self.pending_rs = data.pending_rs;
        }

        self.macro_args_base = data.macro_args_base;
        self.exclude_level = data.exclude_level;
        self.if_level = data.if_level;
        self.exclude_level_stack = data.exclude_level_stack;
        self.if_level_executed_stack = data.if_level_executed_stack;
        self.call_stack.pop();
        Ok(())
    }

    /// `ClearCallStack`: also pops macro args down to base 0.
    pub fn clear_call_stack(&mut self) {
        self.call_stack.clear();
        self.macro_args_base = 0;
        self.pop_macro_args_to(0);
    }

    /// The raw text of the line the walk currently points at ("" past the
    /// end), used for the lost-sync check.
    fn current_line_str(&self) -> String {
        self.events
            .get(self.event_index)
            .and_then(|e| self.raw_lines.get(line_of(e) - 1))
            .cloned()
            .unwrap_or_default()
    }

    // -------------------------------------------------------------------
    // store / restore
    // -------------------------------------------------------------------

    /// Serialize the full parser state (the reference `Store`, string
    /// encoded because objects cannot cross the C ABI).
    pub fn store(&self) -> String {
        let mut v: Vec<(String, String)> = vec![("version".into(), "1".into())];
        v.push(("storageName".into(), self.storage_name.clone()));
        v.push(("storageShortName".into(), self.storage_short_name.clone()));
        v.push(("curLabel".into(), self.cur_label.clone()));
        v.push(("curPage".into(), self.cur_page.clone()));
        v.push(("eventIndex".into(), self.event_index.to_string()));
        v.push(("charIndex".into(), self.char_index.to_string()));
        v.push(("curLine".into(), self.cur_line.to_string()));
        v.push(("pendingRs".into(), self.pending_rs.to_string()));
        v.push(("ignoreCR".into(), bool_str(self.ignore_cr)));
        v.push((
            "processSpecialTags".into(),
            bool_str(self.process_special_tags),
        ));
        v.push(("interrupted".into(), bool_str(self.interrupted)));
        v.push(("debugLevel".into(), self.debug_level.to_string()));
        v.push(("excludeLevel".into(), self.exclude_level.to_string()));
        v.push(("ifLevel".into(), self.if_level.to_string()));
        v.push((
            "excludeLevelStack".into(),
            encode_int_stack(&self.exclude_level_stack),
        ));
        v.push((
            "ifLevelExecutedStack".into(),
            encode_bool_stack(&self.if_level_executed_stack),
        ));
        v.push(("macroArgStackBase".into(), self.macro_args_base.to_string()));
        v.push((
            "macroArgStackDepth".into(),
            self.macro_args.len().to_string(),
        ));
        for (i, args) in self.macro_args.iter().enumerate() {
            for (j, (k, val)) in args.iter().enumerate() {
                v.push((format!("macroArgs.{i}.{j}.name"), k.clone()));
                v.push((format!("macroArgs.{i}.{j}.value"), val.clone()));
            }
        }
        v.push(("macrosCount".into(), self.macros.len().to_string()));
        for (name, content) in &self.macros {
            v.push((
                format!("macros.{}", kvp::escape_field(name)),
                content.clone(),
            ));
        }
        v.push(("callStackCount".into(), self.call_stack.len().to_string()));
        for (i, e) in self.call_stack.iter().enumerate() {
            v.push((format!("callStack.{i}.storage"), e.storage.clone()));
            v.push((format!("callStack.{i}.label"), e.label.clone()));
            v.push((format!("callStack.{i}.offset"), e.offset.to_string()));
            v.push((format!("callStack.{i}.orgSig"), e.org_sig.clone()));
            v.push((format!("callStack.{i}.charIndex"), e.char_index.to_string()));
            v.push((format!("callStack.{i}.pendingRs"), e.pending_rs.to_string()));
            v.push((
                format!("callStack.{i}.macroArgStackBase"),
                e.macro_args_base.to_string(),
            ));
            v.push((
                format!("callStack.{i}.macroArgStackDepth"),
                e.macro_args_depth.to_string(),
            ));
            v.push((
                format!("callStack.{i}.excludeLevel"),
                e.exclude_level.to_string(),
            ));
            v.push((format!("callStack.{i}.ifLevel"), e.if_level.to_string()));
            v.push((
                format!("callStack.{i}.excludeLevelStack"),
                encode_int_stack(&e.exclude_level_stack),
            ));
            v.push((
                format!("callStack.{i}.ifLevelExecutedStack"),
                encode_bool_stack(&e.if_level_executed_stack),
            ));
            v.push((
                format!("callStack.{i}.spliceCount"),
                e.splice.len().to_string(),
            ));
            for (j, s) in e.splice.iter().enumerate() {
                v.push((format!("callStack.{i}.splice.{j}"), encode_splice(s)));
            }
        }
        v.push(("spliceCount".into(), self.splice_stack.len().to_string()));
        for (i, s) in self.splice_stack.iter().enumerate() {
            v.push((format!("splice.{i}"), encode_splice(s)));
        }
        kvp::encode_dict(&v)
    }

    /// Restore state from a [`Self::store`] string (the reference
    /// `Restore`). Re-loads the scenario from storage like the reference.
    pub fn restore(&mut self, s: &str, env: &mut Environ<'_>) -> Result<(), String> {
        let entries = kvp::decode_dict(s);
        let get = |k: &str| -> Option<&str> {
            entries
                .iter()
                .find(|(kk, _)| kk == k)
                .map(|(_, v)| v.as_str())
        };
        let get_int = |k: &str| get(k).and_then(|v| v.parse::<i64>().ok());
        let get_bool = |k: &str| get(k).is_some_and(|v| v == "1");

        let storage = get("storageName").unwrap_or("").to_string();
        let cur_label = get("curLabel").unwrap_or("").to_string();
        let cur_page = get("curPage").unwrap_or("").to_string();
        let storage_short = get("storageShortName").unwrap_or("").to_string();

        self.clear();
        if !storage.is_empty() {
            self.load_scenario(&storage, env)?;
        }
        if !cur_label.is_empty() {
            self.go_to_label(&cur_label)?;
        }

        // flags
        self.ignore_cr = get_bool("ignoreCR");
        self.process_special_tags = get_bool("processSpecialTags");
        self.interrupted = get_bool("interrupted");
        if let Some(d) = get_int("debugLevel") {
            self.debug_level = d as i32;
        }
        if let Some(i) = get_int("eventIndex") {
            self.event_index = i as usize;
        }
        if let Some(c) = get_int("charIndex") {
            self.char_index = c as usize;
        }
        if let Some(l) = get_int("curLine") {
            self.cur_line = l as usize;
        }
        if let Some(p) = get_int("pendingRs") {
            self.pending_rs = p as usize;
        }
        self.cur_page = cur_page;
        self.storage_short_name = storage_short;

        // condition state
        if let Some(v) = get_int("excludeLevel") {
            self.exclude_level = v as i32;
        }
        if let Some(v) = get_int("ifLevel") {
            self.if_level = v as i32;
        }
        if let Some(v) = get("excludeLevelStack") {
            self.exclude_level_stack = decode_int_stack(v);
        }
        if let Some(v) = get("ifLevelExecutedStack") {
            self.if_level_executed_stack = decode_bool_stack(v);
        }

        // macro args (name/value pairs are written consecutively, so a
        // single ordered pass pairs them up)
        self.macro_args.clear();
        if let Some(v) = get_int("macroArgStackDepth") {
            let depth = v as usize;
            for i in 0..depth {
                let prefix = format!("macroArgs.{i}.");
                let mut map = BTreeMap::new();
                let mut pending_name: Option<String> = None;
                for (k, val) in &entries {
                    if let Some(rest) = k.strip_prefix(&prefix) {
                        if let Some(name) = rest.strip_suffix(".name") {
                            pending_name = Some(kvp::unescape_field(name));
                        } else if rest.strip_suffix(".value").is_some()
                            && let Some(name) = pending_name.take()
                        {
                            map.insert(name, val.clone());
                        }
                    }
                }
                self.macro_args.push(map);
            }
        }
        if let Some(v) = get_int("macroArgStackBase") {
            self.macro_args_base = v as usize;
        }

        // macros
        self.macros.clear();
        if let Some(n) = get_int("macrosCount") {
            let n = n as usize;
            for (k, v) in &entries {
                if let Some(name) = k.strip_prefix("macros.") {
                    self.macros.insert(kvp::unescape_field(name), v.clone());
                }
            }
            debug_assert_eq!(self.macros.len(), n);
        }

        // call stack
        self.call_stack.clear();
        if let Some(n) = get_int("callStackCount") {
            let n = n as usize;
            for i in 0..n {
                let p = format!("callStack.{i}.");
                let mut e = CallStackEntry {
                    storage: get(&format!("{p}storage")).unwrap_or("").to_string(),
                    label: get(&format!("{p}label")).unwrap_or("").to_string(),
                    offset: get_int(&format!("{p}offset")).unwrap_or(0) as usize,
                    org_sig: get(&format!("{p}orgSig")).unwrap_or("").to_string(),
                    char_index: get_int(&format!("{p}charIndex")).unwrap_or(0) as usize,
                    pending_rs: get_int(&format!("{p}pendingRs")).unwrap_or(0) as usize,
                    macro_args_base: get_int(&format!("{p}macroArgStackBase")).unwrap_or(0)
                        as usize,
                    macro_args_depth: get_int(&format!("{p}macroArgStackDepth")).unwrap_or(0)
                        as usize,
                    exclude_level: get_int(&format!("{p}excludeLevel")).unwrap_or(0) as i32,
                    if_level: get_int(&format!("{p}ifLevel")).unwrap_or(0) as i32,
                    ..CallStackEntry::default()
                };
                if let Some(v) = get(&format!("{p}excludeLevelStack")) {
                    e.exclude_level_stack = decode_int_stack(v);
                }
                if let Some(v) = get(&format!("{p}ifLevelExecutedStack")) {
                    e.if_level_executed_stack = decode_bool_stack(v);
                }
                let sc = get_int(&format!("{p}spliceCount")).unwrap_or(0) as usize;
                for j in 0..sc {
                    if let Some(v) = get(&format!("{p}splice.{j}"))
                        && let Some(s) = decode_splice(v)
                    {
                        e.splice.push(s);
                    }
                }
                self.call_stack.push(e);
            }
        }

        // splice stack
        self.splice_stack.clear();
        if let Some(n) = get_int("spliceCount") {
            let n = n as usize;
            for j in 0..n {
                if let Some(v) = get(&format!("splice.{j}"))
                    && let Some(s) = decode_splice(v)
                {
                    self.splice_stack.push(s);
                }
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------------
    // property-style accessors (methods, since the ABI has no instance
    // properties)
    // -------------------------------------------------------------------

    pub fn get_cur_line(&self) -> usize {
        self.cur_line
    }

    pub fn get_cur_pos(&self) -> usize {
        self.current_char_pos()
    }

    pub fn get_cur_line_str(&self) -> String {
        self.raw_lines
            .get(self.cur_line.saturating_sub(1))
            .cloned()
            .unwrap_or_default()
    }

    pub fn get_ignore_cr(&self) -> bool {
        self.ignore_cr
    }

    pub fn set_ignore_cr(&mut self, v: bool) {
        self.ignore_cr = v;
    }

    pub fn get_process_special_tags(&self) -> bool {
        self.process_special_tags
    }

    pub fn set_process_special_tags(&mut self, v: bool) {
        self.process_special_tags = v;
    }

    pub fn get_debug_level(&self) -> i32 {
        self.debug_level
    }

    pub fn set_debug_level(&mut self, v: i32) {
        self.debug_level = v;
    }

    pub fn get_storage_name(&self) -> &str {
        &self.storage_name
    }

    pub fn get_cur_label(&self) -> &str {
        &self.cur_label
    }

    pub fn get_call_stack_depth(&self) -> usize {
        self.call_stack.len()
    }

    /// The macros dictionary as an ordered `Vec<(name, body)>` (for
    /// building a real TJS Dictionary result).
    pub fn get_macros_entries(&self) -> Vec<(String, String)> {
        self.macros
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Serialize the macros dictionary as `name=body` kvp lines (kept for
    /// `store()`/string round-trips).
    pub fn get_macros(&self) -> String {
        kvp::encode_dict(&self.get_macros_entries())
    }

    pub fn set_macros(&mut self, s: &str) -> Result<(), String> {
        let entries = kvp::decode_dict(s);
        self.macros.clear();
        for (k, v) in entries {
            self.macros.insert(k, v);
        }
        Ok(())
    }

    /// The top macro-args dictionary (`macroParams`/`mp`), or `None` when
    /// no macro arguments are on the stack (the reference returns void).
    /// The top macro-args dictionary (`macroParams`/`mp`) as ordered
    /// `Vec<(name, value)>`, or `None` when no macro arguments are on the
    /// stack (the reference returns void).
    pub fn get_macro_params_entries(&self) -> Option<Vec<(String, String)>> {
        let top = self.macro_args.last()?;
        Some(top.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    /// The top macro-args dictionary as kvp lines, or `None` when empty
    /// (kept for callers that need the string form).
    pub fn get_macro_params(&self) -> Option<String> {
        Some(kvp::encode_dict(&self.get_macro_params_entries()?))
    }

    pub fn interrupt(&mut self) {
        self.interrupted = true;
    }

    pub fn reset_interrupt(&mut self) {
        self.interrupted = false;
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Split text into lines and strip leading tabs (reference `LoadScenario`
/// pass 1/2). An empty source yields no lines; a trailing newline does not
/// add an empty final line.
fn split_lines(source: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut rest = source;
    while let Some(idx) = rest.find(['\r', '\n']) {
        let line = source[start..start + idx].trim_start_matches('\t');
        lines.push(line.to_string());
        let newline_len = match rest.as_bytes()[idx] {
            b'\r' if rest.as_bytes().get(idx + 1) == Some(&b'\n') => 2,
            _ => 1,
        };
        start += idx + newline_len;
        rest = &rest[idx + newline_len..];
    }
    if start < source.len() {
        lines.push(source[start..].trim_start_matches('\t').to_string());
    }
    lines
}

/// The last path segment of a storage name (`TVPExtractStorageName`).
fn extract_storage_name(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_string()
}

/// Reconstruct `[name key=value ...]` from parsed events for macro
/// recording; values that would not re-parse as-is are double-quoted with
/// backtick escapes.
fn reconstruct_tag(name: &str, params: &[(String, String)]) -> String {
    let mut s = String::with_capacity(16);
    s.push('[');
    s.push_str(name);
    for (k, v) in params {
        s.push(' ');
        s.push_str(k);
        s.push('=');
        s.push_str(&quote_value(v));
    }
    s.push(']');
    s
}

fn quote_value(v: &str) -> String {
    let needs_quotes = v.is_empty()
        || v.chars()
            .any(|c| matches!(c, ' ' | '\t' | ']' | '"' | '\'' | '`'));
    if !needs_quotes {
        return v.to_string();
    }
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        if matches!(c, '"' | '`') {
            out.push('`');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Parse macro content back into events (the reference re-parses the
/// spliced buffer; we pre-parse once).
fn parse_macro_content(content: &str) -> Result<Vec<Event>, String> {
    kag::parse(content)
        .map(|s| s.events)
        .map_err(|e| format!("{MSG_SYNTAX} ({e})"))
}

fn bool_str(b: bool) -> String {
    if b { "1".to_string() } else { "0".to_string() }
}

/// Hex-encode an int stack like the reference `StoreIntStackToDic`.
fn encode_int_stack(stack: &[i32]) -> String {
    let hex = b"0123456789abcdef";
    let mut out = String::with_capacity(stack.len() * 8);
    for &v in stack {
        let v = v as u32;
        for shift in (0..4).rev() {
            out.push(hex[((v >> (shift * 8 + 4)) & 0xf) as usize] as char);
            out.push(hex[((v >> (shift * 8)) & 0xf) as usize] as char);
        }
    }
    out
}

fn decode_int_stack(s: &str) -> Vec<i32> {
    let mut stack = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 8 <= bytes.len() {
        let mut v: u32 = 0;
        for j in 0..8 {
            let c = bytes[i + j];
            let digit = if c.is_ascii_digit() {
                c - b'0'
            } else {
                c.to_ascii_lowercase() - b'a' + 10
            } as u32;
            v = (v << 4) | digit;
        }
        stack.push(v as i32);
        i += 8;
    }
    stack
}

/// Encode a bool stack as "01" characters (reference
/// `StoreBoolStackToDic`).
fn encode_bool_stack(stack: &[bool]) -> String {
    stack.iter().map(|&b| if b { '1' } else { '0' }).collect()
}

fn decode_bool_stack(s: &str) -> Vec<bool> {
    s.chars().map(|c| c == '1').collect()
}

/// Serialize one splice as `K:0,char_pos,line_r_after:<remaining events>`.
/// The remaining events are serialized directly (event_pos is slice-
/// relative, so it is always 0 here; char_pos applies to the first event
/// when it is a partially-emitted text run).
fn encode_splice(s: &Splice) -> String {
    let mut out = String::new();
    out.push_str("K:0,");
    out.push_str(&s.char_pos.to_string());
    out.push(',');
    out.push_str(if s.line_r_after { "1" } else { "0" });
    out.push(':');
    out.push_str(&encode_events(&s.events[s.event_pos..]));
    out
}

/// Escape one event field: on top of [`kvp::escape_field`], the structural
/// separators `;` `:` `=` are escaped too so they can appear in text.
fn escape_event_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            ';' => out.push_str("\\;"),
            ':' => out.push_str("\\:"),
            '=' => out.push_str("\\="),
            c => out.push(c),
        }
    }
    out
}

/// Encode a list of events compactly: `T:<text>`, `G:<name>:<n>:<br>;k=v;...`
/// (fields escaped with [`escape_event_field`]), `L:<name>[:<page>]`,
/// `C:<text>`, `D:<raw>`.
fn encode_events(events: &[Event]) -> String {
    let mut out = String::new();
    for (i, e) in events.iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        match e {
            Event::Text { text, .. } => {
                out.push('T');
                out.push(':');
                out.push_str(&escape_event_field(text));
            }
            Event::Tag {
                name,
                params,
                bracket,
                ..
            } => {
                out.push('G');
                out.push(':');
                out.push_str(&escape_event_field(name));
                out.push(':');
                out.push_str(&params.len().to_string());
                out.push(':');
                out.push(if *bracket == Bracket::At { 'A' } else { 'S' });
                for (k, v) in params {
                    out.push(';');
                    out.push_str(&escape_event_field(k));
                    out.push('=');
                    out.push_str(&escape_event_field(v));
                }
            }
            Event::Label {
                name, macro_name, ..
            } => {
                out.push('L');
                out.push(':');
                out.push_str(&escape_event_field(name));
                if let Some(m) = macro_name {
                    out.push(':');
                    out.push_str(&escape_event_field(m));
                }
            }
            Event::Comment { text, .. } => {
                out.push('C');
                out.push(':');
                out.push_str(&escape_event_field(text));
            }
            Event::Directive { raw, .. } => {
                out.push('D');
                out.push(':');
                out.push_str(&escape_event_field(raw));
            }
        }
    }
    out
}

/// Decode a splice encoding (see [`encode_splice`]).
fn decode_splice(s: &str) -> Option<Splice> {
    let (kind, rest) = s.split_once(':')?;
    if kind != "K" {
        return None;
    }
    let (nums, content) = rest.split_once(':')?;
    let mut nums = nums.split(',');
    let event_pos: usize = nums.next()?.parse().ok()?;
    let char_pos: usize = nums.next()?.parse().ok()?;
    let line_r_after = nums.next()? == "1";
    let events = decode_events(content);
    Some(Splice {
        events,
        event_pos,
        char_pos,
        line_r_after,
    })
}

/// Decode the event list produced by [`encode_events`].
fn decode_events(s: &str) -> Vec<Event> {
    let mut events = Vec::new();
    let mut i = 0;
    let chars: Vec<char> = s.chars().collect();
    while i < chars.len() {
        if chars[i] == ';' {
            i += 1; // event separator
        }
        if i + 1 >= chars.len() || chars[i + 1] != ':' {
            break;
        }
        let kind = chars[i];
        i += 2;
        let (field, next) = read_field(&chars, i);
        i = next;
        match kind {
            'T' => events.push(Event::Text {
                text: field,
                line: 0,
            }),
            'C' => events.push(Event::Comment {
                text: field,
                line: 0,
            }),
            'D' => events.push(Event::Directive {
                name: String::new(),
                args: Vec::new(),
                raw: field,
                line: 0,
            }),
            'L' => {
                let macro_name = if i < chars.len() && chars[i] == ':' {
                    i += 1;
                    let (m, n) = read_field(&chars, i);
                    i = n;
                    Some(m)
                } else {
                    None
                };
                events.push(Event::Label {
                    name: field,
                    macro_name,
                    line: 0,
                });
            }
            'G' => {
                // name : count : bracket ; k=v;...
                let name = field;
                let (count_str, n2) = if i < chars.len() && chars[i] == ':' {
                    i += 1;
                    read_field(&chars, i)
                } else {
                    (String::new(), i)
                };
                i = n2;
                let count: usize = count_str.parse().unwrap_or(0);
                let (bracket_char, n3) = if i < chars.len() && chars[i] == ':' {
                    i += 1;
                    if i < chars.len() {
                        (Some(chars[i]), i + 1)
                    } else {
                        (None, i)
                    }
                } else {
                    (None, i)
                };
                i = n3;
                let bracket = if bracket_char == Some('A') {
                    Bracket::At
                } else {
                    Bracket::Square
                };
                let mut params = Vec::new();
                for _ in 0..count {
                    if i < chars.len() && chars[i] == ';' {
                        i += 1;
                    }
                    let (k, n4) = read_field(&chars, i);
                    i = n4;
                    let v = if i < chars.len() && chars[i] == '=' {
                        i += 1;
                        let (vv, n5) = read_field(&chars, i);
                        i = n5;
                        vv
                    } else {
                        String::new()
                    };
                    params.push((k, v));
                }
                events.push(Event::Tag {
                    name,
                    params,
                    bracket,
                    line: 0,
                });
            }
            _ => break,
        }
    }
    events
}

/// Read one escaped field from `chars` starting at `i`; returns the field
/// and the index just past its terminating `;`/`:`/`=`/end.
fn read_field(chars: &[char], mut i: usize) -> (String, usize) {
    let mut out = String::new();
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                i += 1;
                if i >= chars.len() {
                    break;
                }
                match chars[i] {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    c => out.push(c),
                }
                i += 1;
            }
            ';' | ':' | '=' => break,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    (out, i)
}

#[cfg(test)]
mod event_codec_tests {
    use super::*;

    #[test]
    fn events_with_separator_chars_round_trip() {
        let events = vec![
            Event::Text {
                text: "a;b:c=d\\e".into(),
                line: 0,
            },
            Event::Tag {
                name: "bg".into(),
                params: vec![
                    ("storage".into(), "a=b;c".into()),
                    ("loop".into(), "true".into()),
                ],
                bracket: Bracket::At,
                line: 0,
            },
        ];
        let s = encode_events(&events);
        assert_eq!(decode_events(&s), events);
    }

    #[test]
    fn splice_round_trip() {
        let splice = Splice {
            events: vec![
                Event::Tag {
                    name: "a".into(),
                    params: vec![("x".into(), "1".into())],
                    bracket: Bracket::At,
                    line: 0,
                },
                Event::Text {
                    text: "rest;".into(),
                    line: 0,
                },
            ],
            event_pos: 1,
            char_pos: 2,
            line_r_after: true,
        };
        let s = encode_splice(&splice);
        let decoded = decode_splice(&s).expect("decodes");
        assert_eq!(decoded.events, splice.events[1..]);
        assert_eq!(decoded.event_pos, 0);
        assert_eq!(decoded.char_pos, 2);
        assert!(decoded.line_r_after);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_eval(_exp: &str) -> Result<EvalResult, String> {
        Err("unexpected eval".to_string())
    }

    /// A parser plus a name→text storage map, with an environment that
    /// borrows each half separately (the state machine takes `&mut self`
    /// plus `&mut Environ`, so the borrows are split at the call site).
    struct Harness {
        state: KagParserState,
        files: BTreeMap<String, String>,
    }

    impl Harness {
        fn new(files: &[(&str, &str)]) -> Harness {
            let mut map = BTreeMap::new();
            for (k, v) in files {
                map.insert(k.to_string(), v.to_string());
            }
            Harness {
                state: KagParserState::default(),
                files: map,
            }
        }

        /// Run `f` with split borrows: the state and an environment whose
        /// storage callback reads `self.files`.
        fn with_env<T>(
            &mut self,
            eval: &mut dyn FnMut(&str) -> Result<EvalResult, String>,
            f: impl FnOnce(&mut KagParserState, &mut Environ<'_>) -> T,
        ) -> T {
            let Harness { state, files } = self;
            let mut env = Environ {
                eval,
                load_storage: &mut |name: &str| {
                    files
                        .get(name)
                        .cloned()
                        .ok_or_else(|| format!("storage {name} not found"))
                },
            };
            f(state, &mut env)
        }

        /// Walk the whole scenario collecting tag outputs.
        fn walk(&mut self, name: &str) -> Vec<TagOutput> {
            let mut no_eval: fn(&str) -> Result<EvalResult, String> = no_eval;
            self.with_env(&mut no_eval, |state, env| {
                state.load_scenario(name, env).unwrap();
                let mut tags = Vec::new();
                while let Some(t) = state.next_tag(env).unwrap() {
                    tags.push(t);
                }
                tags
            })
        }
    }

    fn tag(name: &str, params: &[(&str, &str)]) -> TagOutput {
        (
            name.to_string(),
            params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn text_emits_chars_and_line_ends_emit_r() {
        let mut h = Harness::new(&[("a.ks", "*start\nHello\nWorld\n")]);
        let tags = h.walk("a.ks");
        let mut expected = vec![
            tag("ch", &[("text", "H")]),
            tag("ch", &[("text", "e")]),
            tag("ch", &[("text", "l")]),
            tag("ch", &[("text", "l")]),
            tag("ch", &[("text", "o")]),
            tag("r", &[("eol", "true")]),
            tag("ch", &[("text", "W")]),
            tag("ch", &[("text", "o")]),
            tag("ch", &[("text", "r")]),
            tag("ch", &[("text", "l")]),
            tag("ch", &[("text", "d")]),
            tag("r", &[("eol", "true")]),
        ];
        assert_eq!(tags, expected);

        // ignoreCR = true → no r tags at all
        let mut no_eval: fn(&str) -> Result<EvalResult, String> = no_eval;
        h.with_env(&mut no_eval, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            state.set_ignore_cr(true);
            let mut tags = Vec::new();
            while let Some(t) = state.next_tag(env).unwrap() {
                tags.push(t);
            }
            expected.retain(|t| t.0 != "r");
            assert_eq!(tags, expected);
        });
    }

    #[test]
    fn at_and_square_tags_and_empty_lines() {
        let mut h = Harness::new(&[(
            "a.ks",
            "; comment\n*start\nHello [b]world[/b]!\n@wait 1000\n[bg storage=\"a b.jpg\"]\n\nNext\n",
        )]);
        let tags = h.walk("a.ks");
        let names: Vec<&str> = tags.iter().map(|t| t.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "ch", "ch", "ch", "ch", "ch", "ch", "b", "ch", "ch", "ch", "ch", "ch", "/b", "ch",
                "r", "wait", "bg", "r", "r", "ch", "ch", "ch", "ch", "r",
            ]
        );
        // [bg storage="a b.jpg"] value keeps the space
        let bg = tags.iter().find(|t| t.0 == "bg").unwrap();
        assert_eq!(bg.1, vec![("storage".to_string(), "a b.jpg".to_string())]);
        // two r's before "Next": [bg] line end + the empty line
        let r_count = tags.iter().filter(|t| t.0 == "r").count();
        assert_eq!(r_count, 4);
    }

    #[test]
    fn go_to_label_and_jump_loop() {
        let mut h = Harness::new(&[("a.ks", "*start\nOne\n*loop\n[wait]\n@jump target=*loop\n")]);
        let mut no_eval: fn(&str) -> Result<EvalResult, String> = no_eval;
        h.with_env(&mut no_eval, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            let mut tags = Vec::new();
            let mut guards = 0;
            while guards < 2 {
                let t = state.next_tag(env).unwrap();
                match &t {
                    Some((n, _)) if n == "wait" => {
                        tags.push(t.unwrap());
                        guards += 1;
                    }
                    Some(_) => tags.push(t.unwrap()),
                    None => panic!("unexpected end of scenario"),
                }
            }
            // One, r, wait, r, (jump), wait, r — two wait tags prove the loop.
            assert_eq!(tags.iter().filter(|t| t.0 == "wait").count(), 2);
            assert_eq!(tags[0], tag("ch", &[("text", "O")]));
            assert!(tags.contains(&tag("r", &[("eol", "true")])));

            // goToLabel from the middle
            state.go_to_label("*loop").unwrap();
            let t = state.next_tag(env).unwrap().unwrap();
            assert_eq!(t, tag("wait", &[]));

            // missing label → error
            let err = state.go_to_label("*nope").unwrap_err();
            assert!(err.contains("Label *nope not found"), "{err}");
        });
    }

    #[test]
    fn call_tag_and_return() {
        let mut h = Harness::new(&[(
            "a.ks",
            "*start\nFirst\n@call target=*sub\n@jump target=*end\n*sub\nSubText\n@return\n*end\nLast\n",
        )]);
        let tags = h.walk("a.ks");
        let names: Vec<&str> = tags.iter().map(|t| t.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "ch", "ch", "ch", "ch", "ch", "r", // First + r
                "ch", "ch", "ch", "ch", "ch", "ch", "ch", "r", // SubText + r
                "ch", "ch", "ch", "ch", "r", // Last + r
            ]
        );
        let mut no_eval: fn(&str) -> Result<EvalResult, String> = no_eval;
        h.with_env(&mut no_eval, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            state.next_tag(env).unwrap(); // ch F
            assert_eq!(state.get_call_stack_depth(), 0);
            state.call_label("*sub", env).unwrap();
            assert_eq!(state.get_call_stack_depth(), 1);
        });
    }

    #[test]
    fn call_label_restores_mid_text_position() {
        let mut h = Harness::new(&[(
            "a.ks",
            "*start\nFirst\n@jump target=*end\n*sub\nSubText\n@return\n*end\n",
        )]);
        let mut no_eval: fn(&str) -> Result<EvalResult, String> = no_eval;
        h.with_env(&mut no_eval, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            state.next_tag(env).unwrap(); // ch F
            state.next_tag(env).unwrap(); // ch i
            eprintln!("depth before call: {}", state.get_call_stack_depth());
            state.call_label("*sub", env).unwrap();
            eprintln!("depth after call: {}", state.get_call_stack_depth());
            let mut tags = Vec::new();
            loop {
                match state.next_tag(env) {
                    Ok(Some(t)) => {
                        eprintln!("TAG {:?}", t);
                        tags.push(t);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("ERR {e}");
                        break;
                    }
                }
            }
            // *sub body: S,u,b,T,e,x,t, r — then the return resumes mid
            // "First" at char index 2: r, s, t, r — then @jump *end.
            assert_eq!(tags[0], tag("ch", &[("text", "S")]));
            assert_eq!(tags[7], tag("r", &[("eol", "true")]));
            assert_eq!(tags[8], tag("ch", &[("text", "r")]));
            assert_eq!(tags[9], tag("ch", &[("text", "s")]));
            assert_eq!(tags[10], tag("ch", &[("text", "t")]));
            assert_eq!(tags.len(), 12);
        });
    }

    #[test]
    fn return_without_call_is_an_error() {
        let mut h = Harness::new(&[("a.ks", "*start\n@return\n")]);
        let mut no_eval: fn(&str) -> Result<EvalResult, String> = no_eval;
        h.with_env(&mut no_eval, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            let err = state.next_tag(env).unwrap_err();
            assert!(err.contains("return"), "{err}");
        });
    }

    #[test]
    fn if_else_endif_with_eval() {
        let mut h = Harness::new(&[(
            "a.ks",
            "@if exp=\"cond1\"\n@x a=1\n@else\n@y b=2\n@endif\n@z c=3\n",
        )]);
        // cond1 true → x emitted, y skipped
        let mut cond = |exp: &str| {
            Ok(if exp == "cond1" {
                EvalResult::Integer(1)
            } else {
                EvalResult::Integer(0)
            })
        };
        h.with_env(&mut cond, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            let t = state.next_tag(env).unwrap().unwrap();
            assert_eq!(t, tag("x", &[("a", "1")]));
            let t = state.next_tag(env).unwrap().unwrap();
            assert_eq!(t, tag("z", &[("c", "3")]));
            assert!(state.next_tag(env).unwrap().is_none());
        });
        // cond1 false → x skipped, y emitted
        let mut cond = |_exp: &str| Ok(EvalResult::Integer(0));
        h.with_env(&mut cond, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            let t = state.next_tag(env).unwrap().unwrap();
            assert_eq!(t, tag("y", &[("b", "2")]));
            let t = state.next_tag(env).unwrap().unwrap();
            assert_eq!(t, tag("z", &[("c", "3")]));
        });
    }

    #[test]
    fn macro_recording_and_expansion_with_args() {
        let mut h = Harness::new(&[(
            "a.ks",
            "*start\n[macro name=bgm]\n[bgmplay storage=%storage loop=true]\n[endmacro]\n@bgm storage=\"01.mp3\"\n",
        )]);
        let tags = h.walk("a.ks");
        // r after [endmacro], then the expansion: content =
        // "[r eol=true][bgmplay storage=%storage loop=true][r eol=true][macropop]".
        assert_eq!(
            tags,
            vec![
                tag("r", &[("eol", "true")]),
                tag("r", &[("eol", "true")]),
                tag("bgmplay", &[("storage", "01.mp3"), ("loop", "true")]),
                tag("r", &[("eol", "true")]),
            ]
        );
        // the recorded body contains the reconstruction + [macropop]
        let macros = h.state.get_macros();
        assert!(macros.contains("bgmplay"), "{macros}");
        assert!(macros.contains("storage=%storage"), "{macros}");
        assert!(macros.contains("macropop"), "{macros}");
        assert!(macros.contains("eol=true"), "{macros}");
    }

    #[test]
    fn macro_arg_default_and_missing() {
        let mut h = Harness::new(&[(
            "a.ks",
            "[macro name=mv]\n@move my=%height|10 x=%missing\n[endmacro]\n@mv\n@mv height=99\n",
        )]);
        let tags = h.walk("a.ks");
        let names: Vec<&str> = tags.iter().map(|t| t.0.as_str()).collect();
        // r (after [endmacro]), then each expansion emits
        // [r eol=true] + the move tag.
        assert_eq!(names, ["r", "r", "move", "r", "move"]);
        // First expansion: no args → default height 10, %missing omitted.
        assert_eq!(tags[2], tag("move", &[("my", "10")]));
        // Second expansion with height=99 → resolved arg.
        assert_eq!(tags[4], tag("move", &[("my", "99")]));
    }

    #[test]
    fn store_restore_round_trip() {
        let mut h = Harness::new(&[("a.ks", "*start\nFirst\n@bg file=\"x.jpg\"\nSecond\n@wait\n")]);
        let mut no_eval: fn(&str) -> Result<EvalResult, String> = no_eval;
        h.with_env(&mut no_eval, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            // walk 5 tags (ch F, ch i, ch r, ch s, ch t)
            for _ in 0..5 {
                state.next_tag(env).unwrap();
            }
            let saved = state.store();
            // walk the rest
            let mut rest1 = Vec::new();
            while let Some(t) = state.next_tag(env).unwrap() {
                rest1.push(t);
            }
            // restore and walk the rest again — same sequence
            state.restore(&saved, env).unwrap();
            let mut rest2 = Vec::new();
            while let Some(t) = state.next_tag(env).unwrap() {
                rest2.push(t);
            }
            assert_eq!(rest1, rest2);
            assert!(!rest2.is_empty());
        });
    }

    #[test]
    fn call_inside_macro_expansion_returns_into_splice() {
        // A call made from inside a macro expansion: the pending splice is
        // saved on the call stack and restored on return. Like the
        // reference (which re-enters the calling line's buffer and then
        // continues after the calling line), the walk re-processes the
        // events after the macro tag once the splice is exhausted.
        let mut h = Harness::new(&[(
            "a.ks",
            "[macro name=mm]\n@a x=1\n@call target=*sub\n@b y=2\n@jump target=*end\n[endmacro]\n@mm\n*sub\n@c z=3\n@return\n*end\n",
        )]);
        let tags = h.walk("a.ks");
        let names: Vec<&str> = tags.iter().map(|t| t.0.as_str()).collect();
        // r (after [endmacro]), then the expansion: r, a, (call→sub), c,
        // (return→splice: b, jump *end) → *end.
        assert_eq!(names, ["r", "r", "a", "c", "b"]);
    }

    #[test]
    fn interrupt_returns_interrupt_tag() {
        let mut h = Harness::new(&[("a.ks", "*start\nHello\n")]);
        let mut no_eval: fn(&str) -> Result<EvalResult, String> = no_eval;
        h.with_env(&mut no_eval, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            state.interrupt();
            let t = state.next_tag(env).unwrap().unwrap();
            assert_eq!(t, tag("interrupt", &[]));
            // normal walk continues after the interrupt
            let t = state.next_tag(env).unwrap().unwrap();
            assert_eq!(t, tag("ch", &[("text", "H")]));
        });
    }

    #[test]
    fn emb_inserts_evaluated_text() {
        let mut h = Harness::new(&[("a.ks", "*start\n[emb exp=\"3+4\"]!\n")]);
        let mut ev = |_exp: &str| Ok(EvalResult::Integer(7));
        h.with_env(&mut ev, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            let mut tags = Vec::new();
            while let Some(t) = state.next_tag(env).unwrap() {
                tags.push(t);
            }
            let names: Vec<&str> = tags.iter().map(|t| t.0.as_str()).collect();
            assert_eq!(names, ["ch", "ch", "r"]);
            assert_eq!(tags[0], tag("ch", &[("text", "7")]));
            assert_eq!(tags[1], tag("ch", &[("text", "!")]));
        });
    }

    #[test]
    fn process_special_tags_off_returns_everything() {
        let mut h = Harness::new(&[(
            "a.ks",
            "*start\n@if exp=\"x\"\n@endif\n@jump target=*start\n",
        )]);
        let mut no_eval: fn(&str) -> Result<EvalResult, String> = no_eval;
        h.with_env(&mut no_eval, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            state.set_process_special_tags(false);
            let mut tags = Vec::new();
            while let Some(t) = state.next_tag(env).unwrap() {
                tags.push(t);
            }
            let names: Vec<&str> = tags.iter().map(|t| t.0.as_str()).collect();
            assert_eq!(names, ["if", "endif", "jump"]);
        });
    }

    #[test]
    fn cond_attribute_skips_tags() {
        let mut h = Harness::new(&[("a.ks", "*start\n@x a=1 cond=\"no\"\n@y b=2 cond=\"yes\"\n")]);
        let mut ev = |exp: &str| {
            Ok(if exp == "yes" {
                EvalResult::Integer(1)
            } else {
                EvalResult::Integer(0)
            })
        };
        h.with_env(&mut ev, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            let mut tags = Vec::new();
            while let Some(t) = state.next_tag(env).unwrap() {
                tags.push(t);
            }
            assert_eq!(tags, vec![tag("y", &[("b", "2")])]);
        });
    }

    #[test]
    fn entity_values_are_evaluated() {
        let mut h = Harness::new(&[("a.ks", "*start\n@x a=&\"a\"+\"b\"\n")]);
        let mut ev = |exp: &str| Ok(EvalResult::Str(format!("<{exp}>")));
        h.with_env(&mut ev, |state, env| {
            state.load_scenario("a.ks", env).unwrap();
            let t = state.next_tag(env).unwrap().unwrap();
            assert_eq!(t, tag("x", &[("a", "<\"a\"+\"b\">")]));
        });
    }

    #[test]
    fn split_lines_matches_kag() {
        assert_eq!(split_lines("a\nb\nc"), ["a", "b", "c"]);
        assert_eq!(split_lines("a\r\nb\nc\r"), ["a", "b", "c"]);
        assert_eq!(split_lines("a\n"), ["a"]);
        assert_eq!(split_lines("\n"), [""]);
        assert_eq!(split_lines(""), Vec::<String>::new());
        assert_eq!(split_lines("\t\ta\n\tb"), ["a", "b"]);
    }

    #[test]
    fn int_bool_stack_encoding() {
        let stack = vec![1, -2, 0x12345678];
        let s = encode_int_stack(&stack);
        assert_eq!(decode_int_stack(&s), stack);
        let b = vec![true, false, true, true];
        let s = encode_bool_stack(&b);
        assert_eq!(s, "1011");
        assert_eq!(decode_bool_stack(&s), b);
    }

    #[test]
    fn events_round_trip() {
        let events = vec![
            Event::Text {
                text: "he\\llo\nx".into(),
                line: 0,
            },
            Event::Tag {
                name: "bg".into(),
                params: vec![
                    ("storage".into(), "a=b;c".into()),
                    ("loop".into(), "true".into()),
                ],
                bracket: Bracket::At,
                line: 0,
            },
            Event::Label {
                name: "*x".into(),
                macro_name: Some("page".into()),
                line: 0,
            },
        ];
        let s = encode_events(&events);
        assert_eq!(decode_events(&s), events);
    }
}

//! The KAGParser native instance state machine.
//!
//! This is a stateful *pull* wrapper around the `parser` module's *push*
//! parser: [`KagParserState::load`] parses a scenario once into events,
//! and [`KagParserState::next_tag`] walks them one tag at a time, exactly
//! like the reference `tTJSNI_KAGParser::_GetNextTag`
//! (`reference/cpp/core/base/KAGParser.cpp`) pulls one tag per call.
//!
//! # Divergences from the reference (documented)
//!
//! * The reference emits one `ch` tag per **character** of text and an
//!   `r eol=true` tag at every line end (when `ignoreCR` is false); we
//!   keep that (the `parser` module gives whole text runs, which we split
//!   char-by-char). Empty lines, which the `parser` module drops, are
//!   recovered from the line numbers and emit `r eol=true` the same way.
//! * The owner-object event callbacks (`onScenarioLoad`, `onScenarioLoaded`,
//!   `onLabel`, `onScript`, `onJump`, `onCall`, `onReturn`) are supplied by
//!   the native layer through [`Environ`]. A missing handler or a handler
//!   error leaves the tag processed, matching the reference's default.
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
//! * The `parser` module groups a `%` line into [`Event::Directive`];
//!   the walk emits its characters as ordinary text. The base reference has
//!   no `%` handling at all, so the script-visible behaviour matches
//!   (`reference/cpp/core/base/KAGParser.cpp:1464-1512`).

use std::collections::{BTreeMap, BTreeSet};

use super::parser::{Bracket, Event};

use super::kvp;

// Error messages mirroring the reference (KAGParser.cpp).
pub const MSG_NO_LINE: &str = "Readed scenario file %1 is empty.";
pub const MSG_SYNTAX: &str = "Syntax error.'[' match to ']', \" match to \", 'macro' march to 'endmacro'. \
     Notice space and newline.";
pub const MSG_LABEL_NOT_FOUND: &str = "Label %2 not found in scenario file %1.";
pub const MSG_CALL_STACK_UNDERFLOW: &str =
    "'return' is not matched to any 'call' ( 'return' is unexpected )";
pub const MSG_RETURN_LOST_SYNC: &str = "Lost return position due to the scenario file changed.";
pub const MSG_LABEL_IN_MACRO: &str = "Label in macro 'iscript' is illegal.";
pub const MSG_CANNOT_OMIT_FIRST_LABEL: &str = "Can not ommit first label name in scenario.";
pub const MSG_INLINE_SCRIPT_NOT_END: &str = "Matched [endscript] or @endscript not found.";

/// The result of evaluating a TJS expression through [`Environ::eval`].
#[derive(Debug, Clone, PartialEq)]
pub enum EvalResult {
    Void,
    Integer(i64),
    Real(f64),
    Str(String),
    /// An object result. The object reference itself does not cross the
    /// [`Environ`] boundary, so the eval hook carries the string form derived
    /// from the already-evaluated object (`TJSObjectToString`) and the
    /// reference `operator bool` (`Object != nullptr`, so TJS `null` is
    /// false).
    Object {
        text: String,
        truthy: bool,
    },
}

impl EvalResult {
    /// TJS `operator bool` semantics: void → false, numbers → non-zero,
    /// strings → non-empty, objects → not null.
    pub fn truthy(&self) -> bool {
        match self {
            EvalResult::Void => false,
            EvalResult::Integer(i) => *i != 0,
            EvalResult::Real(r) => *r != 0.0,
            EvalResult::Str(s) => !s.is_empty(),
            EvalResult::Object { truthy, .. } => *truthy,
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
            EvalResult::Object { text, .. } => Some(text.clone()),
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

/// Owner callback that fires `onLabel(label, pageName)` as the walk passes
/// a label line (reference `SkipCommentOrLabel`).
pub type LabelCallback<'a> = &'a mut dyn FnMut(&str, Option<&str>);

/// Owner callback that fires `onScript(script, storageShortName, scriptStart)`
/// as the walk passes an inline script block (reference `SkipCommentOrLabel`).
pub type ScriptCallback<'a> = &'a mut dyn FnMut(&str, &str, usize);

/// Owner callback that fires `onJump`/`onCall`/`onReturn` with the candidate
/// tag name and resolved parameters, and returns whether the tag should be
/// processed (`false` vetoes it, `KAGParser.cpp:1888-1975`).
pub type TagCallback<'a> = &'a mut dyn FnMut(&str, &[(String, String)]) -> bool;

/// Owner callback for `onScenarioLoad(name)`: returns the replacement
/// scenario text when the handler returns a string (`KAGParser.cpp:919-937`).
pub type ScenarioLoadCallback<'a> = &'a mut dyn FnMut(&str) -> Option<String>;

/// Owner callback for `onScenarioLoaded(name)` (`KAGParser.cpp:955-966`).
pub type ScenarioLoadedCallback<'a> = &'a mut dyn FnMut(&str);

/// The environment a parser walks in: expression evaluation (into the TJS
/// VM) and scenario loading (from storage). Both are injectable so the
/// state machine stays pure and unit-testable.
pub struct Environ<'a> {
    pub eval: &'a mut dyn FnMut(&str) -> Result<EvalResult, String>,
    pub load_storage: &'a mut dyn FnMut(&str) -> Result<String, String>,
    /// Fires the owner's `onLabel(label, pageName)` callback as the walk
    /// passes a label line (reference `SkipCommentOrLabel`). `None` in pure
    /// state-machine tests, where no owner object exists.
    pub fire_label: Option<LabelCallback<'a>>,
    /// Fires the owner's `onScript` callback for an inline script block.
    pub fire_script: Option<ScriptCallback<'a>>,
    /// Fires the owner's `onJump` callback (vetoes the jump when it returns
    /// false).
    pub fire_jump: Option<TagCallback<'a>>,
    /// Fires the owner's `onCall` callback (vetoes the call when it returns
    /// false).
    pub fire_call: Option<TagCallback<'a>>,
    /// Fires the owner's `onReturn` callback (vetoes the return when it
    /// returns false).
    pub fire_return: Option<TagCallback<'a>>,
    /// Fires the owner's `onScenarioLoad` before a scenario is read.
    pub fire_scenario_load: Option<ScenarioLoadCallback<'a>>,
    /// Fires the owner's `onScenarioLoaded` after a scenario is read.
    pub fire_scenario_loaded: Option<ScenarioLoadedCallback<'a>>,
}

/// One pending macro/emb expansion spliced ahead of the main event stream
/// (the analog of the reference's `LineBuffer`).
#[derive(Debug, Clone, PartialEq)]
struct Splice {
    /// Parsed events of the expansion (macro content) or a single Text
    /// event (emb result).
    events: Vec<Event>,
    /// Per-event scan columns aligned with `events`.
    cols: Vec<usize>,
    /// The expansion text (`LineBuffer`); `curLineStr` returns this while
    /// the expansion is active.
    buffer: String,
    /// Next event / char position within `events`.
    event_pos: usize,
    char_pos: usize,
    /// Whether the tag that started this splice was the last thing on its
    /// line in its own stream and was a `[...]` tag — an `r eol=true`
    /// fires when the outermost expansion is exhausted.
    line_r_after: bool,
}

/// The reference `tCallStackData` fields the native `store()` dictionary
/// carries (`KAGParser.cpp:546-634`). The stack fields use the reference
/// encodings (`StoreIntStackToDic` / `StoreBoolStackToDic`).
#[derive(Debug, Clone)]
pub struct CallStackSnapshot {
    /// The scenario storage the call returns to.
    pub storage: String,
    /// The label the call returns to.
    pub label: String,
    /// The walk offset the call returns to.
    pub offset: usize,
    /// The original line text at the call site.
    pub org_line_str: String,
    /// The position within that line at the call site.
    pub pos: usize,
    /// The expansion buffer alive at the call site (`LineBuffer`).
    pub line_buffer: String,
    /// Whether the expansion buffer was in use (`LineBufferUsing`).
    pub line_buffer_using: bool,
    /// The macro-argument stack base at the call site.
    pub macro_arg_stack_base: usize,
    /// The macro-argument stack depth at the call site.
    pub macro_arg_stack_depth: usize,
    /// The exclude level at the call site.
    pub exclude_level: i32,
    /// The if level at the call site.
    pub if_level: i32,
    /// The exclude-level stack, reference hex-encoded.
    pub exclude_level_stack: String,
    /// The if-executed stack, reference `"01"`-encoded.
    pub if_level_executed_stack: String,
}

/// One call-stack entry (the reference's `tCallStackData`).
#[derive(Debug, Clone, Default, PartialEq)]
struct CallStackEntry {
    storage: String,
    label: String,
    offset: usize,
    org_sig: String,
    char_index: usize,
    /// The scan column at the call site (the reference `tCallStackData`
    /// `Pos`).
    cur_pos: usize,
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
    PMacro,
    ErasePMacro,
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
        "pmacro" => Special::PMacro,
        "erasepmacro" => Special::ErasePMacro,
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
///
/// `Clone` supports the reference `assign` copy
/// (`tTJSNI_KAGParser::operator=`, `KAGParser.cpp:395-466`), which copies
/// the whole walk state between two parser instances.
#[derive(Debug, Clone)]
pub struct KagParserState {
    // scenario
    events: Vec<Event>,
    labels: BTreeMap<String, usize>,
    raw_lines: Vec<String>,
    /// Per-event scan column aligned with `events` (see
    /// [`super::parser::Scenario::cols`]).
    event_cols: Vec<usize>,
    comment_or_label_lines: BTreeSet<usize>,
    storage_name: String,
    storage_short_name: String,
    /// The reference raises `CannotOmitFirstLabelName` when it first builds
    /// the label cache (`GoToLabel` or the first walk step), not at load.
    label_error: bool,
    /// Deferred syntax errors keyed by the event index at which the walk
    /// raises them, with the source line for its line-end boundary.
    pending_errors: BTreeMap<usize, (usize, String)>,

    // walk position
    event_index: usize,
    char_index: usize,
    /// Zero-based index of the line the scan cursor is on (the reference
    /// `CurLine`, `reference/cpp/core/base/KAGParser.cpp:987-991`).
    cur_line: usize,
    /// Column of the scan cursor within the current line or expansion
    /// buffer (the reference `CurPos`).
    cur_pos: usize,
    pending_rs: usize,
    splice_stack: Vec<Splice>,

    // current label/page
    cur_label: String,
    cur_page: String,

    // macro machinery
    macros: BTreeMap<String, String>,
    /// KAGParserEx parameter macros (`paramMacros`): macro name → ordered
    /// `(parameter, value)` pairs. Registered by `@pmacro` (or the
    /// `paramMacros` property) and spliced into a tag whenever one of its
    /// parameter names matches a key.
    param_macros: BTreeMap<String, Vec<(String, String)>>,
    /// Active macro-argument levels, innermost last. Each level is an
    /// ordered `(name, value)` list so `*` injection and the
    /// `macroParams` property preserve source order.
    macro_args: Vec<Vec<(String, String)>>,
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
    pub interrupted: bool,
    /// Whether line-end `r eol=true` tags are suppressed (`ignoreCR`).
    pub ignore_cr: bool,
    /// Whether KAG control tags (`if`, `macro`, `jump`, …) are executed by
    /// the parser instead of being returned as output
    /// (`processSpecialTags`).
    pub process_special_tags: bool,
    /// Whether a tag may continue on `;`-prefixed lines
    /// (`multiLineTagEnabled`, KAGParserEx).
    pub multi_line_tag_enabled: bool,
    /// Reference debug level (`debugLevel`), one of the `tTVPKAGDebugLevel`
    /// values.
    pub debug_level: i32,
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
            event_cols: Vec::new(),
            comment_or_label_lines: BTreeSet::new(),
            storage_name: String::new(),
            storage_short_name: String::new(),
            label_error: false,
            pending_errors: BTreeMap::new(),
            event_index: 0,
            char_index: 0,
            cur_line: 0,
            cur_pos: 0,
            pending_rs: 0,
            splice_stack: Vec::new(),
            cur_label: String::new(),
            cur_page: String::new(),
            macros: BTreeMap::new(),
            param_macros: BTreeMap::new(),
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
            multi_line_tag_enabled: false,
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
        | Event::Comment { line, .. }
        | Event::Script { line, .. } => *line,
    }
}

// ---------------------------------------------------------------------------
// loading
// ---------------------------------------------------------------------------

impl KagParserState {
    /// Load a scenario from already-decoded text. Only an empty scenario
    /// fails here (`TVPKAGNoLine`); syntax and first-label errors are raised
    /// later by [`Self::next_tag`]/[`Self::go_to_label`], like the reference's
    /// lazy `EnsureLabelCache`/`_GetNextTag`
    /// (`reference/cpp/core/base/KAGParser.cpp:194-238`, `:1367-1500`).
    pub fn load_scenario_text(&mut self, name: &str, text: &str) -> Result<(), String> {
        let options = super::parser::ParseOptions {
            multiline_tags: self.multi_line_tag_enabled,
        };
        let recovered = match super::parser::parse_recovering(text, options) {
            Ok(recovered) => recovered,
            Err(super::parser::Error::EmptyScenario) => {
                return Err(MSG_NO_LINE.replace("%1", name));
            }
            Err(e) => return Err(deferred_error_message(&e)),
        };
        self.label_error = recovered.label_error;
        self.pending_errors.clear();
        for pending in &recovered.pending_errors {
            self.pending_errors
                .entry(pending.after_event)
                .or_insert_with(|| (pending.line, deferred_error_message(&pending.error)));
        }
        self.events = recovered.events;
        self.labels = recovered.labels;
        self.event_cols = recovered.cols;
        self.raw_lines = split_lines(text);
        self.comment_or_label_lines = self
            .events
            .iter()
            .filter(|e| matches!(e, Event::Comment { .. } | Event::Label { .. }))
            .map(line_of)
            .collect();
        // The lines an inline script block spans are skipped as a unit, so
        // no `r eol=true` fires for them (`KAGParser.cpp:1130-1176`).
        for event in &self.events {
            if let Event::Script {
                text, start_line, ..
            } = event
            {
                let content_lines = text.matches("\r\n").count();
                for l in (start_line + 1)..=(start_line + content_lines + 1) {
                    self.comment_or_label_lines.insert(l);
                }
            }
        }
        self.storage_name = name.to_string();
        self.storage_short_name = extract_storage_name(name);
        self.rewind();
        self.pending_rs = self.initial_gap_lines();
        Ok(())
    }

    /// Load a scenario through the environment (reads + decodes storage).
    /// Loading the *same* storage again just rewinds, like the reference's
    /// scenario cache (`if(StorageName == name) Rewind()`). Fires the owner's
    /// `onScenarioLoad`/`onScenarioLoaded` callbacks
    /// (`KAGParser.cpp:911-967`).
    pub fn load_scenario(&mut self, name: &str, env: &mut Environ<'_>) -> Result<(), String> {
        if self.storage_name == name {
            self.rewind();
            self.pending_rs = self.initial_gap_lines();
        } else {
            // `onScenarioLoad` may return the scenario text itself.
            let override_text = match env.fire_scenario_load.as_deref_mut() {
                Some(fire) => fire(name),
                None => None,
            };
            let text = match override_text {
                Some(text) => text,
                None => (env.load_storage)(name)?,
            };
            self.load_scenario_text(name, &text)?;
        }
        if let Some(fire) = env.fire_scenario_loaded.as_deref_mut() {
            fire(name);
        }
        Ok(())
    }

    /// Set the walk position to the start.
    fn rewind(&mut self) {
        self.event_index = 0;
        self.char_index = 0;
        self.pending_rs = 0;
        self.splice_stack.clear();
        self.cur_line = 0;
        self.cur_pos = 0;
        self.cur_label.clear();
        self.cur_page.clear();
        self.break_condition_and_macro();
    }

    /// `r eol=true` emissions for empty lines before the first event.
    /// Suppressed entirely when `ignore_cr` is on (the reference wraps every
    /// line-end emission, including this leading gap, in `if(!IgnoreCR)`).
    fn initial_gap_lines(&self) -> usize {
        if self.ignore_cr {
            return 0;
        }
        let mut first_line = self
            .events
            .first()
            .map(line_of)
            .unwrap_or(self.raw_lines.len() + 1);
        if let Some(error_line) = self.pending_error_line(0) {
            first_line = first_line.min(error_line);
        }
        (1..first_line)
            .filter(|l| !self.comment_or_label_lines.contains(l))
            .count()
    }

    /// The source line of the deferred syntax error the walk raises at
    /// `after_event`, if any.
    fn pending_error_line(&self, after_event: usize) -> Option<usize> {
        self.pending_errors.get(&after_event).map(|(line, _)| *line)
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
        self.event_cols.clear();
        self.comment_or_label_lines.clear();
        self.label_error = false;
        self.pending_errors.clear();
        self.storage_name.clear();
        self.storage_short_name.clear();
        self.rewind();
        self.macro_args.clear();
        self.macro_args_base = 0;
        self.call_stack.clear();
    }

    /// Copy another parser's walk state (`operator=`, `KAGParser.cpp:395-466`).
    /// `process_special_tags` and `interrupted` stay with the destination:
    /// the reference does not copy `ProcessSpecialTags`/`Interrupted`.
    pub fn assign_from(&mut self, src: &KagParserState) {
        let process_special_tags = self.process_special_tags;
        let interrupted = self.interrupted;
        *self = src.clone();
        self.process_special_tags = process_special_tags;
        self.interrupted = interrupted;
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
        // `GoToLabel` builds the label cache before looking the label up
        // (`KAGParser.cpp:1021`), so a bad first label surfaces here.
        if self.label_error {
            return Err(MSG_CANNOT_OMIT_FIRST_LABEL.to_string());
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
            self.cur_line = line.saturating_sub(1);
            self.cur_pos = 0;
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
            // The reference builds the label cache at the first
            // `SkipCommentOrLabel` (`KAGParser.cpp:1076`), before any tag.
            if self.label_error {
                return Err(MSG_CANNOT_OMIT_FIRST_LABEL.to_string());
            }
            if self.pending_rs > 0 {
                self.pending_rs -= 1;
                // A line end advances the scan cursor to the next line
                // before the `r` tag is returned (`CurLine++` in
                // `_GetNextTag`).
                self.cur_line += 1;
                self.cur_pos = 0;
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
                // A line end inside an excluded (false `if`/`ignore`)
                // region is consumed but not returned, matching the
                // reference `_GetNextTag` (`!RecordingMacro && ExcludeLevel
                // == -1`).
                if self.exclude_level != -1 {
                    continue;
                }
                return Ok(Some((
                    "r".to_string(),
                    vec![("eol".to_string(), "true".to_string())],
                )));
            }

            // A deferred syntax error is raised once the walk has emitted
            // every event before it (`_GetNextTag` scans the line and throws).
            if self.splice_stack.is_empty()
                && let Some((_, message)) = self.pending_errors.get(&self.event_index)
            {
                return Err(message.clone());
            }

            let (ev, from_splice) = self.current_event_cloned();
            let Some(ev) = ev else {
                if from_splice {
                    self.pop_splice();
                    continue;
                }
                return Ok(None);
            };

            // The scan cursor sits on the event's source line before it is
            // read (the reference `SkipCommentOrLabel` sets `CurLine`
            // first). A splice walks an expansion buffer, so the source
            // line does not change.
            if !from_splice {
                match &ev {
                    Event::Text { line, .. }
                    | Event::Tag { line, .. }
                    | Event::Label { line, .. }
                    | Event::Directive { line, .. }
                    | Event::Script { line, .. } => {
                        self.cur_line = line.saturating_sub(1);
                    }
                    Event::Comment { .. } => {}
                }
            }

            match &ev {
                Event::Text { text, .. } | Event::Directive { raw: text, .. } => {
                    if self.recording_macro {
                        self.record_text(text);
                        self.advance_current();
                        continue;
                    }
                    // Text inside an excluded region is consumed silently,
                    // matching the reference `_GetNextTag` (a normal
                    // character is only returned when `ExcludeLevel == -1`).
                    if self.exclude_level != -1 {
                        self.advance_current();
                        continue;
                    }
                    let pos = self.current_char_pos();
                    let Some(c) = text.chars().nth(pos) else {
                        // Run already fully emitted (defensive).
                        self.advance_current();
                        continue;
                    };
                    // A trailing `\` at the end of a line is a line
                    // continuation: it is dropped and no `r` fires
                    // (`KAGParser.cpp:1415-1429`).
                    if !from_splice
                        && pos + 1 == text.chars().count()
                        && c == '\\'
                        && !self.ignore_cr
                        && self
                            .raw_lines
                            .get(line_of(&ev).saturating_sub(1))
                            .is_some_and(|raw| raw.ends_with('\\'))
                    {
                        self.advance_current();
                        continue;
                    }
                    self.bump_char_pos();
                    // `CurPos` is the column within the whole line (or
                    // expansion buffer), one past the emitted character.
                    self.cur_pos = self.current_col() + self.current_char_pos();
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
                    macro_entity,
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
                            self.record_tag(name, params, *macro_entity);
                        }
                        self.advance_current();
                        continue;
                    }
                    // The tag's column is the position just past its
                    // closing delimiter (a line command ends its line).
                    let tag_col = self.current_col();
                    self.advance_current();
                    if let Some(out) =
                        self.process_tag(name, params, *macro_entity, *bracket, *line, env)?
                    {
                        if *bracket == Bracket::At {
                            // A line command advances to the next line
                            // (`TVP_KAG_STEP_NEXT`).
                            self.cur_line = *line;
                            self.cur_pos = 0;
                        } else {
                            self.cur_pos = tag_col;
                        }
                        return Ok(Some(out));
                    }
                }
                Event::Label {
                    name, macro_name, ..
                } => {
                    if self.recording_macro {
                        return Err(MSG_LABEL_IN_MACRO.to_string());
                    }
                    // Mirror reference SkipCommentOrLabel: advancing past
                    // a label updates the current label/page/line and fires
                    // the owner's `onLabel(label, pageName)` callback even
                    // though the label itself emits no tag.
                    let name = name.clone();
                    let macro_name = macro_name.clone();
                    self.cur_label = name.clone();
                    self.cur_page = macro_name.clone().unwrap_or_default();
                    self.cur_pos = 0;
                    if let Some(fire) = env.fire_label.as_deref_mut() {
                        fire(&name, macro_name.as_deref());
                    }
                    self.advance_current();
                }
                // An inline script block fires the owner's `onScript` and is
                // consumed (no tag); inside a macro recording it is illegal
                // (`KAGParser.cpp:1130-1176`).
                Event::Script {
                    text, start_line, ..
                } => {
                    if self.recording_macro {
                        return Err(MSG_LABEL_IN_MACRO.to_string());
                    }
                    if self.exclude_level == -1
                        && let Some(fire) = env.fire_script.as_deref_mut()
                    {
                        fire(text, &self.storage_short_name, *start_line);
                    }
                    self.advance_current();
                }
                // A comment line is control flow (the reference
                // `SkipCommentOrLabel` skips it and leaves `CurPos` at 0).
                Event::Comment { .. } => {
                    self.cur_pos = 0;
                    self.advance_current();
                }
            }
        }
    }

    /// The scan column recorded for the current event (see
    /// [`super::parser::Scenario::cols`]).
    fn current_col(&self) -> usize {
        if let Some(top) = self.splice_stack.last() {
            top.cols.get(top.event_pos).copied().unwrap_or(0)
        } else {
            self.event_cols.get(self.event_index).copied().unwrap_or(0)
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

        let mut next_line = self
            .events
            .get(self.event_index + 1)
            .map(line_of)
            .unwrap_or(self.raw_lines.len() + 1);
        // A deferred syntax error is a boundary: no line end follows it.
        if let Some(error_line) = self.pending_error_line(self.event_index + 1) {
            next_line = next_line.min(error_line);
        }

        let line_ends = match &ev {
            Event::Tag {
                bracket: Bracket::At,
                ..
            } => false,
            Event::Tag { .. } | Event::Text { .. } | Event::Directive { .. } => {
                next_line != line && !self.line_continues(&ev, line)
            }
            _ => false,
        };
        if line_ends {
            self.end_of_line_r();
        }

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

    /// Whether the source line has no end-of-line `r` because it continues
    /// onto the next line: a trailing `\` that stayed a plain character, or a
    /// line ending in `[p]` (`reference/cpp/core/base/KAGParser.cpp:1415-1429`).
    fn line_continues(&self, ev: &Event, line: usize) -> bool {
        if self.ignore_cr {
            return false;
        }
        let Some(raw) = self.raw_lines.get(line.saturating_sub(1)) else {
            return false;
        };
        if raw.ends_with("[p]") {
            return true;
        }
        raw.ends_with('\\')
            && matches!(
                ev,
                Event::Text { text, .. } | Event::Directive { raw: text, .. }
                    if text.ends_with('\\')
            )
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
        macro_entity: Option<usize>,
        bracket: Bracket,
        line: usize,
        env: &mut Environ<'_>,
    ) -> Result<Option<TagOutput>, String> {
        // --- attribute resolution + cond -------------------------------
        //
        // KAGParserEx evaluates every attribute through `EntryParam`, which
        // first checks the `paramMacros` dictionary and recursively splices
        // the registered `(name, value)` list before resolving `&`/`%`.
        let mut condition = true;
        let mut resolved: Vec<(String, String)> = Vec::new();
        let process_attrs = (!self.recording_macro && self.exclude_level == -1) || name == "elsif";
        // The `*` marker splices the caller's macro arguments at its
        // position. These values were already resolved when the calling tag
        // was processed, so they are inserted verbatim (no `&`/`%` re-eval).
        let inject = macro_entity.is_some() && !self.recording_macro && self.exclude_level == -1;
        let injected: Option<Vec<(String, String)>> = if inject {
            self.macro_args.last().cloned()
        } else {
            None
        };
        let mut pos = 0;
        loop {
            if inject
                && macro_entity == Some(pos)
                && let Some(args) = &injected
            {
                for (k, v) in args {
                    if k != "tagname" {
                        push_param(&mut resolved, k.clone(), v.clone());
                    }
                }
            }
            if pos >= params.len() {
                break;
            }
            let (k, v) = &params[pos];
            let (entity, macroarg, rest) = split_value_prefix(v);
            self.entry_param(
                k,
                rest,
                entity,
                macroarg,
                process_attrs,
                &mut condition,
                &mut resolved,
                env,
            )?;
            pos += 1;
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
                    let (events, cols) = parse_macro_content(&macro_content)?;
                    self.push_macro_args(&resolved);
                    self.splice_stack.push(Splice {
                        events,
                        cols,
                        buffer: macro_content.clone(),
                        event_pos: 0,
                        char_pos: 0,
                        line_r_after,
                    });
                } else {
                    let exp = self.exp_attr(&resolved)?;
                    let text = (env.eval)(&exp)?.to_string_repr().unwrap_or_default();
                    // `escape` defaults to true. The reference converts the
                    // attribute variant with `operator bool`, which for a
                    // string is `AsInteger() != 0` (`tjs2/tjsVariant.cpp:301-307`)
                    // — so `escape=false` and `escape=0` evaluate to false,
                    // while a bare flag (`"true"`) evaluates to true.
                    let escape = match self.attr(&resolved, "escape") {
                        Some(v) => parse_tjs_bool(v),
                        None => true,
                    };
                    let (events, cols, buffer) = if escape {
                        let buffer = text.clone();
                        (vec![Event::Text { text, line }], vec![0], buffer)
                    } else {
                        let (events, cols) = parse_inline_events(&text, line)?;
                        (events, cols, text)
                    };
                    self.splice_stack.push(Splice {
                        events,
                        cols,
                        buffer,
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
                // `onJump` may veto the jump (`KAGParser.cpp:1897-1914`).
                let process = match env.fire_jump.as_deref_mut() {
                    Some(fire) => fire("jump", &resolved),
                    None => true,
                };
                if process {
                    self.go_to_storage_and_label(storage, target, env)?;
                }
                return Ok(None);
            }
            if special == Some(Special::Call) {
                let storage = self.attr(&resolved, "storage");
                let target = self.attr(&resolved, "target");
                // `onCall` may veto the call (`KAGParser.cpp:1922-1941`).
                let process = match env.fire_call.as_deref_mut() {
                    Some(fire) => fire("call", &resolved),
                    None => true,
                };
                if process {
                    self.push_call_stack();
                    self.go_to_storage_and_label(storage, target, env)?;
                }
                return Ok(None);
            }
            if special == Some(Special::Return) {
                let storage = self.attr(&resolved, "storage");
                let target = self.attr(&resolved, "target");
                // `onReturn` may veto the return (`KAGParser.cpp:1949-1968`).
                let process = match env.fire_return.as_deref_mut() {
                    Some(fire) => fire("return", &resolved),
                    None => true,
                };
                if process {
                    self.pop_call_stack(storage, target, env)?;
                }
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
            if special == Some(Special::PMacro) {
                // `@pmacro name=<macro> p1=v1 p2=v2 ...` stores the
                // remaining (already resolved / spliced) parameters as the
                // macro's parameter list, excluding `name`.
                let name = self.attr(&resolved, "name").unwrap_or_default().to_string();
                let list: Vec<(String, String)> = resolved
                    .iter()
                    .filter(|(k, _)| k != "name")
                    .cloned()
                    .collect();
                self.param_macros.insert(name, list);
                return Ok(None);
            }
            if special == Some(Special::ErasePMacro) {
                let name = self.attr(&resolved, "name").unwrap_or_default();
                if self.param_macros.remove(name).is_none() {
                    return Err(format!("Unknown macro \"{name}\""));
                }
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// The reference `EntryParam`: look the attribute up in `paramMacros`
    /// and, when found, recursively splice each registered `(name, value)`
    /// pair (resolving a leading `&`/`%` at run time); otherwise resolve the
    /// value and append it to `resolved` (skipping `cond`, which is
    /// consumed). Returns whether the attribute was stored.
    #[allow(clippy::too_many_arguments)]
    fn entry_param(
        &self,
        attribname: &str,
        value: &str,
        entity: bool,
        macroarg: bool,
        process_attrs: bool,
        condition: &mut bool,
        resolved: &mut Vec<(String, String)>,
        env: &mut Environ<'_>,
    ) -> Result<bool, String> {
        if process_attrs && let Some(macro_list) = self.param_macros.get(attribname) {
            for (name, param) in macro_list {
                let (entity, macroarg, rest) = split_value_prefix(param);
                self.entry_param(name, rest, entity, macroarg, true, condition, resolved, env)?;
            }
            return Ok(false);
        }

        let mut value_opt: Option<String> = Some(value.to_string());
        if process_attrs {
            if entity {
                value_opt = (env.eval)(value)?.to_string_repr();
            } else if macroarg {
                value_opt = self.resolve_macro_arg(value);
            }
        }
        if attribname == "cond" {
            if process_attrs {
                let cond_str = value_opt.unwrap_or_default();
                *condition = (env.eval)(&cond_str)?.truthy();
            }
            return Ok(false);
        }
        if let Some(value) = value_opt {
            push_param(resolved, attribname.to_string(), value);
        }
        Ok(true)
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
    /// top macro-args list (reference attribute processing). `None` means
    /// "no value" (the attribute is omitted).
    fn resolve_macro_arg(&self, rest: &str) -> Option<String> {
        if self.macro_args.is_empty() {
            // No macro arguments: the value is kept as-is (without `%`).
            return Some(rest.to_string());
        }
        let top = self.macro_args.last().expect("checked non-empty");
        // Later entries win (the reference writes them into a dictionary
        // in order).
        let lookup = |name: &str| {
            top.iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        match rest.split_once('|') {
            Some((name, default)) => lookup(name).or_else(|| Some(default.to_string())),
            None => lookup(rest),
        }
    }

    fn push_macro_args(&mut self, params: &[(String, String)]) {
        self.macro_args.push(params.to_vec());
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

    fn record_tag(&mut self, name: &str, params: &[(String, String)], macro_entity: Option<usize>) {
        self.recording_macro_str
            .push_str(&reconstruct_tag(name, params, macro_entity));
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
            cur_pos: self.cur_pos,
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
            self.cur_pos = data.cur_pos;
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
        v.push(("curPos".into(), self.cur_pos.to_string()));
        v.push(("lineBuffer".into(), self.line_buffer().to_string()));
        v.push(("lineBufferUsing".into(), bool_str(self.line_buffer_using())));
        v.push(("pendingRs".into(), self.pending_rs.to_string()));
        v.push(("ignoreCR".into(), bool_str(self.ignore_cr)));
        v.push((
            "processSpecialTags".into(),
            bool_str(self.process_special_tags),
        ));
        v.push((
            "multiLineTagEnabled".into(),
            bool_str(self.multi_line_tag_enabled),
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
        v.push((
            "paramMacrosCount".into(),
            self.param_macros.len().to_string(),
        ));
        for (i, (name, list)) in self.param_macros.iter().enumerate() {
            v.push((format!("paramMacros.{i}.name"), name.clone()));
            v.push((format!("paramMacros.{i}.count"), list.len().to_string()));
            for (j, (k, val)) in list.iter().enumerate() {
                v.push((format!("paramMacros.{i}.{j}.name"), k.clone()));
                v.push((format!("paramMacros.{i}.{j}.value"), val.clone()));
            }
        }
        v.push(("callStackCount".into(), self.call_stack.len().to_string()));
        for (i, e) in self.call_stack.iter().enumerate() {
            v.push((format!("callStack.{i}.storage"), e.storage.clone()));
            v.push((format!("callStack.{i}.label"), e.label.clone()));
            v.push((format!("callStack.{i}.offset"), e.offset.to_string()));
            v.push((format!("callStack.{i}.orgSig"), e.org_sig.clone()));
            v.push((format!("callStack.{i}.charIndex"), e.char_index.to_string()));
            v.push((format!("callStack.{i}.curPos"), e.cur_pos.to_string()));
            v.push((
                format!("callStack.{i}.lineBuffer"),
                e.splice
                    .last()
                    .map(|s| s.buffer.clone())
                    .unwrap_or_default(),
            ));
            v.push((
                format!("callStack.{i}.lineBufferUsing"),
                bool_str(!e.splice.is_empty()),
            ));
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
        self.restore_decoded(kvp::decode_dict(s), env)
    }

    /// The body of [`Self::restore`] over already-decoded ordered entries;
    /// the native `restore(dict)` uses this with entries converted from a
    /// reference-format dictionary (`KAGParser.cpp:723-...`).
    pub fn restore_decoded(
        &mut self,
        entries: Vec<(String, String)>,
        env: &mut Environ<'_>,
    ) -> Result<(), String> {
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

        // Parser options must be in place *before* `load_scenario`, because
        // `multi_line_tag_enabled` changes how the scenario text is parsed.
        self.ignore_cr = get_bool("ignoreCR");
        self.process_special_tags = get_bool("processSpecialTags");
        self.multi_line_tag_enabled = get_bool("multiLineTagEnabled");
        self.interrupted = get_bool("interrupted");
        if let Some(d) = get_int("debugLevel") {
            self.debug_level = d as i32;
        }

        self.clear();
        if !storage.is_empty() {
            self.load_scenario(&storage, env)?;
        }
        if !cur_label.is_empty() {
            self.go_to_label(&cur_label)?;
        }

        // flags (the rest are position/state restored below)
        if let Some(i) = get_int("eventIndex") {
            self.event_index = i as usize;
        }
        if let Some(c) = get_int("charIndex") {
            self.char_index = c as usize;
        }
        if let Some(l) = get_int("curLine") {
            self.cur_line = l as usize;
        }
        if let Some(p) = get_int("curPos") {
            self.cur_pos = p as usize;
        }
        if get_bool("lineBufferUsing")
            && let Some(buffer) = get("lineBuffer")
            && !buffer.is_empty()
        {
            let (events, cols) = parse_macro_content(buffer)?;
            self.splice_stack.push(Splice {
                events,
                cols,
                buffer: buffer.to_string(),
                event_pos: 0,
                char_pos: 0,
                line_r_after: false,
            });
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
        // single ordered pass pairs them up; the argument name is the *value*
        // of the `.name` entry — `store` writes
        // `macroArgs.{i}.{j}.name = <arg name>` at `state.rs:1306`)
        self.macro_args.clear();
        if let Some(v) = get_int("macroArgStackDepth") {
            let depth = v as usize;
            for i in 0..depth {
                let prefix = format!("macroArgs.{i}.");
                let mut list: Vec<(String, String)> = Vec::new();
                let mut pending_name: Option<String> = None;
                for (k, val) in &entries {
                    if let Some(rest) = k.strip_prefix(&prefix) {
                        if rest.ends_with(".name") {
                            pending_name = Some(val.clone());
                        } else if rest.ends_with(".value")
                            && let Some(name) = pending_name.take()
                        {
                            list.push((name, val.clone()));
                        }
                    }
                }
                self.macro_args.push(list);
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
            // `store` writes `macrosCount` together with the `macros.`
            // entries, so a mismatch means the save is corrupt.
            if self.macros.len() != n {
                return Err(format!(
                    "KAGParser.restore: macrosCount is {n} but {} macros were read",
                    self.macros.len()
                ));
            }
        }

        // parameter macros
        self.param_macros.clear();
        if let Some(n) = get_int("paramMacrosCount") {
            let n = n as usize;
            for i in 0..n {
                let name = get(&format!("paramMacros.{i}.name"))
                    .unwrap_or("")
                    .to_string();
                let count = get_int(&format!("paramMacros.{i}.count")).unwrap_or(0) as usize;
                let mut list = Vec::with_capacity(count);
                for j in 0..count {
                    let k = get(&format!("paramMacros.{i}.{j}.name"))
                        .unwrap_or("")
                        .to_string();
                    let val = get(&format!("paramMacros.{i}.{j}.value"))
                        .unwrap_or("")
                        .to_string();
                    list.push((k, val));
                }
                self.param_macros.insert(name, list);
            }
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
                    cur_pos: get_int(&format!("{p}curPos")).unwrap_or(0) as usize,
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
                    if let Some(v) = get(&format!("{p}splice.{j}")) {
                        e.splice.push(decode_splice(v)?);
                    }
                }
                if e.splice.is_empty()
                    && get_bool(&format!("{p}lineBufferUsing"))
                    && let Some(buffer) = get(&format!("{p}lineBuffer"))
                    && !buffer.is_empty()
                {
                    let (events, cols) = parse_macro_content(buffer)?;
                    e.splice.push(Splice {
                        events,
                        cols,
                        buffer: buffer.to_string(),
                        event_pos: 0,
                        char_pos: 0,
                        line_r_after: false,
                    });
                }
                self.call_stack.push(e);
            }
        }

        // splice stack
        self.splice_stack.clear();
        if let Some(n) = get_int("spliceCount") {
            let n = n as usize;
            for j in 0..n {
                if let Some(v) = get(&format!("splice.{j}")) {
                    self.splice_stack.push(decode_splice(v)?);
                }
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------------
    // property-style accessors (methods, since the ABI has no instance
    // properties)
    // -------------------------------------------------------------------

    pub fn cur_line(&self) -> usize {
        self.cur_line
    }

    pub fn cur_pos(&self) -> usize {
        self.cur_pos
    }

    pub fn cur_line_str(&self) -> &str {
        // While an expansion is active the reference points `CurLineStr`
        // at `LineBuffer` (`LineBufferUsing`); otherwise it is the current
        // source line.
        if let Some(top) = self.splice_stack.last() {
            return &top.buffer;
        }
        self.raw_lines
            .get(self.cur_line)
            .map(String::as_str)
            .unwrap_or_default()
    }

    /// The current expansion buffer (`LineBuffer`), or `""`.
    pub fn line_buffer(&self) -> &str {
        self.splice_stack
            .last()
            .map(|s| s.buffer.as_str())
            .unwrap_or("")
    }

    /// Whether an expansion buffer is active (`LineBufferUsing`).
    pub fn line_buffer_using(&self) -> bool {
        !self.splice_stack.is_empty()
    }

    pub fn storage_name(&self) -> &str {
        &self.storage_name
    }

    pub fn storage_short_name(&self) -> &str {
        &self.storage_short_name
    }

    pub fn exclude_level(&self) -> i32 {
        self.exclude_level
    }

    pub fn if_level(&self) -> i32 {
        self.if_level
    }

    pub fn exclude_level_stack(&self) -> &[i32] {
        &self.exclude_level_stack
    }

    pub fn if_level_executed_stack(&self) -> &[bool] {
        &self.if_level_executed_stack
    }

    pub fn macro_args_base(&self) -> usize {
        self.macro_args_base
    }

    pub fn macro_args_depth(&self) -> usize {
        self.macro_args.len()
    }

    /// The KAGParserEx `paramMacros` dictionary content: macro name → ordered
    /// `(parameter, value)` list (`readme.txt:51-60`).
    pub fn param_macros_entries(&self) -> Vec<(String, Vec<(String, String)>)> {
        self.param_macros
            .iter()
            .map(|(name, list)| (name.clone(), list.clone()))
            .collect()
    }

    /// Replace the whole `paramMacros` dictionary (the native property
    /// setter).
    pub fn set_param_macros(&mut self, entries: Vec<(String, Vec<(String, String)>)>) {
        self.param_macros = entries.into_iter().collect();
    }

    /// Every macro-argument level as ordered `(name, value)` lists (the
    /// reference `MacroArgs` vector of dictionaries).
    pub fn macro_args_entries(&self) -> &[Vec<(String, String)>] {
        &self.macro_args
    }

    /// The call stack in the reference `tCallStackData` field set
    /// (`KAGParser.cpp:595-634`) for the native `store()` dictionary.
    pub fn call_stack_snapshots(&self) -> Vec<CallStackSnapshot> {
        self.call_stack
            .iter()
            .map(|entry| CallStackSnapshot {
                storage: entry.storage.clone(),
                label: entry.label.clone(),
                offset: entry.offset,
                // `tCallStackData::OrgLineStr` ↔ our original line string.
                org_line_str: entry.org_sig.clone(),
                // `tCallStackData::Pos` ↔ the scan column at the call
                // site.
                pos: entry.cur_pos,
                line_buffer: entry
                    .splice
                    .last()
                    .map(|s| s.buffer.clone())
                    .unwrap_or_default(),
                line_buffer_using: !entry.splice.is_empty(),
                macro_arg_stack_base: entry.macro_args_base,
                macro_arg_stack_depth: entry.macro_args_depth,
                exclude_level: entry.exclude_level,
                if_level: entry.if_level,
                exclude_level_stack: encode_int_stack(&entry.exclude_level_stack),
                if_level_executed_stack: encode_bool_stack(&entry.if_level_executed_stack),
            })
            .collect()
    }

    pub fn cur_label(&self) -> &str {
        &self.cur_label
    }

    pub fn call_stack_depth(&self) -> usize {
        self.call_stack.len()
    }

    /// The macros dictionary as an ordered `Vec<(name, body)>` (for
    /// building a real TJS Dictionary result).
    pub fn macros_entries(&self) -> Vec<(String, String)> {
        self.macros
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Replace the macro table from the live `macros` dictionary (the
    /// reference keeps one dictionary that scripts and `@macro`/`@erasemacro`
    /// share, `KAGParser.cpp:2471-2481`).
    pub fn set_macros(&mut self, entries: Vec<(String, String)>) {
        self.macros = entries.into_iter().collect();
    }

    /// The top macro-args dictionary (`macroParams`/`mp`) as ordered
    /// `Vec<(name, value)>`, or `None` when no macro arguments are on the
    /// stack (the reference then reports `null`, `KAGParser.cpp:2191-2196` +
    /// `2499-2501`).
    pub fn macro_params_entries(&self) -> Option<&[(String, String)]> {
        self.macro_args.last().map(Vec::as_slice)
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

/// The message the walk raises for a deferred parser error
/// (`reference/cpp/core/base/KAGParser.cpp`).
fn deferred_error_message(error: &super::parser::Error) -> String {
    match error {
        super::parser::Error::EmptyScenario => MSG_NO_LINE.to_string(),
        super::parser::Error::CannotOmitFirstLabelName => MSG_CANNOT_OMIT_FIRST_LABEL.to_string(),
        super::parser::Error::InlineScriptNotEnd => MSG_INLINE_SCRIPT_NOT_END.to_string(),
        e => format!("{MSG_SYNTAX} ({e})"),
    }
}

/// Reconstruct `[name key=value ...]` from parsed events for macro
/// recording; values that would not re-parse as-is are double-quoted with
/// backtick escapes.
fn reconstruct_tag(name: &str, params: &[(String, String)], macro_entity: Option<usize>) -> String {
    let mut s = String::with_capacity(16);
    s.push('[');
    s.push_str(name);
    for (i, (k, v)) in params.iter().enumerate() {
        if macro_entity == Some(i) {
            s.push_str(" *");
        }
        s.push(' ');
        s.push_str(k);
        s.push('=');
        s.push_str(&quote_value(v));
    }
    if macro_entity == Some(params.len()) {
        s.push_str(" *");
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
/// spliced buffer; we pre-parse once). Returns the events and their scan
/// columns.
fn parse_macro_content(content: &str) -> Result<(Vec<Event>, Vec<usize>), String> {
    super::parser::parse(content)
        .map(|s| (s.events, s.cols))
        .map_err(|e| format!("{MSG_SYNTAX} ({e})"))
}

/// Split a leading `&` (entity) or `%` (macro argument) prefix from a
/// value. A backtick marker kept by [`super::parser::parse`] suppresses the prefix so
/// the value stays literal (`&foo` / `%foo`).
///
/// The reference consumes the first `&`/`%` while scanning the value and then
/// checks the *remaining* first character once more during the unescape pass
/// (`KAGParser.cpp:2067-2078`, `:2111-2117`). The two steps are order
/// dependent: `%&expr` consumes `%` then `&` (entity wins) while `%%expr`
/// keeps the second `%` in the value.
fn split_value_prefix(s: &str) -> (bool, bool, &str) {
    if let Some(rest) = s.strip_prefix('`')
        && (rest.starts_with('&') || rest.starts_with('%'))
    {
        return (false, false, rest);
    }
    let mut rest = s;
    let mut entity = false;
    let mut macroarg = false;
    if let Some(r) = rest.strip_prefix('&') {
        entity = true;
        rest = r;
    } else if let Some(r) = rest.strip_prefix('%') {
        macroarg = true;
        rest = r;
    }
    if !entity && let Some(r) = rest.strip_prefix('&') {
        entity = true;
        rest = r;
    }
    if !macroarg && let Some(r) = rest.strip_prefix('%') {
        macroarg = true;
        rest = r;
    }
    // A marker backtick that followed the prefix just consumed keeps an
    // escaped `&`/`%` literal rather than treating it as a second prefix.
    if let Some(r) = rest.strip_prefix('`')
        && (r.starts_with('&') || r.starts_with('%'))
    {
        rest = r;
    }
    (entity, macroarg, rest)
}

/// Store one resolved attribute the way `DicObj->PropSetByVS(
/// TJS_MEMBERENSURE, …)` does: a repeated name overwrites in place, so the
/// first occurrence keeps its position and the last value wins
/// (`reference/cpp/core/base/KAGParser.cpp:2176-2180`).
fn push_param(resolved: &mut Vec<(String, String)>, key: String, value: String) {
    if let Some(slot) = resolved
        .iter_mut()
        .find(|(k, _)| k.as_str() == key.as_str())
    {
        slot.1 = value;
    } else {
        resolved.push((key, value));
    }
}

/// Convert an attribute value to a boolean the way `tTJSVariant::operator
/// bool` does for a string: `AsInteger() != 0` (`tjs2/tjsVariant.cpp:301-307`).
/// `escape=false`/`escape=0` and `escape=0.5` are false, `escape=1` is true,
/// and a bare flag (the `"true"` a missing value becomes,
/// `KAGParser.cpp:2057-2058`) is true.
fn parse_tjs_bool(v: &str) -> bool {
    super::tjsnum::tjs_string_to_integer(v) != 0
}

/// Parse an `emb escape=false` result as an inline tag stream. The
/// reference splices the raw text into the current line buffer, where
/// `LineBufferUsing` keeps a leading `@` from starting a line command and
/// `[` starts a real tag. We reproduce the `@` rule by parsing with a
/// sentinel prefix and stripping it from the first text event. Returns the
/// events and their scan columns (relative to the buffer).
fn parse_inline_events(text: &str, line: usize) -> Result<(Vec<Event>, Vec<usize>), String> {
    if text.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut src = String::with_capacity(text.len() + 1);
    src.push('\u{0}');
    src.push_str(text);
    let scenario = super::parser::parse(&src).map_err(|e| format!("{MSG_SYNTAX} ({e})"))?;
    let mut events = scenario.events;
    let cols = scenario.cols;
    if let Some(Event::Text { text: first, .. }) = events.first_mut()
        && let Some(rest) = first.strip_prefix('\u{0}')
    {
        *first = rest.to_string();
    }
    for e in &mut events {
        match e {
            Event::Label { line: l, .. }
            | Event::Text { line: l, .. }
            | Event::Tag { line: l, .. }
            | Event::Directive { line: l, .. }
            | Event::Comment { line: l, .. }
            | Event::Script { line: l, .. } => *l = line,
        }
    }
    // The sentinel occupies column 0, so shift every recorded column back
    // so the buffer's scan columns match `LineBuffer`.
    let mut kept_events = Vec::with_capacity(events.len());
    let mut kept_cols = Vec::with_capacity(cols.len());
    for (event, col) in events.into_iter().zip(cols) {
        if matches!(&event, Event::Text { text, .. } if text.is_empty()) {
            continue;
        }
        kept_events.push(event);
        kept_cols.push(col.saturating_sub(1));
    }
    Ok((kept_events, kept_cols))
}

fn bool_str(b: bool) -> String {
    if b { "1".to_string() } else { "0".to_string() }
}

/// Hex-encode an int stack like the reference `StoreIntStackToDic`
/// (`KAGParser.cpp:637-659`).
pub fn encode_int_stack(stack: &[i32]) -> String {
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
/// Encode a bool stack like the reference `StoreBoolStackToDic`
/// (`KAGParser.cpp:704-722`).
pub fn encode_bool_stack(stack: &[bool]) -> String {
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
                macro_entity,
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
                out.push(':');
                match macro_entity {
                    Some(idx) => out.push_str(&idx.to_string()),
                    None => out.push('-'),
                }
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
            Event::Script {
                text, start_line, ..
            } => {
                out.push('X');
                out.push(':');
                out.push_str(&start_line.to_string());
                out.push(':');
                out.push_str(&escape_event_field(text));
            }
        }
    }
    out
}

/// Decode a splice encoding (see [`encode_splice`]).
fn decode_splice(s: &str) -> Result<Splice, String> {
    let (kind, rest) = s
        .split_once(':')
        .ok_or_else(|| "splice: missing kind".to_string())?;
    if kind != "K" {
        return Err(format!("splice: unknown kind '{kind}'"));
    }
    let (nums, content) = rest
        .split_once(':')
        .ok_or_else(|| "splice: missing counts".to_string())?;
    let mut nums = nums.split(',');
    let event_pos: usize = nums
        .next()
        .ok_or_else(|| "splice: missing event position".to_string())?
        .parse()
        .map_err(|_| "splice: invalid event position".to_string())?;
    let char_pos: usize = nums
        .next()
        .ok_or_else(|| "splice: missing char position".to_string())?
        .parse()
        .map_err(|_| "splice: invalid char position".to_string())?;
    let line_r_after = nums.next() == Some("1");
    let events = decode_events(content)?;
    let cols = vec![0; events.len()];
    Ok(Splice {
        events,
        cols,
        buffer: String::new(),
        event_pos,
        char_pos,
        line_r_after,
    })
}

/// Decode the event list produced by [`encode_events`].
fn decode_events(s: &str) -> Result<Vec<Event>, String> {
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
            'X' => {
                let start_line: usize = field
                    .parse()
                    .map_err(|_| format!("event: invalid start line '{field}'"))?;
                let text = if i < chars.len() && chars[i] == ':' {
                    i += 1;
                    let (t, n) = read_field(&chars, i);
                    i = n;
                    t
                } else {
                    String::new()
                };
                events.push(Event::Script {
                    text,
                    start_line,
                    line: 0,
                });
            }
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
                let count: usize = count_str
                    .parse()
                    .map_err(|_| format!("event: invalid parameter count '{count_str}'"))?;
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
                let (marker, n3b) = if i < chars.len() && chars[i] == ':' {
                    i += 1;
                    read_field(&chars, i)
                } else {
                    (String::new(), i)
                };
                i = n3b;
                let macro_entity = match marker.as_str() {
                    "" | "-" => None,
                    s => Some(
                        s.parse()
                            .map_err(|_| format!("event: invalid macro entity '{s}'"))?,
                    ),
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
                    macro_entity,
                    bracket,
                    line: 0,
                });
            }
            _ => break,
        }
    }
    Ok(events)
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
mod tests {
    use super::{Environ, EvalResult, KagParserState, parse_tjs_bool};

    /// The reference consumes the first `&`/`%` while scanning the value and
    /// then checks the remaining first character again during unescaping
    /// (`reference/cpp/core/base/KAGParser.cpp:2067-2078`, `:2111-2117`), so
    /// the two steps are order dependent.
    #[test]
    fn value_prefixes_follow_the_reference_two_step_order() {
        assert_eq!(super::split_value_prefix("&expr"), (true, false, "expr"));
        assert_eq!(super::split_value_prefix("%expr"), (false, true, "expr"));
        assert_eq!(super::split_value_prefix("&%expr"), (true, true, "expr"));
        assert_eq!(super::split_value_prefix("%&expr"), (true, true, "expr"));
        assert_eq!(super::split_value_prefix("&&expr"), (true, false, "&expr"));
        assert_eq!(super::split_value_prefix("%%expr"), (false, true, "%expr"));
        // A backtick-escaped prefix stays literal.
        assert_eq!(
            super::split_value_prefix("`&%expr"),
            (false, false, "&%expr")
        );
    }

    /// `tTJSVariant::operator bool` on a string is `AsInteger() != 0`
    /// (`reference/cpp/core/tjs2/tjsVariant.h:913-925`,
    /// `reference/cpp/core/tjs2/tjsVariant.cpp:301-307`): `"0.5"` is false and
    /// a bare flag (`"true"`, `reference/cpp/core/base/KAGParser.cpp:2057-2058`)
    /// is true.
    #[test]
    fn parse_tjs_bool_uses_as_integer() {
        assert!(!parse_tjs_bool("0.5"));
        assert!(!parse_tjs_bool("0"));
        assert!(!parse_tjs_bool("false"));
        assert!(!parse_tjs_bool(""));
        assert!(parse_tjs_bool("1"));
        assert!(parse_tjs_bool("true"));
    }

    /// A `macrosCount` that disagrees with the serialized `macros.` entries
    /// marks a corrupt save, which `restore_decoded` rejects instead of
    /// silently accepting.
    #[test]
    fn restore_rejects_a_mismatched_macros_count() {
        let mut state = KagParserState::default();
        let mut eval = |_e: &str| Ok(EvalResult::Void);
        let mut load = |_name: &str| Ok(String::new());
        let mut env = Environ {
            eval: &mut eval,
            load_storage: &mut load,
            fire_label: None,
            fire_script: None,
            fire_jump: None,
            fire_call: None,
            fire_return: None,
            fire_scenario_load: None,
            fire_scenario_loaded: None,
        };
        let entries = vec![
            ("macrosCount".to_string(), "2".to_string()),
            ("macros.foo".to_string(), "body".to_string()),
        ];
        let error = state.restore_decoded(entries, &mut env).unwrap_err();
        assert!(error.contains("macrosCount"), "{error}");
    }
}

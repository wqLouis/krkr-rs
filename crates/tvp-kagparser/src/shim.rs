//! The script-side wrapper for the `KAGParser` native class.
//!
//! The native `KAGParser` now returns *real* TJS dictionaries (see
//! [`crate`]), so `getNextTag()` produces `%["tagname":"ch","text":"H"]`
//! directly and `getMacros`/`macros` return a live `Dictionary`. The only
//! thing an ABI string-encoding could not do was receive an *object*
//! argument, but the object-return side (which the game uses) works.
//!
//! This module keeps an optional `KAGParserCompat` **delegating** wrapper
//! class that exposes the reference's property-style surface (`ignoreCR`,
//! `processSpecialTags`, `curLine`, ...) as TJS properties and forwards the
//! dict-returning methods straight through. The shipped game does **not**
//! use this wrapper — it subclasses `KAGParser` directly via
//! `super.KAGParser()` (now supported) — so this is purely a convenience /
//! test surface.
//!
//! # Usage
//!
//! ```tjs
//! Scripts.exec(installKAGParserWrapper());
//! var p = new KAGParserCompat();
//! p.ignoreCR = true;
//! p.processSpecialTags = true;
//! p.loadScenario("scenario/01_01.ks");
//! var tag = p.getNextTag();   // a dictionary: %["tagname":"ch","text":"H"]
//! if (tag === void) { /* end of scenario */ }
//! ```

/// TJS code installing the `KAGParserCompat` wrapper class on the global
/// object.
pub const INSTALL_WRAPPER: &str = r#"// KAGParserCompat property wrapper (krkr-rs).
// The native returns real TJS dictionaries for object-shaped results, so
// the wrapper forwards them directly — no string decoding is needed.
class KAGParserCompat {
	var _p;
	function KAGParserCompat(){
		_p = new KAGParser();
	}
	function getNextTag(){ return _p.getNextTag(); }
	function loadScenario(n){ return _p.loadScenario(n); }
	function goToLabel(n){ return _p.goToLabel(n); }
	function callLabel(n){ return _p.callLabel(n); }
	function clear(){ return _p.clear(); }
	function store(){ return _p.store(); }
	function restore(s){ return _p.restore(s); }
	function clearCallStack(){ return _p.clearCallStack(); }
	function popMacroArgs(){ return _p.popMacroArgs(); }
	function interrupt(){ return _p.interrupt(); }
	function resetInterrupt(){ return _p.resetInterrupt(); }
	function getCurLine(){ return _p.getCurLine(); }
	function getCurPos(){ return _p.getCurPos(); }
	function getCurLineStr(){ return _p.getCurLineStr(); }
	function getCallStackDepth(){ return _p.getCallStackDepth(); }
	function getCurStorage(){ return _p.getCurStorage(); }
	function setCurStorage(n){ return _p.setCurStorage(n); }
	function getCurLabel(){ return _p.getCurLabel(); }
	function getMacros(){ return _p.getMacros(); }
	function setMacros(d){ return _p.setMacros(d); }
	function getMacroParams(){ return _p.getMacroParams(); }
	function getMP(){ return _p.getMP(); }
	function getDebugLevel(){ return _p.getDebugLevel(); }
	function setDebugLevel(v){ return _p.setDebugLevel(v); }
	function getMultiLineTagEnabled(){ return _p.getMultiLineTagEnabled(); }
	function setMultiLineTagEnabled(v){ return _p.setMultiLineTagEnabled(v); }
	property ignoreCR {
		getter{ return _p.getIgnoreCR(); }
		setter(v){ return _p.setIgnoreCR(v); }
	}
	property processSpecialTags {
		getter{ return _p.getProcessSpecialTags(); }
		setter(v){ return _p.setProcessSpecialTags(v); }
	}
	property multiLineTagEnabled {
		getter{ return _p.getMultiLineTagEnabled(); }
		setter(v){ return _p.setMultiLineTagEnabled(v); }
	}
	property debugLevel {
		getter{ return _p.getDebugLevel(); }
		setter(v){ return _p.setDebugLevel(v); }
	}
	property curLine { getter{ return _p.getCurLine(); } }
	property curPos { getter{ return _p.getCurPos(); } }
	property curLineStr { getter{ return _p.getCurLineStr(); } }
	property macros {
		getter{ return _p.getMacros(); }
		setter(v){ return _p.setMacros(v); }
	}
	property macroParams { getter{ return _p.getMacroParams(); } }
	property mp { getter{ return _p.getMP(); } }
	property callStackDepth { getter{ return _p.getCallStackDepth(); } }
	property curStorage {
		getter{ return _p.getCurStorage(); }
		setter(v){ return _p.setCurStorage(v); }
	}
	property curLabel { getter{ return _p.getCurLabel(); } }
}
"#;

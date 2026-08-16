//! The script-side wrapper for the `KAGParser` native class.
//!
//! The tjs2-sys C ABI can only marshal void/int/real/string values across
//! the native boundary, so the native returns *strings* where the
//! reference returns TJS dictionaries (see [`crate`]). This module embeds
//! the TJS code that decodes those strings back into dictionaries and
//! wraps the native in a class exposing the reference-shaped surface:
//! `getNextTag()` returning a dictionary, and the property-style members
//! (`ignoreCR`, `processSpecialTags`, `macros`, `curLine`, ...).
//!
//! # Usage
//!
//! ```tjs
//! Scripts.exec(installKAGParserWrapper());
//! var p = new KAGParserCompat();
//! p.ignoreCR = true;
//! p.processSpecialTags = true;
//! p.loadScenario("scenario/01_01.ks");
//! var tag = p.getNextTag();   // a dictionary, e.g. %["tagname":"ch","text":"H"]
//! if (tag === void) { /* end of scenario */ }
//! ```
//!
//! The wrapper is a *delegating* class, not a subclass: the current C ABI
//! cannot create the native instance for `class X extends KAGParser`
//! objects (there is no constructor member that would run
//! `super.KAGParser()`), so `KAGParserCompat` owns a private `KAGParser`
//! and forwards every call.

/// TJS code installing the parser helpers and the `KAGParserCompat`
/// wrapper class on the global object.
pub const INSTALL_WRAPPER: &str = r#"// KAGParser string-encoding helpers + property wrapper (krkr-rs).
function parseKAGField(s){
	var r = "";
	for(var i = 0; i < s.length; i++){
		var c = s.charAt(i);
		if(c == "\\" && i + 1 < s.length){
			i++;
			var n = s.charAt(i);
			if(n == "n") r += "\n";
			else if(n == "r") r += "\r";
			else r += n;
		}else{
			r += c;
		}
	}
	return r;
}
function encodeKAGField(s){
	var r = "";
	for(var i = 0; i < s.length; i++){
		var c = s.charAt(i);
		if(c == "\\") r += "\\\\";
		else if(c == "\n") r += "\\n";
		else if(c == "\r") r += "\\r";
		else r += c;
	}
	return r;
}
function parseKAGDict(s){
	var d = new Dictionary();
	if(s === void || s == "") return d;
	var lines = s.split("\n");
	for(var i = 0; i < lines.length; i++){
		var eq = lines[i].indexOf("=");
		d[parseKAGField(lines[i].substr(0, eq))] = parseKAGField(lines[i].substr(eq + 1));
	}
	return d;
}
// NOTE: this TJS2 fork's grammar has NO for-in statement and Dictionary
// exposes no key enumeration, so a script-side Dictionary cannot be
// serialized. The macros surface therefore uses raw strings (see the
// KAGParserCompat property below); the KAGParserEx.dll plugin that real
// games use for dictionary scenarios is stubbed in krkr-rs anyway.
function parseKAGTag(s){
	if(s === void) return void;
	var d = new Dictionary();
	var lines = s.split("\n");
	d.tagname = parseKAGField(lines[0]);
	for(var i = 1; i < lines.length; i++){
		var eq = lines[i].indexOf("=");
		d[parseKAGField(lines[i].substr(0, eq))] = parseKAGField(lines[i].substr(eq + 1));
	}
	return d;
}
class KAGParserCompat {
	var _p;
	function KAGParserCompat(){
		_p = new KAGParser();
	}
	function getNextTag(){ return parseKAGTag(_p.getNextTag()); }
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
	function getMacros(){ return parseKAGDict(_p.getMacros()); }
	function setMacros(d){ return _p.setMacros(encodeKAGDict(d)); }
	function getMacroParams(){ return parseKAGDict(_p.getMacroParams()); }
	function getMP(){ return parseKAGDict(_p.getMP()); }
	function getDebugLevel(){ return _p.getDebugLevel(); }
	function setDebugLevel(v){ return _p.setDebugLevel(v); }
	property ignoreCR {
		getter{ return _p.getIgnoreCR(); }
		setter(v){ return _p.setIgnoreCR(v); }
	}
	property processSpecialTags {
		getter{ return _p.getProcessSpecialTags(); }
		setter(v){ return _p.setProcessSpecialTags(v); }
	}
	property debugLevel {
		getter{ return _p.getDebugLevel(); }
		setter(v){ return _p.setDebugLevel(v); }
	}
	property curLine { getter{ return _p.getCurLine(); } }
	property curPos { getter{ return _p.getCurPos(); } }
	property curLineStr { getter{ return _p.getCurLineStr(); } }
	// macros/macroParams are raw "k=v\n..." strings: this TJS2 fork cannot
	// enumerate Dictionary members (no for-in), so object round-trips are
	// unsupported (the game's dictionary scenarios go through the stubbed
	// KAGParserEx.dll plugin).
	property macros {
		getter{ return _p.getMacros(); }
		setter(v){ return _p.setMacros(v); }
	}
	property macroParams { getter{ return _p.getMacroParams(); } }
	property mp { getter{ return parseKAGDict(_p.getMP()); } }
	property callStackDepth { getter{ return _p.getCallStackDepth(); } }
	property curStorage {
		getter{ return _p.getCurStorage(); }
		setter(v){ return _p.setCurStorage(v); }
	}
	property curLabel { getter{ return _p.getCurLabel(); } }
}
"#;

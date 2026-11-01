//! The KAG map layer: the script half of a clickable province map.
//!
//! A KAG runtime owns the map. The tags `[mapimage]`, `[mapaction]` and
//! `[mapdisable]` are the runtime's — the KAG the games ship handles them in
//! `system/mainwindow.tjs` — the action table is the runtime's `.ma` file,
//! and the map layer itself (the action table, `queryProvinceAction` and
//! `processProvince`) is the runtime's `system/graphiclayer.tjs`. The engine
//! half — the province plane, the value a point answers and the click target
//! — is the port's.
//!
//! The script below is that map layer, written the way the KAG writes it, so
//! a test can drive the port's engine half through the runtime's own steps:
//! the province image is loaded into the layer, the action table is read and
//! compiled, and a click turns into the region's action.

use media::{Media, decode_index_plane};

use super::Env;

/// The province image: an 8x8 palettized PNG whose palette index is the
/// region id — 16 in the top-left quadrant, 17 in the bottom-right one, 0
/// outside both.
pub const PROVINCE_PNG: &[u8] = include_bytes!("../fixtures/province_map.png");

/// The action table: the KAG's `.ma` format — one action per line,
/// `<region>: <body>`, the body a TJS fragment that fills the action.
pub const ACTIONS_MA: &str = include_str!("../fixtures/province_map.ma");

/// The map layer: the province context, the map layer and the actions the
/// click and the hover run.
///
/// The three global arrays are the actions' observable side: `mapFired` the
/// clicked region's action, `mapEntered` and `mapLeft` the hover's.
pub const MAP_LAYER_TJS: &str = r#"
var mapFired = [];
var mapEntered = [];
var mapLeft = [];

function mapAction(n) { mapFired.push(n); }
function mapEnter(n) { mapEntered.push(n); }
function mapLeave(n) { mapLeft.push(n); }

class ProvinceContext
{
	var target;
	var storage;
	var onenter;
	var onleave;
	var hint;
	var exp;
	var cursor;
	var countpage;
	var autodisable;

	function ProvinceContext() {};
	function finalize() {};
}

class MapLayer extends Layer
{
	var provinceActions = void;
	var pointingProvince = 0;

	function MapLayer(win, par)
	{
		super.Layer(win, par);
		type = ltCoverRect;
		hitType = htMask;
		hitThreshold = 0;
	}

	// The action table: one action per line, `<region>: <body>`, the body a
	// TJS fragment that fills the action.
	function internalLoadProvinceActions(fn)
	{
		provinceActions = [];
		var file = [];
		file.load(fn);
		var body = "function { var a = provinceActions; \n";
		for(var i = 0; i < file.count; i++)
		{
			var line = file[i];
			if(line == "" || line[0] == ';') continue;
			var colon = line.indexOf(':');
			if(colon == -1) continue;
			body += "a[" + line.substring(0, colon) + "] = function { " +
				line.substring(colon + 1) + " ;};\n";
		}
		body += "}\n";
		(Scripts.eval(body) incontextof this) ();
	}

	function clearProvinceActions()
	{
		provinceActions = void;
		pointingProvince = 0;
	}

	// The region's action, or void when the region has none.
	function queryProvinceAction(n, checkzero = true)
	{
		if(provinceActions === void) return void;
		if(checkzero) { if(n == 0) return void; }
		var action = provinceActions[n];
		if(action === void) return void;
		var ar = new ProvinceContext();
		(action incontextof ar) ();
		if(ar.target === void && ar.storage === void && ar.onenter === void &&
			ar.onleave === void && ar.hint === void && ar.exp === void &&
			ar.cursor === void && ar.countpage === void && ar.autodisable === void)
				return void;
		return ar;
	}

	function onProvinceEnter(n)
	{
		var action = queryProvinceAction(n);
		if(action !== void && action.onenter !== void) Scripts.eval(action.onenter);
	}

	function onProvinceLeave(n)
	{
		var action = queryProvinceAction(n);
		if(action !== void && action.onleave !== void) Scripts.eval(action.onleave);
	}

	function onMouseMove(x, y, shift)
	{
		super.onMouseMove(...);
		if(provinceActions !== void)
		{
			var n = getProvincePixel(x - imageLeft, y - imageTop);
			if(n != pointingProvince)
			{
				if(pointingProvince != 0) onProvinceLeave(pointingProvince);
				if(n != 0) onProvinceEnter(n);
				pointingProvince = n;
			}
		}
	}

	// The click: the point's region, then the region's action.
	function processProvince(x, y)
	{
		var n = getProvincePixel(x - imageLeft, y - imageTop);
		var action = queryProvinceAction(n);
		if(action === void) return false;
		if(action.exp !== void) Scripts.eval(action.exp);
		return true;
	}

	function onMouseDown(x, y, button, shift)
	{
		super.onMouseDown(...);
		if(button == 0) processProvince(x, y);
	}
}
"#;

/// Write the province image and the action table into the harness's game
/// directory under the names the test names them by, and install the
/// index-plane decoder the province load reads.
///
/// The decoder is the real L1 one, so the plane a test reads is the one the
/// game reads: the image's own eight-bit sample, the palette index of a
/// palettized image.
pub fn install(env: &Env, image: &str, table: &str) {
    let dir = env
        .game_dir()
        .expect("the harness mounts a game directory")
        .clone();
    std::fs::write(dir.join(format!("{image}.png")), PROVINCE_PNG)
        .expect("write the province image");
    std::fs::write(dir.join(table), ACTIONS_MA).expect("write the action table");
    let media = Media::new();
    env.install_index_plane_decoder(move |bytes, hint| {
        decode_index_plane(&media, bytes, hint).map_err(|error| error.to_string())
    });
}

/// The window, the background layer and the map layer the map tests drive.
///
/// The map layer is a child of the window's primary layer, the shape the KAG
/// uses for its background layer.
pub const MAP_SETUP_TJS: &str = r#"
var w = new Window();
w.setSize(64, 64);
var base = new Layer(w, null);
base.setPos(0, 0);
base.setSize(64, 64);
base.setImageSize(64, 64);
base.visible = true;
var map = new MapLayer(w, base);
map.setPos(0, 0);
map.setSize(64, 64);
map.setImageSize(64, 64);
map.visible = true;
"#;

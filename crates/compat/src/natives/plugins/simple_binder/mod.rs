//! `simple_binder` — the Rust port of the `SimpleBinder` plugin-authoring
//! helper (`reference/cpp/plugins/simplebinder/`).
//!
//! `simplebinder` is **not a plugin**: the reference directory contains only
//! `simplebinder.hpp`, `simplebinder.txt` and `readme.txt` — a header-only
//! C++ utility library that other plugins `#include` to make their TJS
//! registration terser. `simplebinder.hpp` defines no `NCB_*` macro use, no
//! `TJS_BEGIN_NATIVE_MEMBERS` block and no `TJSCreateNativeClassForPlugin`
//! call; it is `CMakeLists.txt`-listed as an include directory
//! (`target_include_directories(... ${KRKR2PLUGIN_PATH}/simplebinder)`) and
//! never compiled into the plugin static library. The only in-tree consumer is
//! the optional, commented-out `DrawDeviceForSteam` plugin.
//!
//! # Why this is a no-op
//!
//! `BindUtil` exists to attach C++ callbacks to an `iTJSDispatch2` *store*
//! (the global object or an existing class object) at link time:
//!
//! * `Function` / `Property` / `Constant` / `Variant` create the reference's
//!   `tTJSDispatch`-derived stores (`FunctionStore`, `PropertyStore`,
//!   `ConstantStore`) and `PropSet` them onto the store;
//! * `Class` builds a native class through
//!   `TJSCreateNativeClassForPlugin`;
//! * `GetObject` walks a dot-separated object path.
//!
//! The Rust binding surface already provides the equivalents declaratively and
//! without a second registration mechanism: [`CompatCtx::register_class`],
//! [`CompatCtx::register_instance`], [`CompatCtx::register_static_members`]
//! and [`CompatCtx::register_instance_members`]. A Rust port of the C++ header
//! would be a parallel wrapper over those builders and would hide no new
//! behavior — the "don't over-abstract" rule explicitly rejects that.
//!
//! The reference helper registers no script-visible surface, so the plugin
//! module has none to install. This is the same situation as
//! [`emote_player`](crate::natives::plugins::emote_player), which is likewise
//! a documented no-op module that keeps the plugin-name/module mapping 1:1
//! with the reference DLL set.
//!
//! [`CompatCtx::register_class`]: crate::CompatCtx::register_class
//! [`CompatCtx::register_instance`]: crate::CompatCtx::register_instance
//! [`CompatCtx::register_static_members`]: crate::CompatCtx::register_static_members
//! [`CompatCtx::register_instance_members`]: crate::CompatCtx::register_instance_members

// No items: `simplebinder` is a header-only helper, not a plugin, so there is
// no script surface and no registration function to call.

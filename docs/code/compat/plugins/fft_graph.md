# fftgraph (`fftgraph.dll`)

**DLL name:** `fftgraph.dll` (aliases: none) — linked with `Plugins.link("fftgraph.dll")`
**Rust module:** `crates/compat/src/natives/plugins/fft_graph/`
**Reference:** `reference/cpp/plugins/fftgraph.cpp`
**Registers:** one global function — `drawFFTGraph`

The function appears when the script links `fftgraph.dll`: a ported plugin
installs its members on the first `Plugins.link` that names it, not at engine
startup. See the [plugin index](../plugins.md) for the linking model.

## Surface

| member | kind | behaviour |
|---|---|---|
| `drawFFTGraph()` | global function | an empty no-op; returns `void` |

The entire reference module is:

```cpp
#include "ncbind.hpp"

#define NCB_MODULE_NAME TJS_W("fftgraph.dll")

static void InitPlugin() {
    TVPExecuteScript(TJS_W("function drawFFTGraph(){}"));
}

NCB_PRE_REGIST_CALLBACK(InitPlugin);
```

(`reference/cpp/plugins/fftgraph.cpp:5-7`.)

`fftgraph.dll` registers exactly one global function, `drawFFTGraph`, and the
reference body is empty. The port executes the same script, so the
script-visible surface is identical.

## Behaviour

`drawFFTGraph()` is a global function with an empty body, so it returns
`void`. The definition lives on the VM as an ordinary script member, so a game
that later assigns `drawFFTGraph = function(...) {...}` replaces it and the new
binding wins — the stub is meant to be replaced.

`typeof` of any function value is `"Object"`
(`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2614-2616`).

## State and lifetime

No state: the function is a script-level no-op with an empty body.

## Errors

None. Calling it with or without arguments succeeds and returns `void` — TJS
accepts extra arguments to a zero-parameter function.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/misc_plugins.rs` | that `drawFFTGraph` exists as a global, is a function (`typeof` is `"Object"`), returns `void` with and without arguments, and is an ordinary replaceable script member |

## Known gaps

* None. The reference body is empty and the port's body is empty too.

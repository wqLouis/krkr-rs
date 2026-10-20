# fftgraph (`fftgraph.dll`)

**DLL name:** `fftgraph.dll` — linked with `Plugins.link("fftgraph.dll")`
**Rust module:** `crates/compat/src/natives/plugins/fft_graph/`
**Reference:** `reference/cpp/plugins/fftgraph.cpp`
**Registers:** one global function — `drawFFTGraph`

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
reference body is empty. It is the stub the reference `util_graph.dll` (and
some games) replace with their own implementation, so the contract is only
that the *name* exists and is a function.

## Behaviour

The port executes the same script, so the script-visible surface is identical:
a global `drawFFTGraph` function that returns `void`.

The definition lives on the VM, so a game that later assigns
`drawFFTGraph = function(...) {...}` keeps working exactly as it does on the
reference — the stub is an ordinary script member and is replaceable.

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
| `crates/compat/tests/misc_plugins.rs` | that `drawFFTGraph` exists as a global, is a function, returns `void`, and is an ordinary replaceable script member |
| `crates/compat/tests/plugin.rs` | that `Plugins.link("fftgraph.dll")` resolves to the built-in table |

## Known gaps

* None. The reference body is empty and the port's body is empty too.

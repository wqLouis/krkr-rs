# win32dialog (`win32dialog.dll`)

**DLL name:** `win32dialog.dll` — linked with `Plugins.link("win32dialog.dll")`
**Rust module:** `crates/compat/src/natives/plugins/win32_dialog/`
**Reference:** `reference/cpp/plugins/win32dialog.cpp`
**Registers:** the `WIN32Dialog` class

The DLL name is in the built-in table and the linking rules are on the
[plugin index](../plugins.md).

## Surface

| member | kind | behaviour |
|---|---|---|
| `WIN32Dialog()` | constructor | accepts any arguments; the reference TJS class has no constructor body |
| `messageBox(message, caption, type)` | instance method | shows a message box through the emulated `System.inform` and returns the negated result |

The reference's whole registration is a script class
(`reference/cpp/plugins/win32dialog.cpp:6-10`):

```cpp
static void InitPlugin_WIN32Dialog() {
    TVPExecuteScript(TJS_W("class WIN32Dialog {")
        TJS_W("    function messageBox(message, caption, type) {return ")
        TJS_W("!System.inform(message, caption, 2);}")
        TJS_W("}"));
}
NCB_PRE_REGIST_CALLBACK(InitPlugin_WIN32Dialog);
```

The port registers the class as a native instance class rather than by
executing the reference script, so the behaviour is owned by the compat
layer, while the script-visible surface — `new WIN32Dialog().messageBox(...)`
— is the same.

## Behaviour

### The call

`messageBox(message, caption, type)` evaluates
`!System.inform(message, caption, 2)`:

* `System` is resolved **on every call**, so a script override of
  `System.inform` wins;
* `message` and `caption` pass through unchanged — a call that omits them
  forwards `void`, exactly like the script class' parameters
  (`reference/cpp/plugins/win32dialog.cpp:7-9`), and an object argument
  reaches `System.inform` as the very same object;
* `type` is ignored in favour of the literal `2`
  (`reference/cpp/plugins/win32dialog.cpp:9`);
* the result is negated with the reference `operator bool`
  (`reference/cpp/core/tjs2/tjsVariant.h:911-932`), so an override
  returning `"text"`, `7` or `void` is negated exactly as KiriKiri would. A
  string is coerced through `AsInteger() != 0`
  (`reference/cpp/core/tjs2/tjsVariant.h:920-921`,
  `reference/cpp/core/tjs2/tjsVariant.cpp:301-306`).

### The emulated dialog

The Win32 environment is **emulated** rather than refused: the message box
is shown through the already-emulated `System.inform`
(`crates/compat/src/natives/system.rs`), which logs the text and answers
the clicked-button index the reference stores
(`reference/cpp/core/base/impl/SystemImpl.cpp:594-596` for the button-array
form, `:598-604` for the button-count form). `messageBox` always passes the
literal count `2`, so the emulated dialog reports its first button, `0`, and
the negation yields the integer `1` — the member returns the emulated
button result instead of raising an "unsupported" typed error.

### Overrides

Because the reference declares a *script* class, an instance-level
assignment shadows `messageBox` per object, and the shadow wins on the next
call. The port's native class behaves identically, because the override is
found through the object before the class member.

## State and lifetime

The `WIN32Dialog` payload is an empty unit value: the class carries no state
beyond the synthetic dialog behaviour. The payload is allocated when the
object is created and freed when the object is destroyed.

## Errors

| condition | outcome |
|---|---|
| `System` is not an object | typed error: `WIN32Dialog.messageBox: System is not an object` |
| `System` cannot be resolved | typed error: `WIN32Dialog.messageBox: cannot resolve System: …` |
| the `message` argument cannot be forwarded | typed error: `WIN32Dialog.messageBox: cannot forward message: …` |
| the `caption` argument cannot be forwarded | typed error: `WIN32Dialog.messageBox: cannot forward caption: …` |
| `System.inform` raises | typed error: `WIN32Dialog.messageBox: System.inform failed: …` — the reference propagates the same failure as a TJS exception thrown from the script body, never a fabricated button result |
| the compat context is not bound | typed error: `WIN32Dialog.messageBox: compat context is not bound` |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/misc_plugins.rs` | the emulated `System.inform` result and its negation; that the lookup happens on the `System` object at call time and a script override wins; that the raw argument types and an object argument's identity pass through unchanged; that an override which throws surfaces as a typed error; that an instance-level override of `messageBox` wins; and that the class is an instance class |
| `crates/compat/tests/plugin.rs` | that `win32dialog.dll` resolves to the built-in table |

## Known gaps

* The reference's class body is a *script* class, so a script could read its
  source or subclass it in ways the native class does not model. The
  observable call behaviour — the `System.inform` lookup, the argument
  pass-through, the negation and the instance-level override — is the same.
* The reference's `type` argument is ignored in the reference body as well;
  the port ignores it identically.
* The message box itself is emulated: there is no modal window, so the text
  is logged and the first button is reported. The reference shows a real
  dialog and returns the button the user clicked.

# `text.rs` — script text decoding

**Source:** `crates/compat/src/text.rs`

Scenario and script files arrive as bytes. This module turns them into text:
it recognises the KiriKiri `FE FE` container, byte-order marks, an explicit
`Scripts.textEncoding` override, and — failing all of those — detects the
charset.

The TJS native text streams (`crates/tjs/cpp/streams.cpp`) use the same
handling: the stream layer owns the `FE FE` container and the
byte-order-mark branches, and it asks this module for the charset of a
BOM-less payload (the text decode seam, [`binding.md`](binding.md) §4).

---

## 1. The entry points

| Function | Returns |
|---|---|
| `decode_text(bytes, encoding) -> Result<String, String>` | the decoded UTF-8 text |
| `decode_text_units(bytes, encoding) -> Result<Vec<u16>, String>` | the raw UTF-16 code units |
| `decode_text_reported(bytes, encoding) -> Result<DecodedText, String>` | the text plus a label naming the path that produced it |

`decode_text_units` exists because a Rust `String` cannot hold an unpaired
surrogate, while an `FE FE` container decrypts to code units that may contain
one. The script loader decodes through this function and hands the VM the
code units instead of a lossy UTF-8 string.

`TextEncoding` selects the decoder:

| Variant | Meaning |
|---|---|
| `TextEncoding::Detect` | run charset detection |
| `TextEncoding::Named(name)` | decode with an explicit charset name |

The two are deliberately distinct: a caller that means the current code page
or any other fixed encoding must say so with `Named`, so "no encoding" cannot
mean detection in one place and the code page in another.

---

## 2. The detection order

1. **The `FE FE` container.** `FE FE` is never a valid UTF BOM, so it wins
   unambiguously.
2. **A byte-order mark.** UTF-32 is tested **before** UTF-16, because a
   UTF-32LE BOM begins with the UTF-16LE BOM `FF FE` — the longer match must
   win for UTF-32LE to be reachable.

   | BOM | Decoder |
   |---|---|
   | `FF FE 00 00` | UTF-32LE |
   | `00 00 FE FF` | UTF-32BE |
   | `EF BB BF` | UTF-8 |
   | `FF FE` | UTF-16LE |
   | `FE FF` | UTF-16BE |

3. **An explicit named encoding** (`Scripts.textEncoding`, or a plugin that
   names a fixed encoding) forces the decoder.
4. **Charset detection.**

---

## 3. The `FE FE` container

Modes 0/1 and 2 share the five-byte header `FE FE <mode> FF FE`: the two
bytes after the mode are the **mandatory** UTF-16LE byte-order mark.

### Mode 0 and 1 — the cipher

The payload starts at **offset 5** and is read as little-endian code units.

| Mode | Transform |
|---|---|
| 0 | the legacy per-code-unit XOR, applied only to units `>= 0x20` |
| 1 | the adjacent-bit swap |

The units are kept raw. The lossy string view is applied only by
`decode_text`, so the script loader can preserve an unpaired surrogate.

### Mode 2 — zlib

Layout: `FE FE 02 FF FE <compressed:u64 LE> <uncompressed:u64 LE> <zlib>` —
a 21-byte header, then the compressed payload.

The decode is strict:

| Check | Failure |
|---|---|
| the header is 21 bytes | `"FE FE zlib container needs 21 header bytes, got {n}"` |
| both sizes fit the address space | `"FE FE compressed size does not fit the address space"` / `"...uncompressed size..."` |
| the compressed size does not overrun the payload | `"FE FE compressed size {n} overruns the {m} available payload bytes"` |
| the inflate succeeds | `"FE FE zlib decode failed: {error}"` |
| the decoded length equals the header's uncompressed size | `"FE FE size mismatch: header says {n} bytes, decoded {m}"` |

### The header check

A container whose bytes 3–4 are not `FF FE` is rejected:
`"FE FE container mode {mode} is missing its UTF-16LE byte-order mark"`.

An unknown mode is rejected:
`"FE FE container mode {other} is not supported"`.

---

## 4. Charset detection

BOM-less text with no `Scripts.textEncoding` override is decoded from the
charset `chardetng` reports, with ISO-2022-JP kept in the candidate set.

The legacy East-Asian charsets overlap byte-for-byte, so the decision needs
the statistical model the detector provides; `encoding_rs` then does the bulk
decode and owns the per-charset tables.

### The single-byte special case

A single-byte detection maps each byte to one character, so it would decode a
BOM-less KAG scenario (mostly ASCII with a few CP932 double-byte characters)
as Latin-x and mangle every non-ASCII storage name. The port therefore tries
CP932 first whenever the decision is single-byte:

* `cp932_japanese_text` decodes **strictly** — any malformed sequence rejects
  the bytes, so a genuine Latin-1 payload keeps the Latin-1 mapping — and the
  result is accepted only when it contains a kana or a CJK ideograph, which
  only a CP932 double-byte sequence produces. The single-byte half-width
  katakana a Latin-1 payload can produce are deliberately *not* a signal.
* A reported `SHIFT_JIS` decodes as `cp932` (`encoding_rs`'s WHATWG
  Shift_JIS is already Windows-31J), so the reported encoding is used as-is.
* A genuine `WINDOWS-1252` payload keeps the Latin-1 mapping; the label for
  that path is `"detected WINDOWS-1252->latin1"`.

---

## 5. Dropped bytes

Every conversion path — a byte-order mark, `Scripts.textEncoding`, or charset
detection — **drops byte sequences the charset cannot decode** and logs the
drop **once per decode**. The surrounding text is kept: the decode is a
single pass over the input, and the drop is reported rather than silently
applied.

The only **typed error** from a decode is an unsupported explicit
`Scripts.textEncoding` name, and the size overflow of the worst-case UTF-8
length (`"{encoding}: input is too large to decode"`).

An `FE FE` payload's unpaired surrogate is dropped with one warning when the
lossy string view is taken; the code-unit view keeps it.

---

## 6. The reported path

`decode_text_reported` returns the text plus a short label naming the path
that produced it:

| Label | Path |
|---|---|
| `"FE FE container mode {n}"` | the container |
| `"UTF-32LE BOM"`, `"UTF-32BE BOM"`, `"UTF-8 BOM"`, `"UTF-16LE BOM"`, `"UTF-16BE BOM"` | a byte-order mark |
| `"explicit '{name}'"` | the `Scripts.textEncoding` override |
| `"detected {name}"` | the detector's decision |
| `"detected WINDOWS-1252->latin1"` | the single-byte Latin-1 special case |

The label is used for the drop diagnostics, so a log line names the encoding
that was actually used.

---

## 7. Where it is used

| Caller | How |
|---|---|
| `Scripts.execStorage` / `evalStorage` | the storage bytes are offered to the TJS2 **bytecode loader first**; only when the loader reports the input is not bytecode is it decoded as text |
| `Scripts.compileStorage` | the same order |
| the plugin text paths | `TextEncoding::Named` for a plugin that names a fixed encoding |
| the TJS text read stream (`Array.load`) | the stream layer keeps the container and the byte-order-mark branches; a BOM-less payload is decoded here, through the text decode seam ([`binding.md`](binding.md) §4) |

An undecodable script raises a typed error instead of producing mojibake.

---

## 8. Tests

The module carries a large in-crate test suite (the `#[cfg(test)]` module at
the end of `text.rs`), and the behaviour is exercised through the script
loader as well.

| Test | What it asserts |
|---|---|
| `gbk_script_is_detected_and_decoded` | a GBK scenario is detected and decoded |
| `big5_script_is_detected_and_decoded` | a Big5 scenario is detected and decoded |
| `euc_kr_script_is_detected_and_decoded` | an EUC-KR scenario is detected and decoded |
| `shift_jis_script_is_detected_and_decoded` | a Shift-JIS scenario is detected and decoded |
| `short_cp932_ams_scenario_is_detected_and_decoded` | a short CP932 scenario is detected (the statistical model needs enough data) |
| `short_cp932_ams_scenario_with_latin2_bytes_is_detected_and_decoded` | the same with a Latin-2 byte present |
| `windows1252_payload_keeps_the_latin1_mapping` | a genuine Latin-1 payload is not stolen by CP932 |
| `short_cp932_sample_decodes_through_the_override` | an explicit `cp932` override wins |
| `bom_takes_precedence_over_detection` | a BOM beats the detector |
| `utf32_bom_is_decoded` | UTF-32LE/BE are reachable |
| `override_takes_precedence_over_detection` | the explicit encoding beats the detector |
| `malformed_utf8_bom_payload_is_dropped_and_logged` | a bad UTF-8 payload drops and logs once |
| `malformed_utf16le_bom_payload_is_dropped_and_logged` | the same for UTF-16LE |
| `malformed_utf16be_bom_payload_is_dropped_and_logged` | the same for UTF-16BE |
| `malformed_bytes_are_dropped_and_logged` | the same for detected text |
| `fefe_payload_drops_unpaired_surrogates` | the lossy string view drops the surrogate and logs |
| `fefe_containers_keep_every_code_unit` | the code-unit view keeps it |
| `fefe_zlib_truncates_an_odd_payload` | an odd trailing byte in the zlib payload |
| `fefe_cipher_payload_starts_at_offset_five` | the cipher payload offset |
| `fefe_cipher_drops_an_odd_trailing_byte` | an odd trailing byte in the cipher payload |
| `fefe_cipher_requires_the_byte_order_mark` | the mandatory `FF FE` |
| `detected_script_with_a_malformed_byte_is_dropped` | a malformed byte in detected text |
| `override_cp932_drops_malformed_bytes` / `override_utf8_drops_malformed_bytes` | the same through an override |
| `utf32_bom_drops_invalid_scalars` | an invalid UTF-32 scalar |
| `reported_decoding_names_the_encoding_path` | the label table |
| `unsupported_charset_name_raises_a_typed_error` | only an unsupported *explicit* name is an error |
| `crates/compat/tests/scripts.rs` | the loader order: bytecode first, then text; a corrupt script is a typed error |
| `crates/compat/tests/streams.rs::array_load_decodes_a_shift_jis_storage_member` | the text read stream decodes a BOM-less Shift-JIS member as Japanese |
| `crates/compat/tests/streams.rs::array_load_decodes_a_utf8_storage_member` | a BOM-less UTF-8 member still decodes as UTF-8 |

---

## See also

* [`natives.md`](natives.md) — the `Scripts` class that drives the loader
* [`../../arch/l4.md`](../../arch/l4.md) §3 — script execution and loading in the VM host

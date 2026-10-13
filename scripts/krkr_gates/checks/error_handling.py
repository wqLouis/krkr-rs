"""Silent-fallback and dropped-error guard (AGENTS §4).

Scans ``crates/**`` for parse-then-default coercion, fallbacks on known
fallible calls, discarded results, pointer/address sign checks and narrowing,
and wildcard ``Err`` arms that swallow a failure. Sites are reported with a
rule name and file:line; an accepted site needs an entry in
``scripts/allowlists/error_handling_allow.txt`` (a rule name or a source-line
anchor).
"""

import re
import sys

from ..common import ALLOWLISTS_DIR, FIXTURES_DIR, REPO_ROOT

ROOT = REPO_ROOT
ALLOW = ALLOWLISTS_DIR / "error_handling_allow.txt"
ARGS: set[str] = set()
PRINT_ALL = False

CRATES = ROOT / "crates"
# The one documented `#[allow(dead_code)]` exception (testing helper).
DEAD_CODE_EXCEPTION = "crates/compat/tests/common/mod.rs"

# Known-fallible calls.  These names return `Result` (or a `Result`-like
# error path); a fallback on them is a candidate §4 violation.  `format.i32`
# (MediaFormat) and `str::strip_prefix` are `Option` and are handled by the
# allowlist rather than removed from the list, so nothing is silently ignored.
FALLIBLE = re.compile(
    r"(metadata\(|read_dir\(|CString::new\(|\.status\(|\.send_eof\("
    r"|\.poll\(|decode|inflate|\.lock\(|\.wait\(|\.strip_prefix\(|\.absolute\("
    r"|\.i32\(|\.i16\(|\.u8\(|\.u16\(|\.u32\(|\.u64\(|\.unicode_string\("
    r"|\.key4\(|\btry_from\(|\btry_into\(|\.integer\(|\.take\("
    r"|\.read\(|\.write\(|\.parse\()"
)
# `.ok()?` on a `try_from`/`try_into` is the out-of-range => `None` contract
# (AGENTS §4 list calls this legitimate), so those two are not in the `.ok()`
# detector.  Everything else matches FALLIBLE.
FALLIBLE_OK = re.compile(
    r"(metadata\(|read_dir\(|CString::new\(|\.status\(|\.send_eof\("
    r"|\.poll\(|decode|inflate|\.lock\(|\.wait\(|\.strip_prefix\(|\.absolute\("
    r"|\.i32\(|\.i16\(|\.u8\(|\.u16\(|\.u32\(|\.u64\(|\.unicode_string\("
    r"|\.key4\(|\.integer\(|\.take\(|\.read\(|\.write\(|\.parse\()"
)
# Fallible calls whose discarded result is worth flagging under `let _ =`.
FALLIBLE_LET = re.compile(
    r"(set_member|set_call_missing|set_default_member|resolve_face"
    r"|update_layer|\.poll\(|\.stop\(|delete_member|send_eof|call_member)"
)

UNWRAP = re.compile(r"\.unwrap_or")
PARSE = re.compile(r"\.parse(::<[^>]*>)?\(\)")
OK_CALL = re.compile(r"\.ok\(\)")
IFLET_OK = re.compile(r"if let Ok\(")
LET_UND = re.compile(r"let _ = ")
DEBUG_ASSERT = re.compile(r"debug_assert(_eq|_ne)?!")
NON_EXHAUSTIVE = re.compile(r"#\[non_exhaustive\]")
DEAD_CODE = re.compile(r"#!?\[allow\(dead_code\)\]")

# A pointer- or address-derived value.  These tokens name the value that
# crosses the ABI or is keyed in process state; a signed cast or a sign test
# on one of them is the aarch64 hazard (D37).
POINTER_LIKE = re.compile(
    r"\b(instance|objthis|_?ptr|addr|address|handle|retained)\b"
    r"|into_raw\(|\.as_ptr\(\)|\.as_mut_ptr\(\)|object_handle\(\)|pixel_buffer"
)
# A cast to a signed type; check the receiver window for a pointer-like token.
SIGNED_CAST = re.compile(r"\bas\s+i(?:8|16|32|64)\b")
# A sign/positivity test directly on a pointer-like value.
SIGN_TEST = re.compile(
    r"\b(instance|objthis|_?ptr|addr|address|handle|retained)\b"
    r"\s*(?:[<>]=?\s*0(?:\.0)?|\.max\(\s*0\s*\)|\.abs\(\))"
)
# A hidden, script-visible property that carries a pointer as an integer.
HIDDEN_HANDLE = re.compile(r'\bname\s*:\s*"__handle"')

# --- flow-sensitive pointer analysis (D37) --------------------------------
# The `pointer-sign` rule is syntactic: it sees the cast and the sign test only
# when they share a line.  The rules below are a small flow pass over one file
# that tracks which locals were assigned from a pointer-derived expression and
# flags a later use.  It reuses `mask()`/`test_ranges()` so it ignores comments,
# literals and `#[cfg(test)]` items, and it follows a statement across lines.
#
# The tokenizer splits the masked source; operators are single tokens so `==`
# and `=` cannot be confused.
TOKEN_RE = re.compile(
    r"[A-Za-z_][A-Za-z0-9_]*"
    r"|0[xX][0-9a-fA-F_]+"
    r"|\d[\d_]*(?:\.\d+)?(?:[eE][+-]?\d+)?"
    r"|::|->|=>|\.\.=?|\|\||&&|==|!=|<=|>=|<<=|>>=|\+=|-=|\*=|/=|%=|&=|\|=|\^=|<<|>>"
    r"|[+\-*/%&|^!~<>=?.,;:()\[\]{}@#']"
)
# `x as *const T` / `x as *mut T`.
PTR_CAST = re.compile(r"\bas\s*\*\s*(?:const|mut)\b")
# Expressions that yield a pointer or a raw address.
POINTER_ACCESSOR = re.compile(
    r"\.\s*as_ptr\b|\.\s*as_mut_ptr\b|into_raw\b|object_handle\b"
    r"|native_instance_payload\b|TJS_GET_NATIVE_INSTANCE\b|pixel_buffer"
)
# Names that denote the ABI payload pointer or a raw address.
ADDR_NAMES = {"instance", "objthis", "ptr", "addr", "address", "handle", "retained"}
# A function name that plausibly returns an address; the interprocedural part
# is name-based, so generic names (`new`, `read`) must not qualify.
PTR_FUNC_NAME = re.compile(
    r"(?i)(?:ptr|addr|address|raw|handle|instance|pixel_?buf|as_usize|to_usize|native)"
)


def _expr_is_addr(expr_tokens):
    """True when a return/tail expression yields a pointer or raw address."""
    blob = " ".join(expr_tokens)
    if PTR_CAST.search(blob) or POINTER_ACCESSOR.search(blob):
        return True
    return re.search(r"\b(?:instance|objthis)\s+as\b", blob) is not None


CMP_OPS = {"<", ">", "<=", ">=", "==", "!="}
# Tokens that end an operand window.
OPERAND_BOUNDARY = {
    ";", "{", "}", ",", "(", ")", "[", "]",
    "&&", "||", "!", "=", "=>", "return", "if", "while", "match",
}
INTEGER_TYPES = {"u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "usize", "isize"}
LET_SKIP = {
    "let", "mut", "const", "ref", "static", "as", "self", "Self",
    "true", "false", "move", "in", "where", "dyn", "impl", "c_char", "_",
} | INTEGER_TYPES


def mask(src):
    """Blank comments and string/char literal *contents*, keeping newlines.

    The result has the same length as `src`, so byte offsets and line numbers
    stay valid.  Braces inside literals can no longer desynchronise the
    `#[cfg(test)]` brace matcher.
    """
    out = list(src)
    n = len(src)
    i = 0

    def blank(a, b):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            j = i + 2
            while j < n and src[j] != "\n":
                j += 1
            blank(i, j)
            i = j
        elif c == "/" and i + 1 < n and src[i + 1] == "*":
            depth = 1
            j = i + 2
            while j < n and depth:
                if src[j] == "/" and j + 1 < n and src[j + 1] == "*":
                    depth += 1
                    j += 2
                elif src[j] == "*" and j + 1 < n and src[j + 1] == "/":
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == "r" and i + 1 < n and (src[i + 1] == '"' or src[i + 1] == "#"):
            m = re.match(r'r(#*)"', src[i:])
            if m:
                hashes = m.group(1)
                start = i + len(m.group(0))
                end = src.find('"' + hashes, start)
                if end == -1:
                    blank(i, n)
                    i = n
                else:
                    blank(i, end + len(hashes) + 1)
                    i = end + len(hashes) + 1
                continue
            i += 1
        elif c == '"':
            j = i + 1
            while j < n:
                if src[j] == "\\":
                    j += 2
                elif src[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == "'":
            m = re.match(
                r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^'\\])'", src[i:]
            )
            if m:
                blank(i, i + len(m.group(0)))
                i += len(m.group(0))
            else:
                i += 1
        else:
            i += 1
    return "".join(out)


def test_ranges(masked):
    """Character ranges of every `#[cfg(test)]` item."""
    ranges = []
    n = len(masked)
    for m in re.finditer(r"#\[cfg\(test\)\]", masked):
        j = m.end()
        while j < n and masked[j] not in "{;":
            j += 1
        if j >= n:
            continue
        if masked[j] == ";":
            ranges.append((m.start(), j))
            continue
        depth = 0
        k = j
        while k < n:
            if masked[k] == "{":
                depth += 1
            elif masked[k] == "}":
                depth -= 1
                if depth == 0:
                    break
            k += 1
        ranges.append((m.start(), k + 1))
    return ranges


def in_ranges(pos, ranges):
    return any(a <= pos < b for a, b in ranges)


def stmt_prefix(text, pos):
    """The expression text from the last statement boundary before `pos`.

    A statement boundary is `;`, `{` or `}`, which lets a receiver chain that
    spans lines (`.lock()\n.unwrap_or_else(..)`) still be inspected.
    """
    lo = 0
    for ch in ";{}":
        k = text.rfind(ch, 0, pos)
        if k > lo:
            lo = k
    return text[lo + 1 : pos]


def _blank_ranges(masked, ranges):
    """Blank `#[cfg(test)]` items but keep newlines, so braces stay balanced."""
    chars = list(masked)
    for a, b in ranges:
        for k in range(a, min(b, len(chars))):
            if chars[k] != "\n":
                chars[k] = " "
    return "".join(chars)


def _tokenize(text):
    return [(m.group(0), m.start()) for m in TOKEN_RE.finditer(text)]


def collect_ptr_funcs(masked, ranges):
    """Names of functions whose return is pointer-derived.

    Used by `pointer-flow`: a `-> *const`/`-> *mut` return, or an integer
    return whose body produces an address (`as usize` on a `.as_ptr()`,
    a `pixel_buffer` accessor, `instance as`, ...).  Over-reports by design;
    a missed pointer flow is worse than a reviewed exception.
    """
    text = _blank_ranges(masked, ranges)
    toks = _tokenize(text)
    funcs = set()
    n = len(toks)
    i = 0
    while i < n:
        if toks[i][0] != "fn":
            i += 1
            continue
        if i + 1 >= n or not re.match(r"^[A-Za-z_]\w*$", toks[i + 1][0]):
            i += 1
            continue
        name = toks[i + 1][0]
        j = i + 1
        depth = 0
        while j < n:
            tok = toks[j][0]
            if tok == "(":
                depth += 1
            elif tok == ")":
                depth -= 1
                if depth == 0:
                    j += 1
                    break
            j += 1
        ret = []
        if j < n and toks[j][0] == "->":
            j += 1
            rdepth = 0
            while j < n:
                tok = toks[j][0]
                if rdepth == 0 and tok in ("{", "where"):
                    break
                if tok == "(":
                    rdepth += 1
                elif tok == ")":
                    rdepth -= 1
                ret.append(tok)
                j += 1
        body_toks = []
        if j < n and toks[j][0] == "{":
            d = 0
            k = j
            while k < n:
                if toks[k][0] == "{":
                    d += 1
                elif toks[k][0] == "}":
                    d -= 1
                    if d == 0:
                        body_toks = toks[j + 1 : k]
                        break
                k += 1
        ret_text = " ".join(ret)
        if ("*const" in ret_text or "*mut" in ret_text) and PTR_FUNC_NAME.search(name):
            funcs.add(name)
            i += 1
            continue
        # An integer return is pointer-derived only when a `return` or the tail
        # expression itself produces an address; a body that merely mentions
        # `.as_ptr()` (a byte-buffer accessor) does not qualify.
        addr_returns = []
        depth = 0
        m = 0
        while m < len(body_toks):
            tk = body_toks[m][0]
            if tk in "([{":
                depth += 1
            elif tk in ")]}":
                depth -= 1
            elif tk == "return" and depth == 0:
                e = []
                e3 = m + 1
                d2 = 0
                while e3 < len(body_toks):
                    et = body_toks[e3][0]
                    if et in "([{":
                        d2 += 1
                    elif et in ")]}":
                        if d2 == 0:
                            break
                        d2 -= 1
                    elif et == ";" and d2 == 0:
                        break
                    e.append(et)
                    e3 += 1
                addr_returns.append(e)
            m += 1
        if body_toks:
            last_semi = -1
            depth = 0
            for m, (tk, _pos) in enumerate(body_toks):
                if tk in "([{":
                    depth += 1
                elif tk in ")]}":
                    depth -= 1
                elif tk == ";" and depth == 0:
                    last_semi = m
            tail = [tk for tk, _pos in body_toks[last_semi + 1 :]]
            if tail:
                addr_returns.append(tail)
        if any(_expr_is_addr(expr) for expr in addr_returns) and PTR_FUNC_NAME.search(name):
            funcs.add(name)
        i += 1
    return funcs


def lhs_idents(toks, eq):
    """Identifiers in the pattern immediately left of the `=` at `eq`."""
    j = eq - 1
    depth = 0
    start = 0
    while j >= 0:
        tok = toks[j][0]
        if tok in ")]}":
            depth += 1
        elif tok in "([{":
            if depth == 0:
                start = j + 1
                break
            depth -= 1
        elif depth == 0 and tok == ";":
            start = j + 1
            break
        j -= 1
    idents = []
    for k in range(start, eq):
        tok = toks[k][0]
        if re.match(r"^[A-Za-z_]\w*$", tok) and tok not in LET_SKIP:
            idents.append(tok)
    return idents


def rhs_end(toks, start):
    """Index of the `;`/`}` ending the statement whose RHS starts at `start`."""
    depth = 0
    j = start
    n = len(toks)
    while j < n:
        tok = toks[j][0]
        if tok in "([{":
            depth += 1
        elif tok in ")]":
            if depth == 0:
                return j
            depth -= 1
        elif tok == "}":
            if depth == 0:
                return j
            depth -= 1
        elif tok == ";" and depth == 0:
            return j
        j += 1
    return j


def operand_left(toks, i):
    j = i - 1
    parts = []
    while j >= 0:
        tok = toks[j][0]
        if tok in OPERAND_BOUNDARY:
            break
        parts.append(toks[j])
        j -= 1
    return list(reversed(parts))


def receiver_before(toks, i):
    """The expression cast/projected immediately before `i`, calls included.

    Unlike `operand_left`, a closing `()`/`[]` is part of the receiver
    (`payload.rgba.as_ptr() as u32`), but a statement/logical separator ends it.
    """
    j = i - 1
    parts = []
    depth = 0
    while j >= 0:
        tok = toks[j][0]
        if tok in ("]", ")"):
            depth += 1
        elif tok in ("[", "("):
            if depth == 0:
                break
            depth -= 1
        elif depth == 0 and tok in (
            ";",
            ",",
            "{",
            "}",
            "return",
            "=",
            "&&",
            "||",
            "->",
            "=>",
            "let",
        ):
            break
        parts.append(toks[j])
        j -= 1
    return list(reversed(parts))


def operand_right(toks, i):
    j = i + 1
    parts = []
    n = len(toks)
    while j < n:
        tok = toks[j][0]
        if tok in OPERAND_BOUNDARY or tok in CMP_OPS:
            break
        parts.append(toks[j])
        j += 1
    return parts


def blob_of(tokens):
    return " ".join(tk for tk, _pos in tokens)


def param_typed_names(toks, fn_i):
    """(pointer names, c_char names) declared in a function's parameter list."""
    n = len(toks)
    j = fn_i + 1
    while j < n and toks[j][0] != "(":
        if toks[j][0] in (";", "{", "}"):
            return set(), set()
        j += 1
    if j >= n:
        return set(), set()
    depth = 0
    ptr_names = set()
    cchar_names = set()
    k = j
    while k < n:
        tk = toks[k][0]
        if tk == "(":
            depth += 1
        elif tk == ")":
            depth -= 1
            if depth == 0:
                break
        elif tk == ":" and depth == 1 and k + 1 < n:
            name = toks[k - 1][0] if k - 1 >= 0 else ""
            if not re.match(r"^[A-Za-z_]\w*$", name):
                k += 1
                continue
            if toks[k + 1][0] == "*" and k + 2 < n and toks[k + 2][0] in (
                "const",
                "mut",
            ):
                ptr_names.add(name)
            elif toks[k + 1][0] == "c_char":
                cchar_names.add(name)
        k += 1
    return ptr_names, cchar_names


def lhs_ptr_typed(toks, eq):
    """True when the pattern left of `=` carries a `: *const`/`: *mut` type."""
    j = eq - 1
    depth = 0
    start = 0
    while j >= 0:
        tok = toks[j][0]
        if tok in ")]}":
            depth += 1
        elif tok in "([{":
            if depth == 0:
                start = j + 1
                break
            depth -= 1
        elif depth == 0 and tok == ";":
            start = j + 1
            break
        j -= 1
    for k in range(start, eq - 2):
        if (
            toks[k][0] == ":"
            and toks[k + 1][0] == "*"
            and toks[k + 2][0] in ("const", "mut")
        ):
            return True
    return False


def _generic_open(toks, i):
    """Heuristic: is the `<` at `i` a generic/turbofish opener, not `less than`?

    A generic is written tight on at least one side (`Option<*mut T>`, `Vec<u8>`,
    `state::<T>`); a comparison the guard cares about is written with whitespace
    (`h > 0`).  A tight `a<b` is therefore missed, which the audit backstops.
    """
    if i == 0 or i + 1 >= len(toks):
        return False
    pt, pp = toks[i - 1]
    nt, np = toks[i + 1]
    pos = toks[i][1]
    if pt == "::":
        return True
    if not re.match(r"^[A-Za-z_]\w*$", pt) and pt not in (")", "]", ">"):
        return False
    if not (
        re.match(r"^[A-Za-z_]\w*$", nt)
        or nt in ("*", "&", "'", "(", "[", "_", "dyn", "impl", "::")
    ):
        return False
    gap_before = pp + len(pt) < pos
    gap_after = pos + 1 < np
    return (not gap_before) or (not gap_after)


def scan_pointer_flow(rel, masked, offsets, ranges, ptr_funcs, orig):
    """The flow-sensitive pointer rules; see the rule list in the header."""
    import bisect

    findings = []
    text = _blank_ranges(masked, ranges)
    toks = _tokenize(text)
    n = len(toks)
    if not n:
        return findings

    def add(pos, rule):
        ln = bisect.bisect_right(offsets, pos)
        ln = max(1, min(ln, len(orig)))
        code = orig[ln - 1].strip() if ln - 1 < len(orig) else ""
        findings.append((rel, ln, rule, code))

    ptr_vars = set()
    cchar_vars = set()
    signed_vars = set()
    generic_depth = 0

    ident_in = re.compile(r"[A-Za-z_]\w*")

    def words_of(blob):
        return set(ident_in.findall(blob))

    def top_level(tokens):
        """(index, token) pairs at the outermost expression depth."""
        out = []
        depth = 0
        for idx, (tk, _pos) in enumerate(tokens):
            if tk in "([{":
                depth += 1
            elif tk in ")]}":
                depth -= 1
            elif depth == 0:
                out.append((idx, tk))
        return out

    def rhs_taints(tokens):
        """True when the expression itself produces a pointer/address value.

        Only a top-level cast/accessor counts: `foo(x.as_ptr())` returns
        whatever `foo` returns, not an address, so it must not taint.
        """
        if not tokens:
            return False
        top = top_level(tokens)
        if not top:
            return False
        top_blob = " ".join(tk for _idx, tk in top)
        if PTR_CAST.search(top_blob) or POINTER_ACCESSOR.search(top_blob):
            return True
        top_words = {tk for _idx, tk in top if re.match(r"^[A-Za-z_]\w*$", tk)}
        stripped = top_blob.strip()
        # A bare pointer local/address name, or one cast as an integer.  A
        # pointer used only as the base of a numeric field (`value.ty`) is not
        # itself the value.  A field/accessor named like an address is caught
        # by `is_ptr_operand` or by `POINTER_ACCESSOR` above.
        if stripped in ptr_vars or stripped in ADDR_NAMES:
            return True
        cast = re.match(r"^([A-Za-z_]\w*)\s+as\s+", stripped)
        if cast and (cast.group(1) in ptr_vars or cast.group(1) in ADDR_NAMES):
            return True
        if top_words & ptr_vars and "." not in top_blob:
            return True
        for name in top_words & ptr_funcs:
            idx = next(i for i, tk in top if tk == name)
            if idx + 1 < len(tokens) and tokens[idx + 1][0] == "(":
                return True
        return False

    def is_ptr_operand(tokens):
        """A comparison operand that carries an address (or a pointer local)."""
        if rhs_taints(tokens):
            return True
        top = [
            tk for _i, tk in top_level(tokens) if re.match(r"^[A-Za-z_]\w*$", tk)
        ]
        return bool(top) and top[-1] in ADDR_NAMES

    def is_cchar_text(blob):
        return "c_char" in blob or bool(words_of(blob) & cchar_vars)

    i = 0
    while i < n:
        tok = toks[i][0]
        pos = toks[i][1]

        if tok == "fn":
            param_ptrs, param_cchars = param_typed_names(toks, i)
            ptr_vars = set(param_ptrs)
            cchar_vars = set(param_cchars)
            signed_vars = set()
            generic_depth = 0
            i += 1
            continue

        if tok in ("<", ">", ">>"):
            if tok == "<":
                if generic_depth > 0 or _generic_open(toks, i):
                    generic_depth += 1
                    i += 1
                    continue
            elif tok == ">":
                if generic_depth > 0:
                    generic_depth -= 1
                    i += 1
                    continue
            else:  # ">>"
                if generic_depth > 0:
                    generic_depth = max(0, generic_depth - 2)
                    i += 1
                    continue

        if tok == "=":
            idents = lhs_idents(toks, i)
            end = rhs_end(toks, i + 1)
            rhs_tokens = toks[i + 1 : end]
            rhs = blob_of(rhs_tokens)
            if idents:
                if rhs_taints(rhs_tokens) or lhs_ptr_typed(toks, i):
                    ptr_vars.update(idents)
                if "c_char" in rhs:
                    cchar_vars.update(idents)
                if re.search(r"\bas\s+(?:i8|i16|i32|i64|isize)\b", rhs):
                    signed_vars.update(idents)
            i += 1
            continue

        if tok == "as":
            recv_tokens = receiver_before(toks, i)
            recv = blob_of(recv_tokens)
            recv_words = words_of(recv)
            tgt = toks[i + 1][0] if i + 1 < n else ""
            if tgt == "c_char":
                if (
                    re.search(r"\bas\s+(?:i8|i16|i32|i64|isize)\b", recv)
                    or re.search(r"(?:^|[^\w])-\s*\d", recv)
                    or bool(recv_words & signed_vars)
                ):
                    add(pos, "char-signedness")
            if tgt in ("i8", "u8", "i16", "u16") and (
                "c_char" in recv or bool(recv_words & cchar_vars)
            ):
                add(pos, "char-signedness")
            if tgt in ("u32", "i32") and rhs_taints(recv_tokens):
                add(pos, "addr-narrow")
            i += 1
            continue

        if tok in CMP_OPS:
            left_tokens = operand_left(toks, i)
            right_tokens = operand_right(toks, i)
            left = blob_of(left_tokens)
            right = blob_of(right_tokens)
            lp = is_ptr_operand(left_tokens)
            rp = is_ptr_operand(right_tokens)
            if lp and rp:
                add(pos, "pointer-compare")
            elif lp or rp:
                add(pos, "pointer-flow")
            if is_cchar_text(left) or is_cchar_text(right):
                other = right if is_cchar_text(left) else left
                ordering = tok in ("<", ">", "<=", ">=")
                negative = re.search(r"(?:^|[^\w])-\s*\d", other) is not None
                if negative or (ordering and re.fullmatch(r"0(?:\.0)?", other.strip())):
                    add(pos, "char-signedness")
            i += 1
            continue

        if tok in ptr_vars:
            nxt = toks[i + 1][0] if i + 1 < n else ""
            nx2 = toks[i + 2][0] if i + 2 < n else ""
            prv = toks[i - 1][0] if i - 1 >= 0 else ""
            pr2 = toks[i - 2][0] if i - 2 >= 0 else ""
            pr3 = toks[i - 3][0] if i - 3 >= 0 else ""
            pr4 = toks[i - 4][0] if i - 4 >= 0 else ""
            if nxt == "." and nx2 in ("max", "abs"):
                add(pos, "pointer-flow")
            elif nxt == "." and nx2 == "try_into":
                add(pos, "pointer-flow")
            elif prv == "(" and pr2 in ("try_from", "try_into"):
                add(pos, "pointer-flow")
            elif prv == "(" and pr2 == "new" and pr3 == "::" and "NonZero" in pr4:
                add(pos, "pointer-flow")
            i += 1
            continue

        i += 1
    return findings


# --- error-swallow pass (AGENTS §4: "drops the error variant") --------------
# Wildcard `Err(_)`/`Err(..)` arms are the §4 "discards the error" shape.  The
# body decides whether the error is really dropped: an arm that raises a new
# error (`report_error`, `Err(..)`), forwards it (`?`), transforms it
# (`map_err`), panics, or logs it is not silent, and a `reference/cpp/**`
# citation beside the arm is the documented justification.  Everything else is
# a reviewed allowlist entry.
CITATION_WINDOW = 4


def _arm_body(toks, start):
    """Tokens of a match arm body, ending at the arm's terminating comma."""
    body = []
    depth = 0
    i = start
    n = len(toks)
    while i < n:
        tk, _pos = toks[i]
        if tk in "([{":
            depth += 1
        elif tk in ")]}":
            if depth == 0:
                break
            depth -= 1
        elif tk == "," and depth == 0:
            break
        body.append(toks[i])
        i += 1
    return body


def _brace_body(toks, brace):
    """Tokens inside the `{ ... }` whose opening brace is at `brace`."""
    body = []
    depth = 0
    i = brace
    n = len(toks)
    while i < n:
        tk, _pos = toks[i]
        if tk == "{":
            depth += 1
        elif tk == "}":
            depth -= 1
            if depth == 0:
                break
        elif depth >= 1:
            body.append(toks[i])
        i += 1
    return body


def _arm_raises(body):
    """True when a wildcard-error arm raises, forwards, or logs the failure."""
    blob = " ".join(tk for tk, _pos in body)
    if "report_error" in blob or "map_err" in blob:
        return True
    if re.search(r"\bErr\b", blob):
        return True
    if any(tk == "?" for tk, _pos in body):
        return True
    for kw in ("panic", "unreachable", "todo", "unimplemented", "assert"):
        if kw in blob:
            return True
    for k, (tk, _pos) in enumerate(body):
        if tk in ("log", "tracing") and k + 1 < len(body) and body[k + 1][0] == "::":
            return True
    return False


def scan_error_swallow(rel, masked, offsets, ranges, orig):
    """Flag a wildcard-error arm that discards the error into a value.

    `Err(_)`/`Err(..)` in a `match`, and `if let Err(_)`/`if let Err(..)`, are
    the §4 "drops the error variant" shape.  The arm body and an attached
    `reference/cpp/**` citation decide whether it is a reviewed mapping.
    """
    import bisect

    text = _blank_ranges(masked, ranges)
    toks = _tokenize(text)
    findings = []
    n = len(toks)
    i = 0
    while i < n:
        tk, pos = toks[i]
        body = None
        if (
            tk == "Err"
            and i + 3 < n
            and toks[i + 1][0] == "("
            and toks[i + 2][0] in ("_", "..")
            and toks[i + 3][0] == ")"
        ):
            # A match arm: skip an optional guard, then require `=>`.
            m = i + 4
            depth = 0
            while m < n:
                t = toks[m][0]
                if t in "([{":
                    depth += 1
                elif t in ")]}":
                    if depth == 0:
                        break
                    depth -= 1
                elif t == "=>" and depth == 0:
                    break
                elif t == "," and depth == 0:
                    break
                m += 1
            if m < n and toks[m][0] == "=>":
                body = _arm_body(toks, m + 1)
        elif (
            tk == "if"
            and i + 5 < n
            and toks[i + 1][0] == "let"
            and toks[i + 2][0] == "Err"
            and toks[i + 3][0] == "("
            and toks[i + 4][0] in ("_", "..")
            and toks[i + 5][0] == ")"
        ):
            # `if let Err(_) = .. { body }` — only the braced body is checked.
            m = i + 6
            while m < n and toks[m][0] not in ("{", ";"):
                m += 1
            if m < n and toks[m][0] == "{":
                body = _brace_body(toks, m)
        if body is not None and not _arm_raises(body):
            ln = max(1, min(bisect.bisect_right(offsets, pos), len(orig)))
            end_ln = ln
            if body:
                end_ln = max(
                    1,
                    min(bisect.bisect_right(offsets, body[-1][1]), len(orig)),
                )
            window = "\n".join(orig[max(0, ln - 1 - CITATION_WINDOW) : end_ln])
            if "reference/cpp/" not in window:
                code = orig[ln - 1].strip() if ln - 1 < len(orig) else ""
                findings.append((rel, ln, "error-swallow", code))
        i += 1
    return findings


def scan(roots=None):
    findings = []
    root_dirs = [CRATES] if roots is None else list(roots)
    files = []
    for root in root_dirs:
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*.rs")):
            rel = str(path.relative_to(ROOT))
            if "/tjs/cpp/" in rel:
                continue
            try:
                src = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            files.append((rel, src))

    # Interprocedural names first: a call to a function that itself returns a
    # pointer-derived integer is a pointer source for the flow pass.
    ptr_funcs = set()
    for _rel, src in files:
        masked = mask(src)
        ptr_funcs |= collect_ptr_funcs(masked, test_ranges(masked))

    for rel, src in files:
        masked = mask(src)
        orig = src.splitlines()

        # `#[allow(dead_code)]` is banned everywhere outside the one
        # documented exception, including test and build files (D32).
        for idx, line in enumerate(masked.splitlines()):
            if DEAD_CODE.search(line) and rel != DEAD_CODE_EXCEPTION:
                code = orig[idx].strip() if idx < len(orig) else ""
                findings.append((rel, idx + 1, "allow-dead-code", code))

        if "/tests/" in rel or rel.endswith("build.rs"):
            continue
        ranges = test_ranges(masked)
        lines = masked.splitlines()
        offsets = []
        acc = 0
        for line in lines:
            offsets.append(acc)
            acc += len(line) + 1

        def add(line_no, rule, _masked_code):
            # Report the real source line; detection used the masked copy.
            code = orig[line_no - 1].strip() if line_no - 1 < len(orig) else ""
            findings.append((rel, line_no, rule, code))

        for idx, line in enumerate(lines):
            if not line.strip():
                continue
            base = offsets[idx]
            if in_ranges(base, ranges):
                continue
            stripped = line.strip()

            for m in UNWRAP.finditer(line):
                if in_ranges(base + m.start(), ranges):
                    continue
                pre = stmt_prefix(masked, base + m.start())
                if PARSE.search(pre):
                    add(idx + 1, "parse-unwrap", stripped)
                elif FALLIBLE.search(pre):
                    add(idx + 1, "result-fallback", stripped)

            if LET_UND.search(line) and "?" not in line and FALLIBLE_LET.search(line):
                add(idx + 1, "let-underscore", stripped)

            for m in OK_CALL.finditer(line):
                pre = stmt_prefix(masked, base + m.start())
                if FALLIBLE_OK.search(pre):
                    add(idx + 1, "ok-discard", stripped)

            if IFLET_OK.search(line):
                window = "\n".join(lines[idx : idx + 4])
                if FALLIBLE.search(window):
                    add(idx + 1, "iflet-ok", stripped)

            if DEBUG_ASSERT.search(line):
                add(idx + 1, "debug-assert", stripped)

            for m in SIGNED_CAST.finditer(line):
                if POINTER_LIKE.search(line[max(0, m.start() - 80) : m.start()]):
                    add(idx + 1, "pointer-sign", stripped)
                    break

            if SIGN_TEST.search(line):
                add(idx + 1, "pointer-sign", stripped)

            if idx < len(orig) and HIDDEN_HANDLE.search(orig[idx]):
                add(idx + 1, "pointer-sign", stripped)

            if NON_EXHAUSTIVE.search(line) and rel.startswith("crates/compat/"):
                add(idx + 1, "non-exhaustive", stripped)

        findings.extend(
            scan_pointer_flow(rel, masked, offsets, ranges, ptr_funcs, orig)
        )
        findings.extend(
            scan_error_swallow(rel, masked, offsets, ranges, orig)
        )

    seen = set()
    unique = []
    for rel, line, rule, code in findings:
        key = (rel, line, rule)
        if key in seen:
            continue
        seen.add(key)
        unique.append((rel, line, rule, code))
    return unique


RULES = {
    "parse-unwrap",
    "result-fallback",
    "let-underscore",
    "ok-discard",
    "iflet-ok",
    "debug-assert",
    "non-exhaustive",
    "allow-dead-code",
    "pointer-sign",
    "pointer-flow",
    "pointer-compare",
    "char-signedness",
    "addr-narrow",
    "error-swallow",
}


def normalize(text):
    return " ".join(text.split())


def load_allowlist():
    """Return a list of (path, line, pattern, reason) allowlist entries."""
    entries = []
    if not ALLOW.exists():
        return entries
    for raw in ALLOW.read_text(encoding="utf-8").splitlines():
        text = raw.strip()
        if not text or text.startswith("#"):
            continue
        key, sep, reason = text.partition("\u2014")  # em dash
        if not sep:
            key, sep, reason = text.partition(" -- ")
        parts = key.split(":", 2)
        if len(parts) != 3:
            continue
        path, line, pattern = parts[0].strip(), parts[1].strip(), parts[2].strip()
        try:
            line_no = int(line)
        except ValueError:
            line_no = -1
        entries.append((path, line_no, pattern, reason.strip()))
    return entries


def entry_matches(entry, rel, line, rule, code):
    path, line_no, pattern, _reason = entry
    if path != rel:
        return False
    if pattern in RULES:
        return pattern == rule and line_no == line
    # Code anchor: survives line shifts, fails closed when the code changes.
    return normalize(pattern) in normalize(code)


FIXTURES = FIXTURES_DIR / "guard"
BAD_FIXTURE = "scripts/fixtures/guard/bad_pointer.rs"
GOOD_FIXTURE = "scripts/fixtures/guard/good_pointer.rs"
POINTER_RULES = {
    "pointer-sign",
    "pointer-flow",
    "pointer-compare",
    "char-signedness",
    "addr-narrow",
}
SELF_TEST_BAD_RULES = (
    "pointer-flow",
    "pointer-compare",
    "char-signedness",
    "addr-narrow",
)
# Each (bad, good, required) group: the bad fixture must trip every `required`
# rule and the good fixture must trip nothing at all.
SELF_TEST_GROUPS = (
    (BAD_FIXTURE, GOOD_FIXTURE, SELF_TEST_BAD_RULES),
    (
        "scripts/fixtures/guard/bad_error_swallow.rs",
        "scripts/fixtures/guard/good_error_swallow.rs",
        ("error-swallow",),
    ),
)


def self_test():
    """Assert each rule still matches its bad fixture and not its near-miss.

    A regex that silently stops matching is worse than no guard, so the guard
    runs this on every invocation: every bad fixture must trip its required
    rules and every near-miss fixture must trip none.
    """
    if not FIXTURES.is_dir():
        print(f"ERROR: guard self-test fixtures missing at {FIXTURES}")
        return 1
    findings = scan([FIXTURES])
    by_file = {}
    for rel, line, rule, code in findings:
        by_file.setdefault(rel, []).append((line, rule, code))
    problems = []
    required_total = 0
    good_total = 0
    for bad_fixture, good_fixture, required in SELF_TEST_GROUPS:
        bad_rules = {rule for _line, rule, _code in by_file.get(bad_fixture, [])}
        for rule in required:
            required_total += 1
            if rule not in bad_rules:
                problems.append(
                    f"{bad_fixture}: expected a `{rule}` finding, matched none"
                )
        good = by_file.get(good_fixture, [])
        good_total += len(good)
        for line, rule, code in good:
            problems.append(
                f"{good_fixture}:{line}: unexpected `{rule}` finding: {code}"
            )
    if problems:
        print("ERROR: guard self-test failed:")
        for problem in problems:
            print(f"  {problem}")
        print("  Fix the rule that stopped matching; do not silence the self-test.")
        return 1
    print(
        "error-handling guard: self-test ok "
        f"({required_total} required rule flag(s) matched their bad fixture, "
        f"{good_total} finding(s) in the good fixtures)"
    )
    return 0


def main():
    selftest = self_test()
    if selftest != 0:
        return selftest
    if "--self-test" in ARGS:
        return 0
    findings = scan()
    if PRINT_ALL:
        for rel, line, rule, code in findings:
            print(f"{rel}:{line}:{rule}: {code}")
        print(f"\n{len(findings)} finding(s) total (allowlist not applied)")
        return 0

    allow = load_allowlist()
    unallowed = []
    used = [False] * len(allow)
    for rel, line, rule, code in findings:
        hit = False
        for i, entry in enumerate(allow):
            if entry_matches(entry, rel, line, rule, code):
                used[i] = True
                hit = True
        if not hit:
            unallowed.append((rel, line, rule, code))

    for rel, line, rule, code in unallowed:
        print(f"ERROR: fallback/absence violation [{rule}] {rel}:{line}: {code}")

    stale = [
        f"{allow[i][0]}:{allow[i][1]}:{allow[i][2]}"
        for i in range(len(allow))
        if not used[i]
    ]
    if stale:
        print("")
        print(f"note: {len(stale)} allowlist entr(y/ies) no longer match (stale):")
        for key in sorted(stale):
            print(f"  {key}")

    if unallowed:
        print("")
        print(
            f"ERROR: {len(unallowed)} unallowlisted fallback(s)/absence(s) "
            f"(AGENTS §4); {len(findings)} site(s) checked, {len(allow)} "
            "allowlist entry/entries."
        )
        print("  Either fix the fallback or add a reviewed entry to")
        print(f"  {ALLOW.relative_to(ROOT)}")
        print("  Format: path:line:pattern — reason  (pattern is a rule name")
        print("  or a source-line anchor)")
        return 1

    print(
        f"error-handling guard: clean ({len(findings)} site(s) checked, "
        f"{len(allow)} allowlisted) — {ALLOW.relative_to(ROOT)}"
    )
    return 0


def run(argv=None):
    """Run the check. ``argv`` carries the check-specific flags."""
    global ARGS, PRINT_ALL
    ARGS = set(argv or [])
    PRINT_ALL = "--print-all" in ARGS
    return main()


if __name__ == "__main__":
    sys.exit(run(sys.argv[1:]))

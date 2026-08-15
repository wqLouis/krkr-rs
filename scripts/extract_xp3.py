#!/usr/bin/env python3
"""Extract a file from an XP3 archive (game reference data) to stdout.

Usage: extract_xp3.py <archive.xp3> <normalized-name> [--lines A:B]

Names are normalized (lowercase, '/' separators) like the engine does.
With --lines A:B prints only those 1-based lines of a text file (auto-decodes
UTF-8/UTF-16 BOMs, else CP932 like the engine's stream layer).
"""
import struct
import sys
import zlib


def open_index(path):
    f = open(path, "rb")
    f.seek(11)
    idx = struct.unpack("<Q", f.read(8))[0]
    f.seek(idx)
    flag = f.read(1)[0]
    if flag & 7 == 1:
        csz = struct.unpack("<Q", f.read(8))[0]
        rsz = struct.unpack("<Q", f.read(8))[0]
        index = zlib.decompress(f.read(csz))
    else:
        rsz = struct.unpack("<Q", f.read(8))[0]
        index = f.read(rsz)
    return f, index


def get_file(f, index, target):
    pos = 0
    while pos + 12 <= len(index):
        tag = index[pos : pos + 4]
        size = struct.unpack("<Q", index[pos + 4 : pos + 12])[0]
        if tag == b"File":
            payload = index[pos + 12 : pos + 12 + size]
            p2 = 0
            name = None
            segs = []
            while p2 + 12 <= len(payload):
                st = payload[p2 : p2 + 4]
                ss = struct.unpack("<Q", payload[p2 + 4 : p2 + 12])[0]
                if st == b"info":
                    info = payload[p2 + 12 : p2 + 12 + ss]
                    nlen = struct.unpack("<h", info[20:22])[0]
                    name = info[22 : 22 + nlen * 2].decode("utf-16-le").replace("\\", "/").lower()
                if st == b"segm":
                    for i in range(ss // 28):
                        s = payload[p2 + 12 + i * 28 : p2 + 12 + (i + 1) * 28]
                        m = struct.unpack("<I", s[0:4])[0] & 7
                        start = struct.unpack("<Q", s[4:12])[0]
                        o = struct.unpack("<Q", s[12:20])[0]
                        a = struct.unpack("<Q", s[20:28])[0]
                        segs.append((m, start, o, a))
                p2 += 12 + ss
            if name == target:
                out = b""
                for m, start, o, a in segs:
                    f.seek(start)
                    d = f.read(a)
                    out += zlib.decompress(d) if m == 1 else d
                return out
        pos += 12 + size
    return None


def decode(raw):
    if raw[:3] == b"\xef\xbb\xbf":
        return raw[3:].decode("utf-8", errors="replace")
    if raw[:2] in (b"\xff\xfe", b"\xfe\xff"):
        return raw[2:].decode("utf-16-le" if raw[:2] == b"\xff\xfe" else "utf-16-be", errors="replace")
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        return raw.decode("cp932", errors="replace")


def main():
    args = sys.argv[1:]
    if len(args) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    path, name = args[0], args[1].lower()
    lines = None
    if "--lines" in args:
        rng = args[args.index("--lines") + 1]
        a, b = rng.split(":")
        lines = (int(a), int(b))
    f, index = open_index(path)
    raw = get_file(f, index, name)
    if raw is None:
        print(f"error: {name} not found in {path}", file=sys.stderr)
        return 1
    if lines:
        text = decode(raw)
        for i, line in enumerate(text.splitlines(), 1):
            if lines[0] <= i <= lines[1]:
                print(f"{i}: {line}")
    else:
        sys.stdout.buffer.write(raw)
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Extract the scripts from an XP3 archive into a corpus directory (UTF-8).

Used by scripts/perf/bench-tjs-opt.sh to build the real-game script corpus for
the TJS2 compile benchmark. Scripts are decoded the way the engine does (UTF-8
/ UTF-16 BOM, else CP932) so the TJS2 compiler sees text, not raw bytes.

Usage: extract-scripts.py <archive.xp3> <out-dir> [--ks]
    --ks  also extract .ks scenario files (they are KAG, not TJS; the TJS2
          compiler rejects them, so they are only useful for documentation)
"""
import os
import struct
import sys
import zlib


def read_index(path):
    f = open(path, "rb")
    f.seek(11)
    idx = struct.unpack("<Q", f.read(8))[0]
    f.seek(idx)
    flag = f.read(1)[0]
    if flag & 7 == 1:
        csz = struct.unpack("<Q", f.read(8))[0]
        struct.unpack("<Q", f.read(8))[0]
        return f, zlib.decompress(f.read(csz))
    rsz = struct.unpack("<Q", f.read(8))[0]
    return f, f.read(rsz)


def decode(raw):
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        pass
    if raw[:2] == b"\xff\xfe":
        return raw[2:].decode("utf-16-le", "replace")
    if raw[:2] == b"\xfe\xff":
        return raw[2:].decode("utf-16-be", "replace")
    if raw[:3] == b"\xef\xbb\xbf":
        return raw[3:].decode("utf-8", "replace")
    return raw.decode("cp932", "replace")


def main():
    args = sys.argv[1:]
    if len(args) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    archive, out_dir = args[0], args[1]
    want_ks = "--ks" in args
    f, index = read_index(archive)
    count = 0
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
                    name = (
                        info[22 : 22 + nlen * 2]
                        .decode("utf-16-le")
                        .replace("\\", "/")
                        .lower()
                    )
                if st == b"segm":
                    for i in range(ss // 28):
                        s = payload[p2 + 12 + i * 28 : p2 + 12 + (i + 1) * 28]
                        mode = struct.unpack("<I", s[0:4])[0] & 7
                        start = struct.unpack("<Q", s[4:12])[0]
                        offset = struct.unpack("<Q", s[12:20])[0]
                        length = struct.unpack("<Q", s[20:28])[0]
                        segs.append((mode, start, offset, length))
                p2 += 12 + ss
            if name and (name.endswith(".tjs") or (want_ks and name.endswith(".ks"))):
                data = b""
                for mode, start, _offset, length in segs:
                    f.seek(start)
                    chunk = f.read(length)
                    data += zlib.decompress(chunk) if mode == 1 else chunk
                path = os.path.join(out_dir, name)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                with open(path, "w", encoding="utf-8") as out:
                    out.write(decode(data))
                count += 1
        pos += 12 + size
    print(f"extracted {count} scripts into {out_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

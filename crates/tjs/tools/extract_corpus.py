#!/usr/bin/env python3
"""Extracts the game's scripts into a corpus directory.

The corpus is the set of `.tjs` scripts a game ships. It is not in this
repository — the game is not either — so the corpus is rebuilt from the
game's own archives with this script, and the corpus check
(`cargo run -p tjs --example corpus_digest -- --corpus <directory>`) is
run against the extracted corpus.

    usage: extract_corpus.py <game directory> [corpus directory]

The game directory is the directory the game lives in. Every `*.xp3`
archive under it is read, in the order the archives are found (the path
by file name, case-insensitively, then the shallower path first), and
every `.tjs` file in them is written into the corpus directory, under the
path the archive holds it at. The corpus directory defaults to
`corpus` beside the game directory.

The XP3 index is a chain: each block's flag byte carries INDEX_CONTINUE
(0x80), and when it is set the next index offset follows the block's
data.
"""

import os
import struct
import sys
import zlib

INDEX_ENCODE_METHOD_MASK = 0x07
INDEX_ENCODE_RAW = 0
INDEX_ENCODE_ZLIB = 1
INDEX_CONTINUE = 0x80


def read_index_chain(f):
    f.seek(11)
    offset = struct.unpack("<Q", f.read(8))[0]
    while offset != 0:
        f.seek(offset)
        flag = f.read(1)[0]
        method = flag & INDEX_ENCODE_METHOD_MASK
        if method == INDEX_ENCODE_ZLIB:
            csize = struct.unpack("<Q", f.read(8))[0]
            rsize = struct.unpack("<Q", f.read(8))[0]
            data = zlib.decompress(f.read(csize))
            if len(data) != rsize:
                raise SystemExit("index size mismatch")
        elif method == INDEX_ENCODE_RAW:
            size = struct.unpack("<Q", f.read(8))[0]
            data = f.read(size)
        else:
            raise SystemExit(f"unknown index method {method}")
        yield data
        if flag & INDEX_CONTINUE == 0:
            break
        offset = struct.unpack("<Q", f.read(8))[0]


def chunks(data):
    pos = 0
    while pos + 12 <= len(data):
        tag = data[pos : pos + 4]
        size = struct.unpack("<Q", data[pos + 4 : pos + 12])[0]
        yield tag, data[pos + 12 : pos + 12 + size]
        pos += 12 + size


def entries(data):
    for tag, payload in chunks(data):
        if tag != b"File":
            continue
        name = None
        segs = []
        for stag, sdata in chunks(payload):
            if stag == b"info":
                nlen = struct.unpack("<h", sdata[20:22])[0]
                name = sdata[22 : 22 + nlen * 2].decode("utf-16-le").replace("\\", "/").lower()
            elif stag == b"segm":
                for i in range(len(sdata) // 28):
                    s = sdata[i * 28 : (i + 1) * 28]
                    m = struct.unpack("<I", s[0:4])[0] & 7
                    start = struct.unpack("<Q", s[4:12])[0]
                    o = struct.unpack("<Q", s[12:20])[0]
                    a = struct.unpack("<Q", s[20:28])[0]
                    segs.append((m, start, o, a))
        if name is not None:
            yield name, segs


def extract(f, segs):
    out = b""
    for m, start, o, a in segs:
        f.seek(start)
        d = f.read(a)
        out += zlib.decompress(d) if m == 1 else d
    return out


def archives(game):
    """The archives of the game, in the order they are read."""
    found = []
    for root, directories, files in os.walk(game):
        directories.sort()
        for name in sorted(files, key=str.lower):
            if name.lower().endswith(".xp3"):
                found.append(os.path.join(root, name))
    # the shallower path first, then the name
    found.sort(key=lambda path: (path.count(os.sep), path.lower()))
    return found


def main():
    if len(sys.argv) < 2:
        raise SystemExit(
            "usage: extract_corpus.py <game directory> [corpus directory]"
        )
    game = sys.argv[1]
    if not os.path.isdir(game):
        raise SystemExit(f"the game directory is missing: {game}")
    outdir = sys.argv[2] if len(sys.argv) > 2 else os.path.join(game, "corpus")

    paths = archives(game)
    if not paths:
        raise SystemExit(f"the game directory holds no archive: {game}")

    os.makedirs(outdir, exist_ok=True)
    count = 0
    total = 0
    seen = set()
    for path in paths:
        with open(path, "rb") as f:
            for data in read_index_chain(f):
                for name, segs in entries(data):
                    if not name.endswith(".tjs"):
                        continue
                    rel = name[2:] if name.startswith("./") else name
                    if rel in seen:
                        continue
                    seen.add(rel)
                    raw = extract(f, segs)
                    dest = os.path.join(outdir, rel)
                    os.makedirs(os.path.dirname(dest), exist_ok=True)
                    with open(dest, "wb") as out:
                        out.write(raw)
                    count += 1
                    total += len(raw)
    print(
        f"archives={len(paths)} files={count} bytes={total} "
        f"corpus={outdir}"
    )


if __name__ == "__main__":
    main()

// Minimal file-backed streams for the TJS2 save/load path.
//
// The reference wires `TJSCreate{Text,Binary}StreamFor{Read,Write}` to the
// base module's storage-backed implementations (ScriptMgnIntf.cpp:465);
// krkr-rs compiles only the tjs2 core, so those pointers stay NULL and
// `Array.saveStruct/loadStruct` crash. These factories provide the same
// surface with plain filesystem I/O in the game directory:
//
//   * write: UTF-16LE text (the reference's tTVPTextWriteStream writes
//     tjs_char units as-is), no BOM, no compression ("z" mode is accepted
//     and ignored — documented milestone simplification; round-trips within
//     krkr-rs are consistent).
//   * read: BOM detection (UTF-16LE/BE, UTF-8) + UTF-16LE fallback for
//     BOM-less payloads (the reference's `(const) [...]` save format).
//
// Paths: names are absolute (the game builds them from System.dataPath) or
// resolved against the data dir set via tjs2_set_data_dir.

#include "tjs2_abi.h"

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include <tjs.h>

static std::string g_data_dir;

void tjs2_set_data_dir(const char *dir) {
    g_data_dir = dir ? dir : "";
}

// Resolve a storage-ish name to a filesystem path.
static std::string resolve_path(const ttstr &name) {
    std::string s = name.AsNarrowStdString();
    if(s.empty())
        return s;
    if(s[0] == '/' || s[0] == '\\')
        return s; // absolute
    if(s.find(":/") != std::string::npos || s.find(":") == 1)
        return s; // drive letter
    if(!g_data_dir.empty())
        return g_data_dir + "/" + s;
    return s;
}

// ---------------------------------------------------------------------------
// Binary stream (file-backed)
// ---------------------------------------------------------------------------
class FileBinaryStream : public TJS::tTJSBinaryStream {
    FILE *f;
    bool writable;
    bool append;

public:
    FileBinaryStream(const ttstr &name, const ttstr &mode) : f(nullptr), writable(false), append(false) {
        std::string path = resolve_path(name);
        std::string m = mode.AsNarrowStdString();
        writable = m.find('w') != std::string::npos;
        append = m.find('a') != std::string::npos;
        if(writable) {
            f = std::fopen(path.c_str(), append ? "ab" : "wb");
        } else {
            f = std::fopen(path.c_str(), "rb");
        }
    }

    ~FileBinaryStream() override {
        if(f)
            std::fclose(f);
    }

    tjs_uint64 Seek(tjs_int64 offset, tjs_int whence) override {
        if(!f)
            return 0;
        if(std::fseek(f, (long)offset, whence) != 0)
            return 0;
        return GetPosition();
    }

    tjs_uint Read(void *buffer, tjs_uint read_size) override {
        if(!f || writable)
            return 0;
        return (tjs_uint)std::fread(buffer, 1, read_size, f);
    }

    tjs_uint Write(const void *buffer, tjs_uint write_size) override {
        if(!f || !writable)
            return 0;
        return (tjs_uint)std::fwrite(buffer, 1, write_size, f);
    }

    tjs_uint64 GetSize() override {
        if(!f)
            return 0;
        long cur = std::ftell(f);
        std::fseek(f, 0, SEEK_END);
        long end = std::ftell(f);
        std::fseek(f, cur, SEEK_SET);
        return end;
    }
};

// ---------------------------------------------------------------------------
// Text streams
// ---------------------------------------------------------------------------

// UTF-8 -> UTF-16 helper.
static std::vector<char16_t> utf8_to_utf16(const char *s, size_t len) {
    std::vector<char16_t> out;
    size_t i = 0;
    while(i < len) {
        unsigned char c = s[i];
        if(c < 0x80) {
            out.push_back((char16_t)c);
            i++;
        } else if((c & 0xE0) == 0xC0 && i + 1 < len) {
            out.push_back((char16_t)(((c & 0x1F) << 6) | (s[i + 1] & 0x3F)));
            i += 2;
        } else if((c & 0xF0) == 0xE0 && i + 2 < len) {
            out.push_back((char16_t)(((c & 0x0F) << 12) | ((s[i + 1] & 0x3F) << 6) | (s[i + 2] & 0x3F)));
            i += 3;
        } else {
            i++; // skip invalid
        }
    }
    return out;
}
class TextWriteStream : public TJS::iTJSTextWriteStream {
    FileBinaryStream bin;

public:
    TextWriteStream(const ttstr &name, const ttstr &mode) : bin(name, mode) {}

    void Write(const TJS::tTJSString &targ) override {
        // tjs_char == char16_t; write the UTF-16LE bytes.
        bin.Write(targ.c_str(), targ.GetLen() * sizeof(tjs_char));
    }

    void Destruct() override { delete this; }
};

class TextReadStream : public TJS::iTJSTextReadStream {
    std::vector<char16_t> text; // decoded contents
    size_t pos;
    std::string path;

public:
    TextReadStream(const ttstr &name, const ttstr &mode)
        : pos(0), path(resolve_path(name)) {
        FILE *f = std::fopen(path.c_str(), "rb");
        if(!f)
            return; // empty stream: Read yields nothing
        std::vector<unsigned char> raw;
        unsigned char buf[4096];
        size_t n;
        while((n = std::fread(buf, 1, sizeof(buf), f)) > 0)
            raw.insert(raw.end(), buf, buf + n);
        std::fclose(f);

        if(raw.size() >= 3 && raw[0] == 0xFE && raw[1] == 0xFE) {
            // FE FE encrypted/compressed script data — the Scripts native
            // handles that path (tvp-scripts decompress_script); here we
            // return empty so saveStruct/loadStruct produce defaults rather
            // than crash.
            return;
        }

        size_t off = 0;
        if(raw.size() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE) {
            off = 2; // UTF-16LE BOM
            for(size_t i = off; i + 1 < raw.size(); i += 2)
                text.push_back((char16_t)(raw[i] | (raw[i + 1] << 8)));
        } else if(raw.size() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF) {
            off = 2; // UTF-16BE BOM
            for(size_t i = off; i + 1 < raw.size(); i += 2)
                text.push_back((char16_t)((raw[i] << 8) | raw[i + 1]));
        } else if(raw.size() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF) {
            off = 3; // UTF-8 BOM
            text = utf8_to_utf16((const char *)raw.data() + off, raw.size() - off);
        } else if(!raw.empty()) {
            // BOM-less: try UTF-16LE (even length) else UTF-8.
            if(raw.size() % 2 == 0) {
                bool looks_utf16 = raw.size() >= 2;
                if(looks_utf16)
                    for(size_t i = 0; i + 1 < raw.size(); i += 2)
                        text.push_back((char16_t)(raw[i] | (raw[i + 1] << 8)));
            } else {
                text = utf8_to_utf16((const char *)raw.data(), raw.size());
            }
        }
    }

    tjs_uint Read(TJS::tTJSString &targ, tjs_uint size) override {
        tjs_uint n = (tjs_uint)(text.size() - pos);
        if(n > size)
            n = size;
        targ = TJS::tTJSString(text.data() + pos, n);
        pos += n;
        return n;
    }

    void Destruct() override { delete this; }
};

// ---------------------------------------------------------------------------
// Factories
// ---------------------------------------------------------------------------
static TJS::iTJSTextReadStream *create_text_read(const ttstr &name, const ttstr &mode) {
    return new TextReadStream(name, mode);
}

static TJS::iTJSTextWriteStream *create_text_write(const ttstr &name, const ttstr &mode) {
    return new TextWriteStream(name, mode);
}

static TJS::tTJSBinaryStream *create_bin_read(const ttstr &name, const ttstr &mode) {
    return new FileBinaryStream(name, "r");
}

static TJS::tTJSBinaryStream *create_bin_write(const ttstr &name, const ttstr &mode) {
    return new FileBinaryStream(name, mode);
}

// Called from tjs2_create: wire the stream factories so Array/Dictionary
// saveStruct/loadStruct do not crash.
void tjs2_wire_stream_factories() {
    TJS::TJSCreateTextStreamForRead = create_text_read;
    TJS::TJSCreateTextStreamForWrite = create_text_write;
    TJS::TJSCreateBinaryStreamForRead = create_bin_read;
    TJS::TJSCreateBinaryStreamForWrite = create_bin_write;
}



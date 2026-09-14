// File-backed TJS2 text/binary streams for the save/load path.
//
// The reference wires `TJSCreate{Text,Binary}StreamFor{Read,Write}` to the
// base module's storage-backed implementations (ScriptMgnIntf.cpp); krkr-rs
// compiles only the tjs2 core, so those pointers stay NULL and
// `Array/Dictionary.saveStruct/loadStruct` (and KAG's quick save/load) crash
// or silently truncate. These factories provide the same surface with plain
// filesystem I/O in the game directory.
//
// This is a faithful port of the reference's text streams
// (`reference/cpp/core/base/TextStream.cpp`) and binary-stream mode handling
// (`reference/cpp/core/base/BinaryStream.cpp`):
//
//   * write mode `oN`  -> open the file at byte offset N (UPDATE + seek, so
//                         the leading bytes, e.g. a KAG BMP thumbnail, are
//                         preserved); without `o` the file is created or
//                         truncated (WRITE).
//   * write mode `cN`  -> crypt mode N (N is a single digit, 0/1/2).
//   * write mode `zN`  -> zlib compression (crypt mode 2) at level N.
//   * neither `c` nor `z` -> zlib compressed (crypt mode 2). This is the
//                         default the real KAG engine uses for its
//                         `"o<byteOffset>"` saves (the reference's
//                         `tTVPTextWriteStream` falls into `_cryptMode = 2`
//                         when no `z` is given).
//   * read mode `oN`   -> start reading at byte offset N.
//
// Text payload (`FE FE <mode>` crypt container, after the optional 3-byte
// signature and the UTF-16LE BOM `FF FE`):
//   mode 0: per-char XOR cipher (old buggy crypt mode)
//   mode 1: per-char bit swap
//   mode 2: zlib stream laid out as `[u64 compressed][u64 uncompressed]`
//           followed by the compressed UTF-16LE bytes.
//
// Paths: names are absolute (the game builds them from System.dataPath) or
// resolved against the data dir set via tjs2_set_data_dir.

#include "tjs2_abi.h"

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

#include <zlib.h>

#include <tjs.h>

static std::string g_data_dir;

void tjs2_set_data_dir(const char *dir) {
    g_data_dir = dir ? dir : "";
}

// Resolve a storage-ish name to a filesystem path.
static std::string resolve_path_str(const std::string &s) {
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

static std::string resolve_path(const ttstr &name) {
    return resolve_path_str(name.AsNarrowStdString());
}

// ---------------------------------------------------------------------------
// Mode-string parsing
// ---------------------------------------------------------------------------

// The reference's `parseModeNumber` (BinaryStream.h): find `key` and, if it
// is directly followed by 1..max_digits decimal digits, parse them. Returns
// false when the key is absent or has no number — the caller applies the
// default it wants (`o` -> 0, `c` -> 2/1, `z` -> mode 2).
static bool parse_mode_number(const std::string &mode, char key, int max_digits,
                              long long &out) {
    size_t pos = mode.find(key);
    if(pos == std::string::npos)
        return false;
    if(pos + 1 >= mode.size() || mode[pos + 1] < '0' || mode[pos + 1] > '9')
        return false;
    long long value = 0;
    size_t i = pos + 1;
    for(int n = 0; n < max_digits && i < mode.size() && mode[i] >= '0' &&
                    mode[i] <= '9';
        ++n, ++i)
        value = value * 10 + (mode[i] - '0');
    out = value;
    return true;
}

static long long mode_offset(const std::string &mode) {
    long long ofs = 0;
    parse_mode_number(mode, 'o', 255, ofs);
    return ofs;
}

// ---------------------------------------------------------------------------
// Binary stream (file-backed)
// ---------------------------------------------------------------------------

enum class FileAccess { Read, Write, Append, Update };

static int seek_file(FILE *f, int64_t offset, int whence) {
#if defined(_WIN32)
    return _fseeki64(f, offset, whence);
#else
    return fseeko(f, (off_t)offset, whence);
#endif
}

static int64_t tell_file(FILE *f) {
#if defined(_WIN32)
    return _ftelli64(f);
#else
    return (int64_t)ftello(f);
#endif
}

static FILE *open_file(const std::string &path, FileAccess access) {
    switch(access) {
        case FileAccess::Read:
            return std::fopen(path.c_str(), "rb");
        case FileAccess::Write:
            return std::fopen(path.c_str(), "wb"); // create / truncate
        case FileAccess::Append:
            return std::fopen(path.c_str(), "ab");
        case FileAccess::Update: {
            // TJS_BS_UPDATE: open an existing file read+write; fall back to
            // creating it so a missing offset target cannot fail the save.
            FILE *f = std::fopen(path.c_str(), "r+b");
            if(!f)
                f = std::fopen(path.c_str(), "w+b");
            return f;
        }
    }
    return nullptr;
}

class FileBinaryStream : public TJS::tTJSBinaryStream {
    FILE *f;
    bool readable;
    bool writable;

public:
    FileBinaryStream(const std::string &path, FileAccess access)
        : f(open_file(path, access)),
          readable(access == FileAccess::Read || access == FileAccess::Update),
          writable(access != FileAccess::Read) {}

    ~FileBinaryStream() override {
        if(f)
            std::fclose(f);
    }

    bool ok() const { return f != nullptr; }

    // NOTE: the base class implements GetPosition() as Seek(0, SEEK_CUR), so
    // Seek must return the position itself instead of calling GetPosition()
    // (which would recurse forever).
    tjs_uint64 Seek(tjs_int64 offset, tjs_int whence) override {
        if(!f)
            return 0;
        seek_file(f, offset, whence);
        int64_t p = tell_file(f);
        return p < 0 ? 0 : (tjs_uint64)p;
    }

    tjs_uint Read(void *buffer, tjs_uint read_size) override {
        if(!f || !readable)
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
        int64_t cur = tell_file(f);
        if(seek_file(f, 0, SEEK_END) != 0)
            return 0;
        int64_t end = tell_file(f);
        if(cur >= 0)
            seek_file(f, cur, SEEK_SET);
        return end < 0 ? 0 : (tjs_uint64)end;
    }
};

// ---------------------------------------------------------------------------
// Text stream helpers
// ---------------------------------------------------------------------------

static uint64_t read_u64le(const uint8_t *p) {
    uint64_t v = 0;
    for(int i = 0; i < 8; i++)
        v |= (uint64_t)p[i] << (i * 8);
    return v;
}

static void write_u64le(TJS::tTJSBinaryStream &stream, uint64_t v) {
    uint8_t buf[8];
    for(int i = 0; i < 8; i++)
        buf[i] = (uint8_t)(v >> (i * 8));
    stream.Write(buf, 8);
}

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
            out.push_back((char16_t)(((c & 0x0F) << 12) |
                                     ((s[i + 1] & 0x3F) << 6) |
                                     (s[i + 2] & 0x3F)));
            i += 3;
        } else if((c & 0xF8) == 0xF0 && i + 3 < len) {
            unsigned int cp = ((c & 0x07) << 18) | ((s[i + 1] & 0x3F) << 12) |
                              ((s[i + 2] & 0x3F) << 6) | (s[i + 3] & 0x3F);
            cp -= 0x10000;
            out.push_back((char16_t)(0xD800 + (cp >> 10)));
            out.push_back((char16_t)(0xDC00 + (cp & 0x3FF)));
            i += 4;
        } else {
            i++; // skip invalid
        }
    }
    return out;
}

// UTF-16 -> UTF-8 helper (used by the diagnostic read helper).
static std::string utf16_to_utf8(const char16_t *s, size_t len) {
    std::string out;
    for(size_t i = 0; i < len; i++) {
        uint32_t cp = s[i];
        if(cp >= 0xD800 && cp <= 0xDBFF && i + 1 < len) {
            uint32_t low = s[i + 1];
            if(low >= 0xDC00 && low <= 0xDFFF) {
                cp = 0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
                i++;
            }
        }
        if(cp < 0x80) {
            out.push_back((char)cp);
        } else if(cp < 0x800) {
            out.push_back((char)(0xC0 | (cp >> 6)));
            out.push_back((char)(0x80 | (cp & 0x3F)));
        } else if(cp < 0x10000) {
            out.push_back((char)(0xE0 | (cp >> 12)));
            out.push_back((char)(0x80 | ((cp >> 6) & 0x3F)));
            out.push_back((char)(0x80 | (cp & 0x3F)));
        } else {
            out.push_back((char)(0xF0 | (cp >> 18)));
            out.push_back((char)(0x80 | ((cp >> 12) & 0x3F)));
            out.push_back((char)(0x80 | ((cp >> 6) & 0x3F)));
            out.push_back((char)(0x80 | (cp & 0x3F)));
        }
    }
    return out;
}

// ---------------------------------------------------------------------------
// Text read stream
// ---------------------------------------------------------------------------

class TextReadStream : public TJS::iTJSTextReadStream {
    std::vector<char16_t> text; // decoded contents
    size_t pos;

public:
    TextReadStream(const std::string &path, const std::string &mode)
        : pos(0) {
        long long ofs = mode_offset(mode);
        FileBinaryStream stream(path, FileAccess::Read);
        if(!stream.ok())
            return;
        uint64_t size = stream.GetSize();
        if(ofs < 0 || (uint64_t)ofs > size)
            return; // offset past the end: empty stream
        stream.SetPosition((tjs_uint64)ofs);
        size -= (uint64_t)ofs;

        std::vector<uint8_t> raw((size_t)size);
        if(size > 0)
            stream.Read(raw.data(), (tjs_uint)size);

        if(raw.size() >= 3 && raw[0] == 0xFE && raw[1] == 0xFE) {
            uint8_t m = raw[2];
            if(m == 0 || m == 1) {
                // Signature `FE FE m`, then the BOM `FF FE`, then UTF-16LE.
                // (Tolerate a missing BOM as the reference did not enforce
                // it for the cipher modes.)
                size_t start = 3;
                if(raw.size() >= 5 && raw[3] == 0xFF && raw[4] == 0xFE)
                    start = 5;
                for(size_t i = start; i + 1 < raw.size(); i += 2) {
                    char16_t ch = (char16_t)(raw[i] | (raw[i + 1] << 8));
                    if(m == 0) {
                        if(ch >= 0x20)
                            ch ^= (char16_t)(((ch & 0xfe) << 8) ^ 1);
                    } else {
                        ch = (char16_t)(((ch >> 1) & 0x5555) |
                                        ((ch << 1) & 0xaaaa));
                    }
                    text.push_back(ch);
                }
                return;
            }
            if(m == 2) {
                // `FE FE 02 FF FE [u64 compressed][u64 uncompressed][data]`
                if(raw.size() < 5 + 16)
                    return;
                uint64_t compressed = read_u64le(raw.data() + 5);
                uint64_t uncompressed = read_u64le(raw.data() + 13);
                size_t payload = 5 + 16;
                if(compressed > raw.size() - payload)
                    return;
                std::vector<uint8_t> out((size_t)uncompressed);
                uLongf dest_len = (uLongf)uncompressed;
                int ret = uncompress(out.data(), &dest_len, raw.data() + payload,
                                     (uLong)compressed);
                if(ret != Z_OK || dest_len != uncompressed)
                    return;
                text.resize((size_t)uncompressed / sizeof(char16_t));
                std::memcpy(text.data(), out.data(), (size_t)uncompressed);
                return;
            }
            return; // unsupported mode
        }

        // Plain text: BOM detection (UTF-16LE/BE, UTF-8) + UTF-16LE
        // fallback for BOM-less `(const) [...]` payloads.
        size_t off = 0;
        if(raw.size() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE) {
            off = 2; // UTF-16LE BOM
            for(size_t i = off; i + 1 < raw.size(); i += 2)
                text.push_back((char16_t)(raw[i] | (raw[i + 1] << 8)));
        } else if(raw.size() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF) {
            off = 2; // UTF-16BE BOM
            for(size_t i = off; i + 1 < raw.size(); i += 2)
                text.push_back((char16_t)((raw[i] << 8) | raw[i + 1]));
        } else if(raw.size() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB &&
                  raw[2] == 0xBF) {
            off = 3; // UTF-8 BOM
            text = utf8_to_utf16((const char *)raw.data() + off,
                                 raw.size() - off);
        } else if(!raw.empty()) {
            // BOM-less: try UTF-16LE (even length) else UTF-8.
            if(raw.size() % 2 == 0) {
                for(size_t i = 0; i + 1 < raw.size(); i += 2)
                    text.push_back((char16_t)(raw[i] | (raw[i + 1] << 8)));
            } else {
                text = utf8_to_utf16((const char *)raw.data(), raw.size());
            }
        }
    }

    tjs_uint Read(TJS::tTJSString &targ, tjs_uint size) override {
        size_t remaining = text.size() - pos;
        // size == 0 means "read everything left" (reference Read()).
        size_t n = (size == 0 || (size_t)size > remaining) ? remaining
                                                           : (size_t)size;
        if(n == 0) {
            targ.Clear();
            return 0;
        }
        targ = TJS::tTJSString(text.data() + pos, n);
        pos += n;
        return (tjs_uint)n;
    }

    void Destruct() override { delete this; }
};

// ---------------------------------------------------------------------------
// Text write stream
// ---------------------------------------------------------------------------

class TextWriteStream : public TJS::iTJSTextWriteStream {
    static constexpr size_t COMPRESSION_BUFFER_SIZE = 1024 * 1024;

    std::unique_ptr<FileBinaryStream> stream;
    int crypt_mode;
    int compression_level;
    z_stream zs;
    bool zs_init;
    bool compression_failed;
    int64_t compression_size_position;
    std::vector<Bytef> compression_buffer;

    void write_raw(const void *ptr, size_t size) {
        if(crypt_mode != 2) {
            stream->Write(ptr, (tjs_uint)size);
            return;
        }
        zs.next_in = (Bytef *)ptr;
        zs.avail_in = (uInt)size;
        while(zs.avail_in > 0) {
            int ret = deflate(&zs, Z_NO_FLUSH);
            if(ret != Z_OK) {
                compression_failed = true;
                return;
            }
            if(zs.avail_out == 0) {
                stream->Write(compression_buffer.data(), COMPRESSION_BUFFER_SIZE);
                zs.next_out = compression_buffer.data();
                zs.avail_out = COMPRESSION_BUFFER_SIZE;
            }
        }
    }

public:
    TextWriteStream(const std::string &path, const std::string &mode)
        : crypt_mode(2), // default: zlib compressed (real KAG `"oN"` saves)
          compression_level(Z_DEFAULT_COMPRESSION),
          zs(),
          zs_init(false),
          compression_failed(false),
          compression_size_position(0),
          compression_buffer(COMPRESSION_BUFFER_SIZE) {
        long long v = 0;
        if(parse_mode_number(mode, 'c', 1, v))
            crypt_mode = (int)v; // c0 / c1 / c2
        if(parse_mode_number(mode, 'z', 1, v)) {
            crypt_mode = 2; // zN -> zlib at level N
            compression_level = (int)v;
        }
        if(crypt_mode < 0 || crypt_mode > 2)
            crypt_mode = 2;

        long long ofs = mode_offset(mode);
        FileAccess access =
            ofs != 0 ? FileAccess::Update : FileAccess::Write;
        stream = std::make_unique<FileBinaryStream>(path, access);
        if(!stream->ok())
            return;
        if(ofs != 0)
            stream->SetPosition((tjs_uint64)ofs);

        // Crypt signature: written for every active crypt mode (0/1/2).
        uint8_t sig[3] = {0xFE, 0xFE, (uint8_t)crypt_mode};
        stream->Write(sig, 3);
        // Now the text stream writes unicode text.
        uint8_t bom[2] = {0xFF, 0xFE};
        stream->Write(bom, 2);

        if(crypt_mode == 2) {
            zs.zalloc = Z_NULL;
            zs.zfree = Z_NULL;
            zs.opaque = Z_NULL;
            if(deflateInit(&zs, compression_level) != Z_OK) {
                compression_failed = true;
                return;
            }
            zs_init = true;
            zs.next_in = nullptr;
            zs.avail_in = 0;
            zs.next_out = compression_buffer.data();
            zs.avail_out = COMPRESSION_BUFFER_SIZE;

            // Compression sizes (dummy placeholders, patched in dtor).
            compression_size_position = (int64_t)stream->GetPosition();
            write_u64le(*stream, 0);
            write_u64le(*stream, 0);
        }
    }

    ~TextWriteStream() override {
        if(!stream || !stream->ok())
            return;
        if(zs_init && !compression_failed) {
            int result = 0;
            do {
                result = deflate(&zs, Z_FINISH);
                if(result != Z_OK && result != Z_STREAM_END) {
                    compression_failed = true;
                    break;
                }
                stream->Write(compression_buffer.data(),
                              COMPRESSION_BUFFER_SIZE - zs.avail_out);
                zs.next_out = compression_buffer.data();
                zs.avail_out = COMPRESSION_BUFFER_SIZE;
            } while(result != Z_STREAM_END);

            if(!compression_failed) {
                stream->SetPosition((tjs_uint64)compression_size_position);
                write_u64le(*stream, zs.total_out);
                write_u64le(*stream, zs.total_in);
            }
        }
        if(zs_init)
            deflateEnd(&zs);
    }

    void Write(const TJS::tTJSString &targ) override {
        tjs_int len = targ.GetLen();
        if(len <= 0)
            return;
        std::vector<char16_t> buf((size_t)len);
        const tjs_char *src = targ.c_str();
        for(tjs_int i = 0; i < len; i++) {
            char16_t ch = (char16_t)src[i];
            if(crypt_mode == 1) {
                ch = (char16_t)(((ch >> 1) & 0x5555) | ((ch << 1) & 0xaaaa));
            } else if(crypt_mode == 0) {
                if(ch >= 0x20)
                    ch ^= (char16_t)(((ch & 0xfe) << 8) ^ 1);
            }
            buf[(size_t)i] = ch;
        }
        write_raw(buf.data(), (size_t)len * sizeof(char16_t));
    }

    void Destruct() override { delete this; }
};

// ---------------------------------------------------------------------------
// Factories
// ---------------------------------------------------------------------------
static TJS::iTJSTextReadStream *create_text_read(const ttstr &name,
                                                 const ttstr &mode) {
    return new TextReadStream(resolve_path(name), mode.AsNarrowStdString());
}

static TJS::iTJSTextWriteStream *create_text_write(const ttstr &name,
                                                   const ttstr &mode) {
    return new TextWriteStream(resolve_path(name), mode.AsNarrowStdString());
}

static TJS::tTJSBinaryStream *create_bin_read(const ttstr &name,
                                              const ttstr &mode) {
    std::string m = mode.AsNarrowStdString();
    auto *s = new FileBinaryStream(resolve_path(name), FileAccess::Read);
    long long ofs = mode_offset(m);
    if(ofs != 0)
        s->SetPosition((tjs_uint64)ofs);
    return s;
}

static TJS::tTJSBinaryStream *create_bin_write(const ttstr &name,
                                               const ttstr &mode) {
    std::string m = mode.AsNarrowStdString();
    FileAccess access;
    if(m.find('a') != std::string::npos) {
        access = FileAccess::Append;
    } else {
        long long ofs = mode_offset(m);
        // KAG's `"o<ofs>"` must update an existing file (a BMP prefix),
        // never truncate it.
        access = ofs != 0 ? FileAccess::Update : FileAccess::Write;
    }
    auto *s = new FileBinaryStream(resolve_path(name), access);
    long long ofs = mode_offset(m);
    if(ofs != 0)
        s->SetPosition((tjs_uint64)ofs);
    return s;
}

// Called from tjs2_create: wire the stream factories so Array/Dictionary
// saveStruct/loadStruct do not crash.
void tjs2_wire_stream_factories() {
    TJS::TJSCreateTextStreamForRead = create_text_read;
    TJS::TJSCreateTextStreamForWrite = create_text_write;
    TJS::TJSCreateBinaryStreamForRead = create_bin_read;
    TJS::TJSCreateBinaryStreamForWrite = create_bin_write;
}

// ---------------------------------------------------------------------------
// Diagnostic/test helper (declared in crates/tjs2-sys/src/lib.rs)
// ---------------------------------------------------------------------------

// Read a whole text stream (honoring the mode's `oN` offset and the `FE FE`
// crypt container) and return the decoded text as a malloc'd UTF-8 string
// (free with `tjs2_free_string`). Returns 0 on success, non-zero on failure.
extern "C" int tjs2_read_text_stream_all(const char *path_utf8,
                                         const char *mode_utf8,
                                         char **out_utf8) {
    if(out_utf8)
        *out_utf8 = nullptr;
    if(!path_utf8 || !out_utf8)
        return 1;
    try {
        std::string path = resolve_path_str(path_utf8);
        std::string mode = mode_utf8 ? mode_utf8 : "";
        TextReadStream stream(path, mode);
        TJS::tTJSString content;
        stream.Read(content, 0);
        std::string utf8 =
            utf16_to_utf8(content.c_str(), (size_t)content.GetLen());
        char *buf = (char *)std::malloc(utf8.size() + 1);
        if(!buf)
            return 1;
        std::memcpy(buf, utf8.data(), utf8.size());
        buf[utf8.size()] = 0;
        *out_utf8 = buf;
        return 0;
    } catch(...) {
        return 1;
    }
}

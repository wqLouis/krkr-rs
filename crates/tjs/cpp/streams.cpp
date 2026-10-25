// File-backed TJS2 text/binary streams for the save/load path.
//
// The reference wires `TJSCreate{Text,Binary}StreamFor{Read,Write}` to the
// base module's storage-backed implementations (ScriptMgnIntf.cpp:464-469):
// every name, including `archive.xp3>member`, resolves through the storage
// layer, and a name the storage cannot place raises `TVPCannotOpenStorage`
// (StorageIntf.cpp:1131-1148). krkr-rs compiles only the tjs2 core, so the
// storage lives in the compatibility layer; it installs one read callback
// (tjs2_set_storage_read_callback) that the read factories consult before the
// local filesystem, while the write factories stay filesystem-only.
//
// This is a faithful port of the reference's text streams
// (`reference/cpp/core/base/TextStream.cpp`) and binary-stream mode handling
// (`reference/cpp/core/base/BinaryStream.cpp`):
//
//   * write mode `oN`  -> open the file at byte offset N (UPDATE + seek, so
//                         the leading bytes, e.g. a KAG BMP thumbnail, are
//                         preserved); without `o` the file is created or
//                         truncated (WRITE).
//   * write mode `cN`  -> cipher mode N (N is a single digit): `c1` is the
//                         simple cipher, `c2` the compressed container. A
//                         cipher mode outside {-1, 1, 2} raises the
//                         unsupported-mode-string error.
//   * write mode `c`   -> cipher mode 1 (a `c` without a digit).
//   * write mode `z`   -> the compressed container (cipher mode 2) at zlib
//                         level 0 (a `z` without a digit).
//   * write mode `zN`  -> zlib level N. The cipher mode stays what the `c`
//                         key decided, so `zN` on its own does not select the
//                         container.
//   * neither `c` nor `z` -> plain UTF-16 text with a byte-order mark and no
//                         container.
//   * read mode `oN`   -> start reading at byte offset N.
//
// Text payload (`FE FE <mode>` crypt container):
//   modes 0/1 and 2 share the five-byte header `FE FE <mode> FF FE`. The
//   reference reads the two bytes after the mode as the mandatory UTF-16LE
//   byte-order mark and starts the mode 0/1 payload at offset 5, raising
//   `TVPUnsupportedCipherMode` when the mark is absent. The vendored
//   reconstruction's offset-4 read contradicts the shipped data and the
//   upstream text stream, so the offset-5 reader governs.
//   mode 2: five-byte header `FE FE 02 FF FE` (`:104-116`), then a zlib stream
//           laid out as `[u64 compressed][u64 uncompressed]` followed by the
//           compressed UTF-16LE bytes.
//   plain text: the UTF-16LE byte-order mark `FF FE` followed by the UTF-16LE
//           code units, with no container header.
//
// Paths: storage names (including `archive.xp3>member`) resolve through the
// read callback the compat host installs; the local filesystem serves
// absolute paths and the save files the write factories create. A write
// factory creates the missing parent folders of the file it is about to
// create, the way the reference's local file stream does
// (`reference/cpp/core/base/impl/StorageImpl.cpp:618-641`).

#include "tjs2_abi.h"

#include <algorithm>
#include <cerrno>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <limits>
#include <memory>
#include <string>
#include <vector>

#include <zlib.h>

#include <tjs.h>
#include <tjsError.h>

#include <spdlog/spdlog.h>

static std::string g_data_dir;

void tjs2_set_data_dir(const char *dir) {
    g_data_dir = dir ? dir : "";
}

// ---------------------------------------------------------------------------
// Case-insensitive path resolution
//
// Storage names are case-insensitive in the reference engine (and the KR fork
// lowercases them on write), while the host filesystem may be case-sensitive.
// `resolve_path_str` therefore resolves an existing file case-insensitively
// and, for a genuinely new file under the data dir, lowercases the
// storage-relative part so the canonical spelling is created. Only the
// storage-relative part is normalized: the mount prefix keeps its real
// filesystem spelling.
// ---------------------------------------------------------------------------

namespace fs = std::filesystem;

static char ascii_lower_char(char c) {
    return (c >= 'A' && c <= 'Z') ? (char)(c - 'A' + 'a') : c;
}

static std::string ascii_lower_copy(const std::string &s) {
    std::string out = s;
    for(char &c : out)
        c = ascii_lower_char(c);
    return out;
}

// ASCII-only case-insensitive equality; non-ASCII bytes compare exactly so a
// UTF-8 component is never folded or byte-sliced.
static bool ascii_ieq(const std::string &a, const std::string &b) {
    if(a.size() != b.size())
        return false;
    for(size_t i = 0; i < a.size(); ++i)
        if(ascii_lower_char(a[i]) != ascii_lower_char(b[i]))
            return false;
    return true;
}

// True when `path` names `dir` itself or a path inside it. Separator-aware and
// case-sensitive (the caller decides whether to lowercase the remainder).
static bool path_is_under(const std::string &path, std::string dir) {
    while(!dir.empty() && (dir.back() == '/' || dir.back() == '\\'))
        dir.pop_back();
    if(dir.empty() || path.size() < dir.size())
        return false;
    if(path.compare(0, dir.size(), dir) != 0)
        return false;
    if(path.size() == dir.size())
        return true;
    char c = path[dir.size()];
    return c == '/' || c == '\\';
}

// Resolve `full` component by component against the real filesystem, keeping
// the real spelling of existing entries. Components that do not exist yet are
// appended with `lowercase_new` applied (true for storage-relative names under
// the data dir). Uses the non-throwing `std::filesystem` overloads so a
// permission error can never propagate as a C++ exception.
static std::string resolve_ci(const std::string &full, bool lowercase_new) {
    if(full.empty())
        return full;
    bool absolute = full[0] == '/' || full[0] == '\\';
    std::vector<std::string> parts;
    {
        std::string comp;
        for(size_t i = absolute ? 1 : 0; i < full.size(); ++i) {
            char c = full[i];
            if(c == '/' || c == '\\') {
                if(!comp.empty()) {
                    parts.push_back(comp);
                    comp.clear();
                }
            } else {
                comp.push_back(c);
            }
        }
        if(!comp.empty())
            parts.push_back(comp);
    }
    if(parts.empty())
        return full;

    std::error_code ec;
    fs::path cur = absolute ? fs::path("/") : fs::path();
    for(size_t idx = 0; idx < parts.size(); ++idx) {
        std::string match;
        if(fs::is_directory(cur, ec)) {
            fs::directory_iterator it(cur, ec);
            const fs::directory_iterator end;
            while(!ec && it != end) {
                std::string name = it->path().filename().string();
                if(ascii_ieq(name, parts[idx])) {
                    match = name;
                    break;
                }
                it.increment(ec);
            }
        }
        if(!match.empty()) {
            cur /= match;
            continue;
        }
        // First missing component: append the rest, lowercasing them when
        // this is a new storage-relative name under the data dir.
        for(size_t j = idx; j < parts.size(); ++j) {
            std::string name = parts[j];
            if(lowercase_new)
                name = ascii_lower_copy(name);
            cur /= name;
        }
        return cur.string();
    }
    return cur.string();
}

// Resolve a storage-ish name to a filesystem path.
//
// The local `file` media prefix (`file://.`, `file://`, `file:`) is L3 syntax
// the host path resolver does not understand; the reference's
// `TVPGetLocallyAccessibleName` maps it to a local path. The prefix list
// mirrors the compat boundary (`crates/compat/src/storage.rs`).
static std::string strip_file_media(const std::string &s) {
    static const char *const prefixes[] = {"file://.", "file://", "file:"};
    for(const char *prefix : prefixes) {
        size_t n = std::strlen(prefix);
        if(s.size() >= n && s.compare(0, n, prefix) == 0)
            return s.substr(n);
    }
    return s;
}

static std::string resolve_path_str(const std::string &raw) {
    std::string s = strip_file_media(raw);
    if(s.empty())
        return s;
    bool absolute = s[0] == '/' || s[0] == '\\';
    bool drive = s.find(":/") != std::string::npos || s.find(":") == 1;
    std::string full;
    if(absolute || drive) {
        full = s; // absolute / drive-letter
    } else if(!g_data_dir.empty()) {
        full = g_data_dir + "/" + s;
    } else {
        return s;
    }

    std::error_code ec;
    bool under_data = !g_data_dir.empty() && path_is_under(full, g_data_dir);
    if(under_data) {
        // Reference semantics: storage names are normalized to lowercase
        // under the data dir. Prefer the lowercase storage-relative spelling
        // so a write updates the canonical file rather than an exact-case
        // duplicate (and so a read finds it regardless of the spelling).
        std::string relative = full.substr(g_data_dir.size());
        size_t start = relative.find_first_not_of("/\\");
        relative = (start == std::string::npos) ? std::string() : relative.substr(start);
        std::string lowered = g_data_dir + "/" + ascii_lower_copy(relative);
        std::string resolved = resolve_ci(lowered, true);
        if(fs::exists(resolved, ec))
            return resolved;
    }
    if(fs::exists(full, ec))
        return full;
    return resolve_ci(full, under_data);
}

static std::string resolve_path(const ttstr &name) {
    return resolve_path_str(name.AsNarrowStdString());
}

// ---------------------------------------------------------------------------
// Mode-string parsing
// ---------------------------------------------------------------------------

// The outcome of reading a mode number. The three cases are distinct
// because the caller reacts to them differently: a key that is absent selects
// the caller's default, while a key that is present without a number selects
// another output form than a key with a number.
enum class ModeNumber {
    // The key is not in the mode string.
    Absent,
    // The key is in the mode string but is not followed by a digit.
    NoNumber,
    // The key is followed by a digit; the value is in `out`.
    Value,
};

// Finds `key` and, when it is directly followed by 1..max_digits decimal
// digits, parses them into `out`.
static ModeNumber parse_mode_number(const std::string &mode, char key,
                                     int max_digits, long long &out) {
    size_t pos = mode.find(key);
    if(pos == std::string::npos)
        return ModeNumber::Absent;
    if(pos + 1 >= mode.size() || mode[pos + 1] < '0' || mode[pos + 1] > '9')
        return ModeNumber::NoNumber;
    long long value = 0;
    size_t i = pos + 1;
    for(int n = 0; n < max_digits && i < mode.size() && mode[i] >= '0' &&
                    mode[i] <= '9';
        ++n, ++i)
        value = value * 10 + (mode[i] - '0');
    out = value;
    return ModeNumber::Value;
}

// The byte offset the mode's `oN` selects. An `o` without a digit carries
// no offset, so the file is written from its start.
static long long mode_offset(const std::string &mode) {
    long long ofs = 0;
    if(parse_mode_number(mode, 'o', 255, ofs) != ModeNumber::Value)
        return 0;
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

// The reference access flag the binary-write factory selects for `mode`:
// `oN` (N > 0) is `TJS_BS_UPDATE` (the reference `TVPCreateBinaryStreamForWrite`
// opens at an offset), an `a` is the port's `TJS_BS_APPEND`, everything else is
// `TJS_BS_WRITE` (create/truncate).
static FileAccess binary_write_access(const std::string &mode) {
    if(mode.find('a') != std::string::npos)
        return FileAccess::Append;
    return mode_offset(mode) != 0 ? FileAccess::Update : FileAccess::Write;
}

// The text-write factory follows the reference `tTVPTextWriteStream` exactly:
// `oN` (N > 0) is `TJS_BS_UPDATE`, everything else is `TJS_BS_WRITE`.
static FileAccess text_write_access(const std::string &mode) {
    return mode_offset(mode) != 0 ? FileAccess::Update : FileAccess::Write;
}

class FileBinaryStream : public TJS::tTJSBinaryStream {
    FILE *f;
    bool readable;
    bool writable;
    std::string path;
    std::string open_error;
    bool write_failure_logged = false;

public:
    FileBinaryStream(const std::string &p, FileAccess access)
        : f(open_file(p, access)),
          readable(access == FileAccess::Read || access == FileAccess::Update),
          writable(access != FileAccess::Read),
          path(p) {
        if(!f)
            open_error = std::strerror(errno);
    }

    ~FileBinaryStream() override {
        if(f)
            std::fclose(f);
    }

    bool ok() const { return f != nullptr; }
    const std::string &error() const { return open_error; }

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
        if(!f || !writable) {
            // One line per stream, not per byte: a refused write must be
            // visible in a device log but must not spam it.
            if(!write_failure_logged) {
                write_failure_logged = true;
                std::string reason =
                    f ? std::string("stream is read-only")
                      : ("open failed: " + open_error);
                spdlog::warn("stream write refused '{}' ({} byte(s)): {}",
                             path, write_size, reason);
            }
            return 0;
        }
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
// Binary stream (in-memory, read-only)
// ---------------------------------------------------------------------------

// Wraps a caller-owned byte range so the VM's stream-based
// `tTJS::LoadByteCode` can run on bytes that are already in memory. The
// range is borrowed, not copied: the stream must not outlive it.
class MemoryBinaryStream : public TJS::tTJSBinaryStream {
    const tjs_uint8 *data;
    tjs_uint64 len;
    tjs_int64 pos;

public:
    MemoryBinaryStream(const void *buffer, size_t length)
        : data(static_cast<const tjs_uint8 *>(buffer)), len((tjs_uint64)length),
          pos(0) {}

    // The base class implements GetPosition() as Seek(0, SEEK_CUR), so Seek
    // returns the new position itself instead of calling GetPosition().
    tjs_uint64 Seek(tjs_int64 offset, tjs_int whence) override {
        tjs_int64 base;
        switch(whence) {
            case SEEK_SET:
                base = 0;
                break;
            case SEEK_CUR:
                base = pos;
                break;
            case SEEK_END:
                base = (tjs_int64)len;
                break;
            default:
                return (tjs_uint64)pos;
        }
        tjs_int64 next = base + offset;
        if(next < 0)
            next = 0;
        if(next > (tjs_int64)len)
            next = (tjs_int64)len;
        pos = next;
        return (tjs_uint64)pos;
    }

    tjs_uint Read(void *buffer, tjs_uint read_size) override {
        if(!buffer || pos >= (tjs_int64)len)
            return 0;
        tjs_uint64 available = len - (tjs_uint64)pos;
        tjs_uint count =
            read_size < available ? read_size : (tjs_uint)available;
        std::memcpy(buffer, data + pos, count);
        pos += count;
        return count;
    }

    tjs_uint Write(const void * /*buffer*/, tjs_uint /*write_size*/) override {
        return 0;
    }

    tjs_uint64 GetSize() override { return len; }
};

TJS::tTJSBinaryStream *tjs2_create_memory_stream(const void *data, size_t len) {
    return new MemoryBinaryStream(data, len);
}

// A read-only binary stream owning the bytes it serves: a storage entry the
// compat read callback returned. The reference hands out a seekable
// archive-member stream; reading the whole entry into memory is the port's
// equivalent.
class OwnedMemoryBinaryStream : public TJS::tTJSBinaryStream {
    std::vector<uint8_t> data;
    tjs_int64 pos;

public:
    explicit OwnedMemoryBinaryStream(std::vector<uint8_t> bytes)
        : data(std::move(bytes)), pos(0) {}

    tjs_uint64 Seek(tjs_int64 offset, tjs_int whence) override {
        tjs_int64 base;
        switch(whence) {
            case SEEK_SET:
                base = 0;
                break;
            case SEEK_CUR:
                base = pos;
                break;
            case SEEK_END:
                base = (tjs_int64)data.size();
                break;
            default:
                return (tjs_uint64)pos;
        }
        tjs_int64 next = base + offset;
        if(next < 0)
            next = 0;
        if(next > (tjs_int64)data.size())
            next = (tjs_int64)data.size();
        pos = next;
        return (tjs_uint64)pos;
    }

    tjs_uint Read(void *buffer, tjs_uint read_size) override {
        if(!buffer || pos >= (tjs_int64)data.size())
            return 0;
        size_t available = data.size() - (size_t)pos;
        tjs_uint count = (tjs_uint)std::min<size_t>(read_size, available);
        std::memcpy(buffer, data.data() + pos, count);
        pos += count;
        return count;
    }

    tjs_uint Write(const void * /*buffer*/, tjs_uint /*write_size*/) override {
        return 0;
    }

    tjs_uint64 GetSize() override { return (tjs_uint64)data.size(); }
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
    stream.WriteBuffer(buf, 8);
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

// The storage name as UTF-8 for the diagnostic lines (the same conversion the
// rest of the port uses; `AsNarrowStdString` would mangle non-ASCII).
static std::string ttstr_utf8(const ttstr &s) {
    return utf16_to_utf8(s.c_str(), (size_t)s.GetLen());
}

// A UTF-8 message as a TJS string (the error messages raised below).
static TJS::ttstr ttstr_from_utf8(const std::string &utf8) {
    std::vector<char16_t> units = utf8_to_utf16(utf8.data(), utf8.size());
    return TJS::ttstr(units.data(), units.size());
}

// The reference's `TJS_BS_*` name for the access a factory chose.
static const char *reference_access_name(FileAccess access) {
    switch(access) {
        case FileAccess::Read:
            return "TJS_BS_READ";
        case FileAccess::Write:
            return "TJS_BS_WRITE";
        case FileAccess::Append:
            return "TJS_BS_APPEND";
        case FileAccess::Update:
            return "TJS_BS_UPDATE";
    }
    return "TJS_BS_UNKNOWN";
}

// One `debug` line per stream the factories hand out (a `warn` when the open
// failed), so a device log shows every attempted storage open and where it
// resolved. Volume is one line per stream, never per byte. `source` is
// `storage` when the compat read callback served the entry and `filesystem`
// otherwise; `resolved` follows the reference `_TVPCreateStream` resolve step
// -- `TJS_BS_WRITE` normalizes the name without requiring the file, every
// other access takes the placed path and requires it
// (`reference/cpp/core/base/StorageIntf.cpp:1131-1148`) -- while `existed` is
// the source's state before the open. A write to a new name therefore shows
// `source=filesystem resolved=true existed=false`.
static void log_stream_open(const char *kind, const ttstr &name,
                            const std::string &mode, FileAccess access,
                            const std::string &source,
                            const std::string &host, bool existed,
                            bool opened, const std::string &reason) {
    std::string label = reference_access_name(access);
    if(mode.find('d') != std::string::npos)
        label += "|TJS_BS_DELETE_ON_CLOSE";
    bool resolved = access == FileAccess::Write || existed;
    if(opened) {
        spdlog::debug(
            "stream open {} '{}' mode='{}' access={} source={} resolved={} "
            "existed={} host='{}'",
            kind, ttstr_utf8(name), mode, label, source, resolved, existed,
            host);
    } else {
        spdlog::warn(
            "stream open {} '{}' mode='{}' access={} source={} resolved={} "
            "existed={} host='{}' failed: {}",
            kind, ttstr_utf8(name), mode, label, source, resolved, existed,
            host, reason);
    }
}

// ---------------------------------------------------------------------------
// Storage seam
//
// The reference wires the four factories to the storage layer
// (`reference/cpp/core/base/ScriptMgnIntf.cpp:464-469`), so
// `Array.load("archive.xp3>member")` reads the archive member. The storage
// lives in the compatibility layer, not in this crate, so the host installs
// one read callback here; a name the storage does not serve falls back to the
// local filesystem (absolute paths and the save files the write factories
// create). One process-wide registration.
// ---------------------------------------------------------------------------

static tjs2_storage_read_fn g_storage_read = nullptr;
static void *g_storage_read_user = nullptr;

void tjs2_set_storage_read_callback(tjs2_storage_read_fn read, void *user) {
    g_storage_read = read;
    g_storage_read_user = user;
}

void tjs2_free_storage_bytes(void *data) {
    std::free(data);
}

// The reference `TVPCannotOpenStorage`
// (`reference/cpp/core/base/MsgIntfInc.h:22`).
[[noreturn]] static void throw_cannot_open(const ttstr &name) {
    throw TJS::eTJSError(
        ttstr_from_utf8("Cannot open storage " + ttstr_utf8(name)));
}

// True when `name` carries a storage-media name other than the local `file`
// media (`psb://...`, `var:...`). The media name is the leading ASCII-letter
// run before the first colon; a single-letter prefix is a Windows drive letter
// and stays a local path.
static bool has_foreign_media_prefix(const ttstr &name) {
    const tjs_char *start = name.c_str();
    const tjs_char *p = start;
    while((*p >= TJS_W('a') && *p <= TJS_W('z')) ||
          (*p >= TJS_W('A') && *p <= TJS_W('Z')))
        p++;
    if(p == start || *p != TJS_W(':'))
        return false;
    const size_t len = (size_t)(p - start);
    if(len == 1)
        return false;
    return !(len == 4 && start[0] == TJS_W('f') && start[1] == TJS_W('i') &&
             start[2] == TJS_W('l') && start[3] == TJS_W('e'));
}

// The reference `TVPCannotWriteToArchive`
// (`reference/cpp/core/base/MsgIntfInc.h:43`).
[[noreturn]] static void throw_cannot_write_to_archive() {
    throw TJS::eTJSError(ttstr_from_utf8("Cannot write to archive"));
}

// The reference `TVPCompressionFailed`
// (`reference/cpp/core/base/MsgIntfInc.h:42`).
[[noreturn]] static void throw_compression_failed() {
    throw TJS::eTJSError(ttstr_from_utf8("Compression failed"));
}

// The reference `TJSSeekError` (`reference/cpp/core/tjs2/tjsError.h:331`),
// raised when a mode's `oN` offset cannot be reached.
[[noreturn]] static void throw_seek_error() {
    throw TJS::eTJSError(ttstr(TJSSeekError));
}

// The reference `TVPUnsupportedCipherMode`
// (`reference/cpp/core/base/MsgIntfInc.h:44`) for a crypt container the
// reader cannot decode.
[[noreturn]] static void throw_unsupported_cipher(const ttstr &name) {
    throw TJS::eTJSError(
        ttstr_from_utf8(ttstr_utf8(name) + " is unsupported cipher mode"));
}

// The mode-string error: the cipher mode a write mode selects is outside the
// supported set {-1, 1, 2}. The message is the mode string complaint
// ("<argument> is unsupported mode string") with "unsupported cipher mode"
// as its argument.
[[noreturn]] static void throw_unsupported_mode_string() {
    throw TJS::eTJSError(
        ttstr_from_utf8("unsupported cipher mode is unsupported mode string"));
}

// Read `name` through the installed storage callback.
//
// Returns true and fills `out` when the storage serves the entry. Returns
// false when the storage does not have it, so the caller falls back to the
// local filesystem; any other storage failure raises the callback's message.
// With no callback installed it always returns false.
static bool read_storage_entry(const ttstr &name, std::vector<uint8_t> &out) {
    if(!g_storage_read)
        return false;
    std::string utf8 = ttstr_utf8(name);
    unsigned char *data = nullptr;
    size_t len = 0;
    char *message = nullptr;
    int result = g_storage_read(g_storage_read_user, utf8.data(), utf8.size(),
                                &data, &len, &message);
    if(result == TJS2_STORAGE_READ_OK) {
        if(len > 0) {
            if(!data) {
                throw TJS::eTJSError(ttstr_from_utf8(
                    "storage read returned no bytes for " + utf8));
            }
            out.assign(data, data + len);
        }
        tjs2_free_storage_bytes(data);
        return true;
    }
    if(result == TJS2_STORAGE_READ_NOT_FOUND) {
        if(message)
            tjs2_free_string(message);
        return false;
    }
    std::string reason =
        message ? std::string(message) : std::string("storage read failed");
    if(message)
        tjs2_free_string(message);
    throw TJS::eTJSError(ttstr_from_utf8(reason));
}

// Read the whole entry for a read factory: the storage first, then the local
// filesystem. `source`, `host` and `existed` describe where the bytes came
// from for the open log. A name that still names an archive member never falls
// back to a literal file, and a name neither source serves raises
// `TVPCannotOpenStorage`.
static std::vector<uint8_t> read_entry(const char *kind, const ttstr &name,
                                       const std::string &mode,
                                       std::string &source,
                                       std::string &host, bool &existed) {
    std::vector<uint8_t> raw;
    source = "storage";
    host = ttstr_utf8(name);
    existed = true;
    if(read_storage_entry(name, raw)) {
        log_stream_open(kind, name, mode, FileAccess::Read, source, host,
                        existed, true, "");
        return raw;
    }

    // A name that still carries the archive delimiter names an archive member;
    // the local filesystem cannot serve it, so a storage miss raises here
    // instead of probing a literal file name.
    if(TJS_strchr(name.c_str(), TJS_W('>')) != nullptr) {
        log_stream_open(kind, name, mode, FileAccess::Read, "storage", host,
                        false, false, "archive member not found");
        throw_cannot_open(name);
    }

    source = "filesystem";
    host = resolve_path(name);
    std::error_code ec;
    existed = fs::exists(host, ec);
    FileBinaryStream stream(host, FileAccess::Read);
    if(!stream.ok()) {
        log_stream_open(kind, name, mode, FileAccess::Read, source, host,
                        existed, false, stream.error());
        throw_cannot_open(name);
    }
    uint64_t size = stream.GetSize();
    if(size > static_cast<uint64_t>(std::numeric_limits<size_t>::max())) {
        log_stream_open(kind, name, mode, FileAccess::Read, source, host,
                        existed, false,
                        "entry is larger than the address space");
        throw_cannot_open(name);
    }
    raw.resize(static_cast<size_t>(size));
    size_t done = 0;
    while(done < raw.size()) {
        const size_t chunk = std::min<size_t>(raw.size() - done, 1u << 30);
        stream.ReadBuffer(raw.data() + done, (tjs_uint)chunk);
        done += chunk;
    }
    log_stream_open(kind, name, mode, FileAccess::Read, source, host, existed,
                    true, "");
    return raw;
}

// ---------------------------------------------------------------------------
// Text read stream
// ---------------------------------------------------------------------------

class TextReadStream : public TJS::iTJSTextReadStream {
    std::vector<char16_t> text; // decoded contents
    size_t pos;

    // Decode the whole entry. A container the reference rejects raises
    // `TVPUnsupportedCipherMode`; there is no empty-stream fallback.
    void decode(const std::vector<uint8_t> &raw, const ttstr &name) {
        if(raw.size() >= 3 && raw[0] == 0xFE && raw[1] == 0xFE) {
            uint8_t m = raw[2];
            if(m != 0 && m != 1 && m != 2)
                throw_unsupported_cipher(name);
            // The reference reads the two bytes after the mode as the
            // mandatory UTF-16LE byte-order mark and starts the payload at
            // offset 5 (`FE FE <mode> FF FE`).
            if(raw.size() < 5 || raw[3] != 0xFF || raw[4] != 0xFE)
                throw_unsupported_cipher(name);
            if(m == 0 || m == 1) {
                // Cipher modes 0/1: decrypt the UTF-16LE code units beginning
                // at offset 5 (reference
                // `reference/cpp/core/base/TextStream.cpp:82-105`).
                const size_t start = 5;
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
            // m == 2
            // `FE FE 02 FF FE [u64 compressed][u64 uncompressed][data]`
            if(raw.size() < 5 + 16)
                throw_unsupported_cipher(name);
            {
                uint64_t compressed = read_u64le(raw.data() + 5);
                uint64_t uncompressed = read_u64le(raw.data() + 13);
                size_t payload = 5 + 16;
                if(compressed > raw.size() - payload)
                    throw_unsupported_cipher(name);
                if(uncompressed == 0)
                    return;
                if(uncompressed >
                   static_cast<uint64_t>(std::numeric_limits<size_t>::max()))
                    throw_unsupported_cipher(name);
                std::vector<uint8_t> out((size_t)uncompressed);
                uLongf dest_len = (uLongf)uncompressed;
                int ret = uncompress(out.data(), &dest_len, raw.data() + payload,
                                     (uLong)compressed);
                if(ret != Z_OK || dest_len != uncompressed)
                    throw_unsupported_cipher(name);
                // The reference assigns `uncompressed / 2` code units, so an
                // odd byte count is truncated rather than rejected
                // (reference/cpp/core/base/TextStream.cpp:129-131).
                size_t units = (size_t)uncompressed / sizeof(char16_t);
                text.resize(units);
                if(units > 0)
                    std::memcpy(text.data(), out.data(),
                                units * sizeof(char16_t));
                return;
            }
        }

        // Plain text: a byte-order mark selects UTF-16LE/BE or UTF-8. A
        // BOM-less payload decodes as UTF-8, the default read encoding; the
        // script decoder applies charset detection for legacy encodings.
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
            text = utf8_to_utf16((const char *)raw.data(), raw.size());
        }
    }

public:
    // `raw` is the whole entry (a storage member or a local file); `offset` is
    // the mode's `oN` byte offset. The reference seeks before reading: an
    // offset past the end is the TJS seek error, an offset at the end reads
    // zero bytes, and a container the reference rejects raises.
    TextReadStream(std::vector<uint8_t> raw, uint64_t offset, const ttstr &name)
        : pos(0) {
        if(offset > raw.size())
            throw_seek_error();
        if(offset == raw.size())
            return; // empty stream at EOF
        raw.erase(raw.begin(), raw.begin() + (std::ptrdiff_t)offset);
        decode(raw, name);
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

// The output form a write mode selects.
//
// The cipher mode comes from the `c` key: its digit when the key carries
// one, 1 when the key is present without a number, and -1 (no cipher) when
// the key is absent. A `z` key that is present without a number selects the
// compressed container (cipher mode 2). Any other cipher mode is an error.
//
// The compression level is the `z` number when the key carries one, the zlib
// default when the key is absent, and 0 when the key is present without a
// number.
struct TextWriteFormat {
    int crypt_mode;
    int compression_level;
};

// Decides the output form of a text write stream from its mode string.
static TextWriteFormat text_write_format(const std::string &mode) {
    long long value = 0;
    int crypt_mode = -1;
    switch(parse_mode_number(mode, 'c', 1, value)) {
        case ModeNumber::Absent:
            crypt_mode = -1; // no cipher
            break;
        case ModeNumber::NoNumber:
            crypt_mode = 1; // the simple cipher
            break;
        case ModeNumber::Value:
            crypt_mode = (int)value;
            break;
    }
    int compression_level = 0;
    switch(parse_mode_number(mode, 'z', 1, value)) {
        case ModeNumber::Absent:
            compression_level = Z_DEFAULT_COMPRESSION;
            break;
        case ModeNumber::NoNumber:
            crypt_mode = 2; // the compressed container
            break;
        case ModeNumber::Value:
            compression_level = (int)value;
            break;
    }
    if(crypt_mode != -1 && crypt_mode != 1 && crypt_mode != 2)
        throw_unsupported_mode_string();
    return {crypt_mode, compression_level};
}

class TextWriteStream : public TJS::iTJSTextWriteStream {
    static constexpr size_t COMPRESSION_BUFFER_SIZE = 1024 * 1024;

    std::unique_ptr<FileBinaryStream> stream;
    int crypt_mode;
    int compression_level;
    z_stream zs;
    bool zs_init;
    bool compression_failed;
    bool finalized;
    int64_t compression_size_position;
    std::vector<Bytef> compression_buffer;

    void write_raw(const void *ptr, size_t size) {
        if(crypt_mode != 2) {
            stream->WriteBuffer(ptr, (tjs_uint)size);
            return;
        }
        if(!zs_init) {
            compression_failed = true;
            throw_compression_failed();
        }
        if(compression_failed)
            return;
        zs.next_in = (Bytef *)ptr;
        zs.avail_in = (uInt)size;
        while(zs.avail_in > 0) {
            int ret = deflate(&zs, Z_NO_FLUSH);
            if(ret != Z_OK) {
                compression_failed = true;
                throw_compression_failed();
            }
            if(zs.avail_out == 0) {
                stream->WriteBuffer(compression_buffer.data(),
                                    COMPRESSION_BUFFER_SIZE);
                zs.next_out = compression_buffer.data();
                zs.avail_out = COMPRESSION_BUFFER_SIZE;
            }
        }
    }

    // Closes the zlib stream and patches the two size placeholders. Called
    // once, from Destruct(), before the object is deleted.
    void finalize() {
        if(finalized)
            return;
        finalized = true;
        if(!zs_init)
            return;
        bool finish_failed = false;
        try {
            if(!compression_failed) {
                int result = 0;
                do {
                    result = deflate(&zs, Z_FINISH);
                    if(result != Z_OK && result != Z_STREAM_END) {
                        compression_failed = true;
                        finish_failed = true;
                        break;
                    }
                    stream->WriteBuffer(compression_buffer.data(),
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
        } catch(...) {
            deflateEnd(&zs);
            zs_init = false;
            throw;
        }
        deflateEnd(&zs);
        zs_init = false;
        if(finish_failed)
            throw_compression_failed();
    }

public:
    TextWriteStream(const std::string &path, const std::string &mode)
        : crypt_mode(-1), // no cipher until the mode decides
          compression_level(Z_DEFAULT_COMPRESSION),
          zs(),
          zs_init(false),
          compression_failed(false),
          finalized(false),
          compression_size_position(0),
          compression_buffer(COMPRESSION_BUFFER_SIZE) {
        TextWriteFormat format = text_write_format(mode);
        crypt_mode = format.crypt_mode;
        compression_level = format.compression_level;

        long long ofs = mode_offset(mode);
        FileAccess access = text_write_access(mode);
        stream = std::make_unique<FileBinaryStream>(path, access);
        if(!stream->ok())
            return;
        if(ofs != 0)
            stream->SetPosition((tjs_uint64)ofs);

        if(crypt_mode == 2) {
            // `FE FE 02 FF FE` then the two little-endian u64 sizes
            // (reference `reference/cpp/core/base/TextStream.cpp:104-116`).
            uint8_t sig[5] = {0xFE, 0xFE, 0x02, 0xFF, 0xFE};
            stream->WriteBuffer(sig, 5);

            zs.zalloc = Z_NULL;
            zs.zfree = Z_NULL;
            zs.opaque = Z_NULL;
            if(deflateInit(&zs, compression_level) != Z_OK) {
                compression_failed = true;
                throw_compression_failed();
            }
            zs_init = true;
            zs.next_in = nullptr;
            zs.avail_in = 0;
            zs.next_out = compression_buffer.data();
            zs.avail_out = COMPRESSION_BUFFER_SIZE;

            // Compression size placeholders, patched by finalize().
            compression_size_position = (int64_t)stream->GetPosition();
            write_u64le(*stream, 0);
            write_u64le(*stream, 0);
        } else if(crypt_mode == 1) {
            // The simple cipher: the three-byte signature `FE FE 01` then the
            // UTF-16LE byte-order mark, so the reader's mandatory-BOM header
            // matches.
            uint8_t sig[5] = {0xFE, 0xFE, 0x01, 0xFF, 0xFE};
            stream->WriteBuffer(sig, 5);
        } else {
            // Plain text: the UTF-16LE byte-order mark and no container
            // header.
            uint8_t bom[2] = {0xFF, 0xFE};
            stream->WriteBuffer(bom, 2);
        }
    }

    ~TextWriteStream() override {
        if(zs_init) {
            deflateEnd(&zs);
            zs_init = false;
        }
    }

    void Write(const TJS::tTJSString &targ) override {
        tjs_int len = targ.GetLen();
        if(len <= 0)
            return;
        std::vector<char16_t> buf((size_t)len);
        const tjs_char *src = targ.c_str();
        for(tjs_int i = 0; i < len; i++) {
            char16_t ch = (char16_t)src[i];
            if(crypt_mode == 1)
                ch = (char16_t)(((ch >> 1) & 0x5555) | ((ch << 1) & 0xaaaa));
            buf[(size_t)i] = ch;
        }
        write_raw(buf.data(), (size_t)len * sizeof(char16_t));
    }

    bool ok() const { return stream && stream->ok(); }
    std::string error() const {
        return stream ? stream->error() : std::string();
    }

    void Destruct() override {
        try {
            finalize();
        } catch(...) {
            delete this;
            throw;
        }
        delete this;
    }
};

// ---------------------------------------------------------------------------
// Factories
// ---------------------------------------------------------------------------
static TJS::iTJSTextReadStream *create_text_read(const ttstr &name,
                                                 const ttstr &mode) {
    std::string m = mode.AsNarrowStdString();
    std::string source, host;
    bool existed = false;
    std::vector<uint8_t> raw =
        read_entry("text-read", name, m, source, host, existed);
    return new TextReadStream(std::move(raw), (uint64_t)mode_offset(m), name);
}

// Creates the missing parent folders of `path`.
//
// The reference creates the folder tree when it opens a local file for
// write: `tTVPLocalFileStream` checks the parent directory of the target
// and calls `TVPCreateFolders` (mkdir -p) before the file is created, and
// raises `TVPCannotOpenStorage` when that fails
// (`reference/cpp/core/base/impl/StorageImpl.cpp:618-641`). Without it a
// write into a directory that does not exist yet fails with "No such file
// or directory", which is what makes a game's save fail when its data
// directory is missing.
//
// Returns false and fills `error` with the reason when the folders cannot
// be created; the caller then raises the reference's open error.
static bool create_parent_folders(const std::string &path, std::string &error) {
    fs::path parent = fs::path(path).parent_path();
    if(parent.empty())
        return true;
    std::error_code ec;
    if(fs::is_directory(parent, ec))
        return true;
    fs::create_directories(parent, ec);
    if(ec) {
        error = ec.message();
        return false;
    }
    return true;
}

static TJS::iTJSTextWriteStream *create_text_write(const ttstr &name,
                                                   const ttstr &mode) {
    // `_TVPCreateStream` normalizes a write name and refuses it when the
    // result still names an archive member
    // (`reference/cpp/core/base/StorageIntf.cpp:1150-1153`).
    if(TJS_strchr(name.c_str(), TJS_W('>')) != nullptr)
        throw_cannot_write_to_archive();
    // A media-prefixed name is routed to that storage media's writer; this
    // layer has no write-capable media provider, so it refuses rather than
    // creating a local file named after the media.
    if(has_foreign_media_prefix(name))
        throw_cannot_open(name);
    std::string m = mode.AsNarrowStdString();
    std::string host = resolve_path(name);
    bool existed = std::filesystem::exists(host);
    FileAccess access = text_write_access(m);
    // The reference creates the parent folders of a file it is about to
    // create (`tTVPLocalFileStream`,
    // `reference/cpp/core/base/impl/StorageImpl.cpp:618-641`).
    std::string folder_error;
    if(access == FileAccess::Write && !existed &&
       !create_parent_folders(host, folder_error)) {
        log_stream_open("text-write", name, m, access, "filesystem", host,
                        existed, false, folder_error);
        throw_cannot_open(name);
    }
    auto *s = new TextWriteStream(host, m);
    log_stream_open("text-write", name, m, access, "filesystem",
                    host, existed, s->ok(), s->error());
    if(!s->ok()) {
        delete s;
        throw_cannot_open(name);
    }
    return s;
}

static TJS::tTJSBinaryStream *create_bin_read(const ttstr &name,
                                              const ttstr &mode) {
    std::string m = mode.AsNarrowStdString();
    std::string source, host;
    bool existed = false;
    std::vector<uint8_t> raw =
        read_entry("binary-read", name, m, source, host, existed);
    std::unique_ptr<OwnedMemoryBinaryStream> s(
        new OwnedMemoryBinaryStream(std::move(raw)));
    long long ofs = mode_offset(m);
    if(ofs != 0)
        s->SetPosition((tjs_uint64)ofs);
    return s.release();
}

static TJS::tTJSBinaryStream *create_bin_write(const ttstr &name,
                                               const ttstr &mode) {
    // `_TVPCreateStream` normalizes a write name and refuses it when the
    // result still names an archive member
    // (`reference/cpp/core/base/StorageIntf.cpp:1150-1153`).
    if(TJS_strchr(name.c_str(), TJS_W('>')) != nullptr)
        throw_cannot_write_to_archive();
    // A media-prefixed name is routed to that storage media's writer; this
    // layer has no write-capable media provider, so it refuses rather than
    // creating a local file named after the media.
    if(has_foreign_media_prefix(name))
        throw_cannot_open(name);
    std::string m = mode.AsNarrowStdString();
    std::string host = resolve_path(name);
    bool existed = std::filesystem::exists(host);
    FileAccess access = binary_write_access(m);
    // The reference creates the parent folders of a file it is about to
    // create (`tTVPLocalFileStream`,
    // `reference/cpp/core/base/impl/StorageImpl.cpp:618-641`).
    std::string folder_error;
    if(access == FileAccess::Write && !existed &&
       !create_parent_folders(host, folder_error)) {
        log_stream_open("binary-write", name, m, access, "filesystem", host,
                        existed, false, folder_error);
        throw_cannot_open(name);
    }
    std::unique_ptr<FileBinaryStream> s(new FileBinaryStream(host, access));
    log_stream_open("binary-write", name, m, access, "filesystem", host,
                    existed, s->ok(), s->error());
    if(!s->ok())
        throw_cannot_open(name);
    long long ofs = mode_offset(m);
    if(ofs != 0)
        s->SetPosition((tjs_uint64)ofs);
    return s.release();
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
// Diagnostic/test helper (declared in crates/tjs/src/lib.rs)
// ---------------------------------------------------------------------------

// A malloc'd NUL-terminated copy of `text` (free with `tjs2_free_string`).
static char *alloc_c_string(const std::string &text) {
    char *buf = (char *)std::malloc(text.size() + 1);
    if(!buf)
        return nullptr;
    std::memcpy(buf, text.data(), text.size());
    buf[text.size()] = 0;
    return buf;
}

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
        std::string mode = mode_utf8 ? mode_utf8 : "";
        TJS::ttstr name = ttstr_from_utf8(path_utf8);
        std::string source, host;
        bool existed = false;
        std::vector<uint8_t> raw =
            read_entry("text-read", name, mode, source, host, existed);
        TextReadStream stream(std::move(raw), (uint64_t)mode_offset(mode),
                              name);
        TJS::tTJSString content;
        stream.Read(content, 0);
        std::string utf8 =
            utf16_to_utf8(content.c_str(), (size_t)content.GetLen());
        char *buf = alloc_c_string(utf8);
        if(!buf)
            return 1;
        *out_utf8 = buf;
        return 0;
    } catch(...) {
        return 1;
    }
}

// ---------------------------------------------------------------------------
// Factory test helpers (declared by the Rust tests)
// ---------------------------------------------------------------------------

// The `TJS_BS_*` access name the binary-write factory chooses for `mode`
// ("TJS_BS_WRITE", "TJS_BS_APPEND" or "TJS_BS_UPDATE"). Static string; do not
// free it.
extern "C" const char *tjs2_test_binary_write_access(const char *mode_utf8) {
    std::string mode = mode_utf8 ? mode_utf8 : "";
    return reference_access_name(binary_write_access(mode));
}

// Open through the wired binary factory and close immediately. `for_write`
// selects the write factory (non-zero) or the read factory (zero).
// `*out_opened` is 1 when the stream opened; `*out_access` receives the
// reference access name (static). Returns 0, or non-zero with a malloc'd
// message in `*out_error`. The read factory raises when neither the storage
// nor the filesystem serves the name; the helper reports that as
// `*out_opened = 0` so the mode test can observe the failure.
extern "C" int tjs2_test_open_binary_stream(const char *name_utf8,
                                            const char *mode_utf8,
                                            int for_write, int *out_opened,
                                            const char **out_access,
                                            char **out_error) {
    if(out_opened)
        *out_opened = 0;
    if(out_access)
        *out_access = "";
    if(out_error)
        *out_error = nullptr;
    if(!name_utf8)
        return 1;
    try {
        std::string mode = mode_utf8 ? mode_utf8 : "";
        TJS::ttstr name = ttstr_from_utf8(name_utf8);
        TJS::ttstr tmode = ttstr_from_utf8(mode);
        TJS::tTJSBinaryStream *stream = for_write ? create_bin_write(name, tmode)
                                                  : create_bin_read(name, tmode);
        if(out_access)
            *out_access = reference_access_name(
                for_write ? binary_write_access(mode) : FileAccess::Read);
        if(out_opened) {
            if(for_write) {
                auto *binary = dynamic_cast<FileBinaryStream *>(stream);
                *out_opened = (binary && binary->ok()) ? 1 : 0;
            } else {
                *out_opened = 1;
            }
        }
        delete stream;
        return 0;
    } catch(const TJS::eTJS &err) {
        if(out_error)
            *out_error = alloc_c_string(err.getMessage().AsStdString());
        return 0;
    } catch(const std::exception &err) {
        if(out_error)
            *out_error = alloc_c_string(std::string("exception: ") + err.what());
        return 1;
    } catch(...) {
        return 1;
    }
}

// Read a whole binary stream through the wired read factory (honoring the
// mode's `oN` offset) into a malloc'd buffer (free with
// tjs2_free_storage_bytes). Returns 0 on success; on failure returns non-zero
// and, if `out_error` is non-NULL, stores a malloc'd UTF-8 message.
extern "C" int tjs2_test_read_binary_stream_all(const char *name_utf8,
                                                const char *mode_utf8,
                                                unsigned char **out_data,
                                                size_t *out_len,
                                                char **out_error) {
    if(out_data)
        *out_data = nullptr;
    if(out_len)
        *out_len = 0;
    if(out_error)
        *out_error = nullptr;
    if(!name_utf8 || !out_data || !out_len)
        return 1;
    try {
        TJS::ttstr name = ttstr_from_utf8(name_utf8);
        TJS::ttstr tmode = ttstr_from_utf8(mode_utf8 ? mode_utf8 : "");
        std::unique_ptr<TJS::tTJSBinaryStream> stream(create_bin_read(name, tmode));
        tjs_uint64 size = stream->GetSize();
        tjs_uint64 position = stream->GetPosition();
        if(position > size)
            position = size;
        size -= position;
        if(size > static_cast<tjs_uint64>(std::numeric_limits<size_t>::max())) {
            if(out_error)
                *out_error = alloc_c_string("stream is larger than the address space");
            return 1;
        }
        std::vector<uint8_t> bytes((size_t)size);
        size_t done = 0;
        while(done < bytes.size()) {
            const size_t chunk = std::min<size_t>(bytes.size() - done, 1u << 30);
            stream->ReadBuffer(bytes.data() + done, (tjs_uint)chunk);
            done += chunk;
        }
        unsigned char *buf = (unsigned char *)std::malloc(bytes.size() + 1);
        if(!buf)
            return 1;
        if(!bytes.empty())
            std::memcpy(buf, bytes.data(), bytes.size());
        *out_data = buf;
        *out_len = bytes.size();
        return 0;
    } catch(const TJS::eTJS &err) {
        if(out_error)
            *out_error = alloc_c_string(err.getMessage().AsStdString());
        return 1;
    } catch(const std::exception &err) {
        if(out_error)
            *out_error = alloc_c_string(std::string("exception: ") + err.what());
        return 1;
    } catch(...) {
        return 1;
    }
}

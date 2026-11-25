// The value layer's primitives, timed against the Rust column that replaces
// them.
//
// This file exists for the measurement in
// crates/tjs/src/vm/runtime/bench.rs and for nothing else: every entry times
// one primitive over a corpus it builds before the timed region, and writes
// the checksum the loop feeds so that the optimiser cannot delete the work.
// No engine path calls any of these entries, and the linker keeps them out
// of every binary that does not name one.
//
// The Rust side of the measurement is the twin of every entry here: the same
// corpus, the same iteration count, the same checksum rule. The checksums are
// printed beside the numbers, because two sides that answer differently did
// not do the same work and their ratio measures nothing.

#include "tjsCommHead.h"

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <string>
#include <vector>

#include "tjsConfig.h"
#include "tjsHashSearch.h"
#include "tjsNumberParse.h"
#include "tjsString.h"
#include "tjsVariant.h"

namespace {
    using Clock = std::chrono::steady_clock;

    /// The table the keyed-table entries drive: the shipped member table's
    /// key type, value type and bucket count.
    using MemberTable = TJS::tTJSHashTable<TJS::ttstr, tjs_int32>;

    /// The nanoseconds from `start` to now.
    long long ElapsedNs(Clock::time_point start) {
        return std::chrono::duration_cast<std::chrono::nanoseconds>(
                   Clock::now() - start)
            .count();
    }

    /// Keeps a value opaque to the optimiser without adding any work.
    ///
    /// The empty assembly block emits no instruction; it tells the compiler
    /// that the value has been changed by something it cannot see, so no
    /// iteration's input can be folded into a constant or hoisted out of the
    /// loop. The Rust twin uses `std::hint::black_box` for the same reason.
    template <typename T> void Opaque(T &value) {
        __asm__ __volatile__("" : "+r"(value));
    }

    /// The decimal form of `value` as text.
    std::u16string Decimal(std::size_t value) {
        if(value == 0)
            return std::u16string(1, u'0');
        std::u16string text;
        while(value != 0) {
            text.insert(text.begin(),
                        static_cast<tjs_char>(u'0' + value % 10));
            value /= 10;
        }
        return text;
    }

    /// The member-name corpus the string comparison, hashing and table
    /// entries read: texts of several lengths, a prefix of another text, and
    /// the empty text.
    std::vector<TJS::ttstr> MemberNameCorpus() {
        std::vector<TJS::ttstr> texts;
        texts.reserve(8);
        texts.push_back(TJS::ttstr(TJS_W("x")));
        texts.push_back(TJS::ttstr(TJS_W("xy")));
        texts.push_back(TJS::ttstr(TJS_W("abc")));
        texts.push_back(TJS::ttstr(TJS_W("abd")));
        texts.push_back(TJS::ttstr(TJS_W("length")));
        texts.push_back(TJS::ttstr(TJS_W("member_1")));
        texts.push_back(TJS::ttstr(TJS_W("a longer member name")));
        texts.push_back(TJS::ttstr(TJS_W("")));
        return texts;
    }

    /// The `count` keys the table entries insert: `member_0` upwards.
    std::vector<TJS::ttstr> MemberNames(std::size_t count) {
        std::vector<TJS::ttstr> texts;
        texts.reserve(count);
        for(std::size_t index = 0; index < count; index++) {
            std::u16string text(u"member_");
            text += Decimal(index);
            texts.push_back(TJS::ttstr(text));
        }
        return texts;
    }

    /// The text the escape entry escapes: the control units that have a named
    /// escape, the backslash, both quote kinds, two control units followed by
    /// a hexadecimal digit, and a unit above the control range.
    TJS::ttstr EscapedText() {
        const tjs_char units[] = {
            TJS_W('a'),       static_cast<tjs_char>(0x09), TJS_W('b'),
            static_cast<tjs_char>(0x0a), TJS_W('c'), TJS_W('\\'),
            TJS_W('"'),       TJS_W('\''),                 static_cast<tjs_char>(0x01),
            TJS_W('a'),       static_cast<tjs_char>(0x02), TJS_W('f'),
            static_cast<tjs_char>(0x7f), TJS_W('z'),       0
        };
        return TJS::ttstr(units);
    }

    /// The texts the number parser entry reads: a plain integer, an integer
    /// wider than the value, a real, an exponent form, a hexadecimal form, a
    /// real with ten significant digits, and a text that is not a number.
    std::vector<TJS::ttstr> NumberTextCorpus() {
        std::vector<TJS::ttstr> texts;
        texts.reserve(7);
        texts.push_back(TJS::ttstr(TJS_W("12345")));
        texts.push_back(TJS::ttstr(TJS_W("12345678901234567890")));
        texts.push_back(TJS::ttstr(TJS_W("12.5")));
        texts.push_back(TJS::ttstr(TJS_W("6.02e23")));
        texts.push_back(TJS::ttstr(TJS_W("0x1f")));
        texts.push_back(TJS::ttstr(TJS_W("3.1415926535")));
        texts.push_back(TJS::ttstr(TJS_W("not a number")));
        return texts;
    }

    /// One value the number formatter entry writes.
    struct FormatValue {
        /// Whether the value is the integer in `integer`.
        bool integer;
        /// The integer value, when `integer` holds.
        tjs_int64 integer_value;
        /// The real value, when `integer` does not hold.
        tjs_real real;
    };

    /// The values the formatter entry writes: an integer, a real, a real with
    /// no exact decimal form, a large and a small exponent form, both zeros
    /// and both named forms.
    std::vector<FormatValue> FormatCorpus() {
        std::vector<FormatValue> values;
        values.reserve(8);
        FormatValue value = {true, 1234567890, 0.0};
        values.push_back(value);
        value = {false, 0, 1.5};
        values.push_back(value);
        value = {false, 0, 1.0 / 3.0};
        values.push_back(value);
        value = {false, 0, 1e20};
        values.push_back(value);
        value = {false, 0, 1e-20};
        values.push_back(value);
        value = {false, 0, -0.0};
        values.push_back(value);
        value = {false, 0, std::numeric_limits<tjs_real>::quiet_NaN()};
        values.push_back(value);
        value = {false, 0, std::numeric_limits<tjs_real>::infinity()};
        values.push_back(value);
        return values;
    }

    /// The bits of a real, as the checksum folds them.
    std::uint64_t RealBits(tjs_real value) {
        std::uint64_t bits = 0;
        std::memcpy(&bits, &value, sizeof bits);
        return bits;
    }

    /// The next corpus index from a loop counter, opaque to the optimiser.
    std::size_t NextIndex(long long iteration, std::size_t count) {
        std::size_t index = static_cast<std::size_t>(iteration) % count;
        Opaque(index);
        return index;
    }
} // namespace

extern "C" {

// Times the concatenation of two texts: the same two texts every iteration,
// and the allocation the concatenation itself makes. Measurement only.
long long tjs2_bench_string_concat(long long iterations,
                                   unsigned long long *out_checksum) {
    const TJS::ttstr left(TJS_W("a member name of twenty-one units"));
    const TJS::ttstr right(TJS_W(".bitmap"));
    std::uint64_t checksum = 0;
    long long ns = 0;
    if(iterations > 0) {
        const Clock::time_point start = Clock::now();
        for(long long iteration = 0; iteration < iterations; iteration++) {
            const TJS::ttstr *operand = &left;
            Opaque(operand);
            const TJS::ttstr joined = *operand + right;
            checksum = checksum * 31u +
                static_cast<std::uint64_t>(joined.GetLen() * 31 +
                                           joined[joined.GetLen() - 1]);
        }
        ns = ElapsedNs(start);
    }
    *out_checksum = checksum;
    return ns;
}

// Times the construction of a text from a literal: one allocation per
// iteration, which is the construction's own. Measurement only.
long long tjs2_bench_string_construct(long long iterations,
                                      unsigned long long *out_checksum) {
    const tjs_char *const source = TJS_W("member_1234567890");
    std::uint64_t checksum = 0;
    long long ns = 0;
    if(iterations > 0) {
        const Clock::time_point start = Clock::now();
        for(long long iteration = 0; iteration < iterations; iteration++) {
            const tjs_char *text = source;
            Opaque(text);
            const TJS::ttstr value(text);
            checksum = checksum * 31u +
                static_cast<std::uint64_t>(value.GetLen());
        }
        ns = ElapsedNs(start);
    }
    *out_checksum = checksum;
    return ns;
}

// Times the comparison of two texts out of the member-name corpus: the
// ordinal comparison the language's operators use. Measurement only.
long long tjs2_bench_string_compare(long long iterations,
                                    unsigned long long *out_checksum) {
    const std::vector<TJS::ttstr> texts = MemberNameCorpus();
    const std::size_t count = texts.size();
    std::uint64_t checksum = 0;
    long long ns = 0;
    if(iterations > 0 && count != 0) {
        const Clock::time_point start = Clock::now();
        for(long long iteration = 0; iteration < iterations; iteration++) {
            const std::size_t index = NextIndex(iteration, count);
            const std::size_t other = (index * 5 + 1) % count;
            const int order =
                TJS::TJS_strcmp(texts[index].c_str(), texts[other].c_str());
            checksum = checksum * 31u +
                static_cast<std::uint64_t>(order < 0 ? 1 : (order > 0 ? 3 : 2));
        }
        ns = ElapsedNs(start);
    }
    *out_checksum = checksum;
    return ns;
}

// Times the member hash over the member-name corpus: the value an object's
// member table files a name under. Measurement only.
long long tjs2_bench_string_hash(long long iterations,
                                 unsigned long long *out_checksum) {
    const std::vector<TJS::ttstr> texts = MemberNameCorpus();
    const std::size_t count = texts.size();
    std::uint64_t checksum = 0;
    long long ns = 0;
    if(iterations > 0 && count != 0) {
        const Clock::time_point start = Clock::now();
        for(long long iteration = 0; iteration < iterations; iteration++) {
            const std::size_t index = NextIndex(iteration, count);
            const tjs_uint32 hash =
                TJS::tTJSHashFunc<TJS::ttstr>::Make(texts[index]);
            checksum = checksum * 31u + static_cast<std::uint64_t>(hash);
        }
        ns = ElapsedNs(start);
    }
    *out_checksum = checksum;
    return ns;
}

// Times the escape rule over one text with control units and both quote
// kinds: the allocation the escaped text makes is the escape's own.
// Measurement only.
long long tjs2_bench_string_escape(long long iterations,
                                   unsigned long long *out_checksum) {
    const TJS::ttstr text = EscapedText();
    std::uint64_t checksum = 0;
    long long ns = 0;
    if(iterations > 0) {
        const Clock::time_point start = Clock::now();
        for(long long iteration = 0; iteration < iterations; iteration++) {
            const TJS::ttstr *source = &text;
            Opaque(source);
            const TJS::ttstr escaped = source->EscapeC();
            checksum = checksum * 31u +
                static_cast<std::uint64_t>(escaped.GetLen() * 31 +
                                           escaped[escaped.GetLen() - 1]);
        }
        ns = ElapsedNs(start);
    }
    *out_checksum = checksum;
    return ns;
}

// Times the text-to-number rule over the number corpus: a value per
// iteration, and the integer or real bits folded into the checksum.
// Measurement only.
long long tjs2_bench_number_parse(long long iterations,
                                  unsigned long long *out_checksum) {
    const std::vector<TJS::ttstr> texts = NumberTextCorpus();
    const std::size_t count = texts.size();
    std::uint64_t checksum = 0;
    long long ns = 0;
    if(iterations > 0 && count != 0) {
        const Clock::time_point start = Clock::now();
        for(long long iteration = 0; iteration < iterations; iteration++) {
            const std::size_t index = NextIndex(iteration, count);
            const tjs_char *cursor = texts[index].c_str();
            TJS::tTJSVariant value;
            std::uint64_t folded = 0;
            if(TJS::TJSParseNumber(value, &cursor)) {
                if(value.Type() == TJS::tvtInteger)
                    folded = static_cast<std::uint64_t>(value.AsInteger());
                else
                    folded = RealBits(value.AsReal());
            }
            checksum = checksum * 31u + folded;
        }
        ns = ElapsedNs(start);
    }
    *out_checksum = checksum;
    return ns;
}

// Times the number-to-text rule over the format corpus: the text the rule
// makes, and the allocation it makes for it. Measurement only.
long long tjs2_bench_number_format(long long iterations,
                                   unsigned long long *out_checksum) {
    const std::vector<FormatValue> values = FormatCorpus();
    const std::size_t count = values.size();
    std::uint64_t checksum = 0;
    long long ns = 0;
    if(iterations > 0 && count != 0) {
        const Clock::time_point start = Clock::now();
        for(long long iteration = 0; iteration < iterations; iteration++) {
            const std::size_t index = NextIndex(iteration, count);
            const FormatValue &value = values[index];
            // a formatted number is never empty, so the formatter never
            // answers null
            TJS::tTJSVariantString *text = value.integer
                ? TJS::TJSIntegerToString(value.integer_value)
                : TJS::TJSRealToString(value.real);
            checksum = checksum * 31u +
                static_cast<std::uint64_t>(text->GetLength() * 31 + (*text)[0]);
            text->Release();
        }
        ns = ElapsedNs(start);
    }
    *out_checksum = checksum;
    return ns;
}

// Times `count` insertions into an empty member table, in key order, then one
// lookup outside the timed region so the table is observable. Measurement
// only.
long long tjs2_bench_table_insert(long long count,
                                  unsigned long long *out_checksum) {
    const std::size_t keys_count = static_cast<std::size_t>(count);
    const std::vector<TJS::ttstr> keys = MemberNames(keys_count);
    MemberTable table;
    std::uint64_t checksum = 0;
    long long ns = 0;
    if(count > 0) {
        const Clock::time_point start = Clock::now();
        for(long long index = 0; index < count; index++) {
            const std::size_t key = static_cast<std::size_t>(index);
            table.Add(keys[key], static_cast<tjs_int32>(index));
            checksum = checksum * 31u +
                static_cast<std::uint64_t>(keys[key].GetLen());
        }
        ns = ElapsedNs(start);
        const tjs_int32 *last = table.Find(keys[keys_count - 1]);
        checksum = checksum * 31u +
            (last != nullptr ? static_cast<std::uint64_t>(*last) : 0u);
    }
    *out_checksum = checksum;
    return ns;
}

// Times `count` lookups in a table filled with the same keys before the timed
// region. Measurement only.
long long tjs2_bench_table_find(long long count,
                                unsigned long long *out_checksum) {
    const std::size_t keys_count = static_cast<std::size_t>(count);
    const std::vector<TJS::ttstr> keys = MemberNames(keys_count);
    MemberTable table;
    for(long long index = 0; index < count; index++) {
        table.Add(keys[static_cast<std::size_t>(index)],
                  static_cast<tjs_int32>(index));
    }
    std::uint64_t checksum = 0;
    long long ns = 0;
    if(count > 0) {
        const Clock::time_point start = Clock::now();
        for(long long index = 0; index < count; index++) {
            const tjs_int32 *value =
                table.Find(keys[static_cast<std::size_t>(index)]);
            checksum = checksum * 31u +
                (value != nullptr ? static_cast<std::uint64_t>(*value) : 0u);
        }
        ns = ElapsedNs(start);
    }
    *out_checksum = checksum;
    return ns;
}

// Times `count` removals from a table filled with the same keys before the
// timed region. Measurement only.
long long tjs2_bench_table_remove(long long count,
                                  unsigned long long *out_checksum) {
    const std::size_t keys_count = static_cast<std::size_t>(count);
    const std::vector<TJS::ttstr> keys = MemberNames(keys_count);
    MemberTable table;
    for(long long index = 0; index < count; index++) {
        table.Add(keys[static_cast<std::size_t>(index)],
                  static_cast<tjs_int32>(index));
    }
    std::uint64_t checksum = 0;
    long long ns = 0;
    if(count > 0) {
        const Clock::time_point start = Clock::now();
        for(long long index = 0; index < count; index++) {
            const bool removed =
                table.Delete(keys[static_cast<std::size_t>(index)]);
            checksum = checksum * 31u + (removed ? 1u : 0u);
        }
        ns = ElapsedNs(start);
    }
    *out_checksum = checksum;
    return ns;
}

} // extern "C"

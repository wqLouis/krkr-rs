// The proof instrument: the two paths, compared.
//
// The C++ path is the compiler's own grammar and generator; the Rust path
// is the adapter that builds the block from the image. Both run in this
// process, over the same text, and the two blocks are compared field by
// field.
//
// This file is a proof instrument. It is not part of the shipped tree.

#include "tjsCommHead.h"

#include "tjsInterCodeGen.h"
#include "tjsScriptBlock.h"
#include "tjs.h"
#include "tjs2_codegen.h"
#include "tjs2_abi.h"

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

using namespace TJS;

namespace {

    //---------------------------------------------------------------------------
    // The loggers the C++ tree logs through.
    //
    // The reference's own main() registers them, and the bridge registers
    // them when it makes an engine. The proof drives the C++ tree directly,
    // so it makes one bridge engine first: without it the tree's error path
    // logs through a logger that was never registered, and the null logger
    // takes the process down.
    //---------------------------------------------------------------------------
    void EnsureLoggers() {
        tjs2_engine *engine = tjs2_create();
        if(engine)
            tjs2_destroy(engine);
    }

    //---------------------------------------------------------------------------
    // The name of a context, as text the report can hold.
    //---------------------------------------------------------------------------
    std::string Narrow(const tjs_char *text) {
        std::string result;
        for(tjs_int index = 0; text[index]; index++)
            result += (char)(text[index] < 128 ? text[index] : '?');
        return result;
    }

    //---------------------------------------------------------------------------
    // The text of a value, by what the value is.
    //
    // An object is compared by its members: the names, the order and the
    // values they hold. The depth keeps a cycle from running away.
    //---------------------------------------------------------------------------
    std::string ValueText(const tTJSVariant &value, int depth);

    //---------------------------------------------------------------------------
    // The members of an object, in the object's own order.
    //
    // The reference enumerates an object's members with EnumMembers and
    // a callback; this is that callback.
    //---------------------------------------------------------------------------
    class tMemberCollector : public tTJSDispatch {
    public:
        // The name of a member and the text of its value, in the order the
        // object holds them.
        std::vector<std::pair<std::string, std::string>> Members;

        tjs_error FuncCall(tjs_uint32 flag, const tjs_char *membername,
                           tjs_uint32 *hint, tTJSVariant *result,
                           tjs_int numparams, tTJSVariant **param,
                           iTJSDispatch2 *objthis) override {
            if(numparams < 3)
                return TJS_E_BADPARAMCOUNT;

            // the hidden members are the object's own, not the ones the
            // script sees: the reference's own enumeration skips them
            tjs_uint32 flags = (tjs_int)*param[1];
            if(flags & TJS_HIDDENMEMBER) {
                if(result)
                    *result = (tjs_int)1;
                return TJS_S_OK;
            }

            Members.push_back(std::make_pair(ValueText(*param[0], 1),
                                             ValueText(*param[2], 1)));
            if(result)
                *result = (tjs_int)1;
            return TJS_S_OK;
        }
    };

    //---------------------------------------------------------------------------
    // The text of a value, by what the value is.
    //
    // An object is compared by its members: the names, the order and the
    // values they hold. The depth keeps a cycle from running away.
    //---------------------------------------------------------------------------
    std::string ValueText(const tTJSVariant &value, int depth) {
        switch(value.Type()) {
            case tvtVoid:
                return "void";
            case tvtInteger:
                return "int:" + std::to_string((long long)value.AsInteger());
            case tvtReal: {
                char buffer[64];
                snprintf(buffer, sizeof(buffer), "real:%.17g",
                         (double)value.AsReal());
                return buffer;
            }
            case tvtString: {
                tTJSVariantString *text = value.AsString();
                if(!text)
                    return "string:";
                std::string result = "string:";
                const tjs_char *chars = text->operator const tjs_char *();
                for(tjs_int index = 0; index < text->GetLength(); index++)
                    result += (char)(chars[index] < 128 ? chars[index] : '?');
                text->Release();
                return result;
            }
            case tvtOctet: {
                tTJSVariantOctet *octet = value.AsOctetNoAddRef();
                if(!octet)
                    return "octet:";
                std::string result = "octet:";
                const tjs_uint8 *bytes = octet->GetData();
                for(tjs_uint index = 0; index < octet->GetLength(); index++) {
                    char buffer[8];
                    snprintf(buffer, sizeof(buffer), "%02x", bytes[index]);
                    result += buffer;
                }
                return result;
            }
            case tvtObject: {
                iTJSDispatch2 *object = value.AsObjectNoAddRef();
                if(!object)
                    return "object:null";
                if(depth >= 4)
                    return "object:deep";
                tMemberCollector collector;
                tTJSVariantClosure closure(&collector, nullptr);
                object->EnumMembers(TJS_IGNOREPROP, &closure, object);
                std::string result = "object{";
                result += std::to_string(collector.Members.size());
                result += "}";
                for(auto &member : collector.Members) {
                    result += "[";
                    result += member.first;
                    result += "]=";
                    result += member.second;
                }
                return result;
            }
        }
        return "unknown";
    }

    //---------------------------------------------------------------------------
    // The text of one constant, so that two constant areas can be compared.
    //---------------------------------------------------------------------------
    std::string ConstantText(const tTJSVariant &value, tTJSScriptBlock *block) {
        switch(value.Type()) {
            case tvtVoid:
                return "void";
            case tvtInteger:
                return "int:" + std::to_string((long long)value.AsInteger());
            case tvtReal: {
                char buffer[64];
                snprintf(buffer, sizeof(buffer), "real:%.17g",
                         (double)value.AsReal());
                return buffer;
            }
            case tvtString: {
                tTJSVariantString *text = value.AsString();
                if(!text)
                    return "string:";
                std::string result = "string:";
                const tjs_char *chars = text->operator const tjs_char *();
                for(tjs_int index = 0; index < text->GetLength(); index++)
                    result += (char)(chars[index] < 128 ? chars[index] : '?');
                text->Release();
                return result;
            }
            case tvtOctet: {
                tTJSVariantOctet *octet = value.AsOctetNoAddRef();
                if(!octet)
                    return "octet:";
                char buffer[64];
                snprintf(buffer, sizeof(buffer), "octet:%u",
                         (unsigned)octet->GetLength());
                return buffer;
            }
            case tvtObject: {
                iTJSDispatch2 *object = value.AsObjectNoAddRef();
                if(!object)
                    return "object:null";
                tTJSInterCodeContext *context =
                    dynamic_cast<tTJSInterCodeContext *>(object);
                if(context)
                    return "context:" +
                        std::to_string(block->GetCodeIndex(context));
                // a constant inline object: its members, in order, by
                // value
                return ValueText(value, 0);
            }
        }
        return "unknown";
    }

    //---------------------------------------------------------------------------
    // What one context holds.
    //---------------------------------------------------------------------------
    struct ContextData {
        int kind = 0;
        std::string name;
        std::vector<int> code;
        std::vector<std::string> data;
        std::vector<std::pair<int, int>> positions;
        int max_variable_count = 0;
        int variable_reserve_count = 0;
        int max_frame_count = 0;
        int func_arg_count = 0;
        int func_unnamed_arg_base = 0;
        int func_collapse_base = 0;
        int parent = -1;
        int prop_setter = -1;
        int prop_getter = -1;
        int super_class_getter = -1;
        int member_count = 0;
    };

    int IndexOf(tTJSScriptBlock *block, tTJSInterCodeContext *context) {
        if(!context)
            return -1;
        return block->GetCodeIndex(context);
    }

    void Collect(tTJSScriptBlock *block, std::vector<ContextData> *out) {
        const std::list<tTJSInterCodeContext *> &list = block->GetContextList();
        for(auto context : list) {
            ContextData data;
            data.kind = (int)context->GetContextType();
            const tjs_char *name = context->GetName();
            data.name = name ? Narrow(name) : std::string();
            tjs_uint size = context->GetCodeSize();
            const tjs_int32 *code = context->GetCodeArea();
            for(tjs_uint index = 0; index < size; index++)
                data.code.push_back((int)code[index]);
            const tTJSVariant *area = context->GetDataArea();
            tjs_int areaSize = context->GetDataSize();
            for(tjs_int index = 0; index < areaSize; index++)
                data.data.push_back(ConstantText(area[index], block));
            const tTJSInterCodeContext::tSourcePos *positions =
                context->GetSourcePosArray();
            tjs_int positionCount = context->GetSourcePosArraySize();
            for(tjs_int index = 0; index < positionCount; index++)
                data.positions.push_back(
                    std::make_pair((int)positions[index].CodePos,
                                    (int)positions[index].SourcePos));
            data.max_variable_count = context->GetMaxVariableCount();
            data.variable_reserve_count = context->GetVariableReserveCount();
            data.max_frame_count = context->GetMaxFrameCount();
            data.func_arg_count = context->GetFuncDeclArgCount();
            data.func_unnamed_arg_base = context->GetFuncDeclUnnamedArgArrayBase();
            data.func_collapse_base = context->GetFuncDeclCollapseBase();
            data.parent = IndexOf(block, context->GetParentContext());
            data.prop_setter = IndexOf(block, context->GetPropSetterContext());
            data.prop_getter = IndexOf(block, context->GetPropGetterContext());
            data.super_class_getter =
                IndexOf(block, context->GetSuperClassGetterContext());
            tjs_int count = 0;
            context->GetCount(&count, nullptr, nullptr, context);
            data.member_count = (int)count;
            out->push_back(data);
        }
    }

    //---------------------------------------------------------------------------
    // The difference between two values, as one line.
    //---------------------------------------------------------------------------
    void ReportDifference(std::string *report, const char *what, int index,
                           const std::string &left, const std::string &right) {
        char buffer[64];
        snprintf(buffer, sizeof(buffer), "  context %d ", index);
        *report += buffer;
        *report += what;
        *report += ": cpp=";
        *report += left;
        *report += " rust=";
        *report += right;
        *report += "\n";
    }

    std::string JoinCode(const std::vector<int> &code) {
        std::string result;
        char buffer[16];
        for(size_t index = 0; index < code.size(); index++) {
            snprintf(buffer, sizeof(buffer), "%d ", code[index]);
            result += buffer;
        }
        return result;
    }

    std::string JoinData(const std::vector<std::string> &data) {
        std::string result;
        for(size_t index = 0; index < data.size(); index++) {
            if(index)
                result += ",";
            result += data[index];
        }
        return result;
    }

    std::string JoinPositions(const std::vector<std::pair<int, int>> &positions) {
        std::string result;
        char buffer[64];
        for(size_t index = 0; index < positions.size(); index++) {
            snprintf(buffer, sizeof(buffer), "%d:%d ", positions[index].first,
                     positions[index].second);
            result += buffer;
        }
        return result;
    }

    std::string Number(int value) { return std::to_string(value); }

    //---------------------------------------------------------------------------
    // Compares the two blocks.
    //---------------------------------------------------------------------------
    void Compare(tTJSScriptBlock *cpp, tTJSScriptBlock *rust,
                 std::string *report) {
        std::vector<ContextData> left;
        std::vector<ContextData> right;
        Collect(cpp, &left);
        Collect(rust, &right);

        if(left.size() != right.size()) {
            char buffer[128];
            snprintf(buffer, sizeof(buffer),
                     "  the context count differs: cpp=%d rust=%d\n",
                     (int)left.size(), (int)right.size());
            *report += buffer;
        }

        size_t count = left.size() < right.size() ? left.size() : right.size();
        for(size_t index = 0; index < count; index++) {
            const ContextData &a = left[index];
            const ContextData &b = right[index];
            int number = (int)index;
            if(a.kind != b.kind)
                ReportDifference(report, "kind", number, Number(a.kind),
                                 Number(b.kind));
            if(a.name != b.name)
                ReportDifference(report, "name", number, a.name, b.name);
            if(a.code != b.code)
                ReportDifference(report, "code", number, JoinCode(a.code),
                                 JoinCode(b.code));
            if(a.data != b.data)
                ReportDifference(report, "constants", number, JoinData(a.data),
                                 JoinData(b.data));
            if(a.positions != b.positions)
                ReportDifference(report, "source positions", number,
                                 JoinPositions(a.positions),
                                 JoinPositions(b.positions));
            if(a.max_variable_count != b.max_variable_count)
                ReportDifference(report, "max variable count", number,
                                 Number(a.max_variable_count),
                                 Number(b.max_variable_count));
            if(a.variable_reserve_count != b.variable_reserve_count)
                ReportDifference(report, "variable reserve count", number,
                                 Number(a.variable_reserve_count),
                                 Number(b.variable_reserve_count));
            if(a.max_frame_count != b.max_frame_count)
                ReportDifference(report, "max frame count", number,
                                 Number(a.max_frame_count),
                                 Number(b.max_frame_count));
            if(a.func_arg_count != b.func_arg_count)
                ReportDifference(report, "argument count", number,
                                 Number(a.func_arg_count),
                                 Number(b.func_arg_count));
            if(a.func_unnamed_arg_base != b.func_unnamed_arg_base)
                ReportDifference(report, "unnamed argument base", number,
                                 Number(a.func_unnamed_arg_base),
                                 Number(b.func_unnamed_arg_base));
            if(a.func_collapse_base != b.func_collapse_base)
                ReportDifference(report, "collapse base", number,
                                 Number(a.func_collapse_base),
                                 Number(b.func_collapse_base));
            if(a.parent != b.parent)
                ReportDifference(report, "parent", number, Number(a.parent),
                                 Number(b.parent));
            if(a.prop_setter != b.prop_setter)
                ReportDifference(report, "property setter", number,
                                 Number(a.prop_setter), Number(b.prop_setter));
            if(a.prop_getter != b.prop_getter)
                ReportDifference(report, "property getter", number,
                                 Number(a.prop_getter), Number(b.prop_getter));
            if(a.super_class_getter != b.super_class_getter)
                ReportDifference(report, "super class getter", number,
                                 Number(a.super_class_getter),
                                 Number(b.super_class_getter));
            if(a.member_count != b.member_count)
                ReportDifference(report, "member count", number,
                                 Number(a.member_count), Number(b.member_count));
        }
    }

} // namespace

//---------------------------------------------------------------------------
// Compiles the text with both paths and compares the two blocks.
//
// The report holds one line per difference. The return value is the number
// of differences, or -1 when one of the paths failed to compile.
//---------------------------------------------------------------------------
extern "C" int tjs2_proof_compare(const uint16_t *text, uint32_t length,
                                   char *report, uint32_t reportSize) {
    std::string result;
    result.reserve(4096);

    EnsureLoggers();

    // The two paths get an engine each: the conditional-compilation
    // values live in the engine, and a path that ran first would leave
    // its values behind for the other.
    tTJS *cppEngine = new tTJS();
    tTJS *rustEngine = new tTJS();

    std::vector<tjs_char> script;
    script.reserve(length + 1);
    for(uint32_t index = 0; index < length; index++)
        script.push_back((tjs_char)text[index]);
    script.push_back(0);

    tTJSScriptBlock *cpp = nullptr;
    tTJSScriptBlock *rust = nullptr;

    std::string cppError;
    std::string rustError;

    try {
        cpp = new tTJSScriptBlock(cppEngine);
        cpp->SetSourceText(script.data());
        cpp->ParseReference(script.data(), false, false);
    } catch(eTJSScriptError &error) {
        cppError = error.what();
    } catch(eTJSError &error) {
        cppError = error.what();
    } catch(...) {
        cppError = "an unknown failure";
    }

    try {
        rust = new tTJSScriptBlock(rustEngine);
        rust->SetSourceText(script.data());
        rust->Parse(script.data(), false, false);
    } catch(eTJSScriptError &error) {
        rustError = error.what();
    } catch(eTJSError &error) {
        rustError = error.what();
    } catch(...) {
        rustError = "an unknown failure";
    }

    int differences = 0;
    if(!cppError.empty() || !rustError.empty()) {
        result += "  the cpp path: " + (cppError.empty() ? "ok" : cppError) +
                  "\n";
        result += "  the rust path: " + (rustError.empty() ? "ok" : rustError) +
                  "\n";
        differences = -1;
    } else {
        Compare(cpp, rust, &result);
        differences = 0;
        for(size_t index = 0; index < result.size(); index++)
            if(result[index] == '\n')
                differences++;
    }

    if(report && reportSize) {
        uint32_t size = (uint32_t)result.size();
        if(size >= reportSize)
            size = reportSize - 1;
        memcpy(report, result.data(), size);
        report[size] = 0;
    }

    if(cpp)
        cpp->Release();
    if(rust)
        rust->Release();
    cppEngine->Release();
    rustEngine->Release();

    return differences;
}

//---------------------------------------------------------------------------
// Dumps the contexts of the two paths, so that they can be lined up by
// name and not only by index.
//
// This is a proof instrument. It is not part of the shipped tree.
//---------------------------------------------------------------------------
extern "C" int tjs2_proof_dump(const uint16_t *text, uint32_t length,
                                char *report, uint32_t reportSize) {
    std::string result;

    EnsureLoggers();

    tTJS *cppEngine = new tTJS();
    tTJS *rustEngine = new tTJS();

    std::vector<tjs_char> script;
    script.reserve(length + 1);
    for(uint32_t index = 0; index < length; index++)
        script.push_back((tjs_char)text[index]);
    script.push_back(0);

    const char *labels[2] = { "CPP", "RUST" };
    for(int which = 0; which < 2; which++) {
        tTJS *engine = which == 0 ? cppEngine : rustEngine;
        try {
            tTJSScriptBlock *block = new tTJSScriptBlock(engine);
            block->SetSourceText(script.data());
            if(which == 0)
                block->ParseReference(script.data(), false, false);
            else
                block->Parse(script.data(), false, false);
            int index = 0;
            for(auto context : block->GetContextList()) {
                char buffer[512];
                snprintf(buffer, sizeof(buffer), "%s %d kind=%d name=%s code=%d data=%d\n",
                         labels[which], index++, (int)context->GetContextType(),
                         Narrow(context->GetName()).c_str(),
                         (int)context->GetCodeSize(), (int)context->GetDataSize());
                result += buffer;
            }
            block->Release();
        } catch(...) {
            result += std::string(labels[which]) + " failed\n";
        }
    }

    if(report && reportSize) {
        uint32_t size = (uint32_t)result.size();
        if(size >= reportSize)
            size = reportSize - 1;
        memcpy(report, result.data(), size);
        report[size] = 0;
    }

    cppEngine->Release();
    rustEngine->Release();
    return 0;
}

//---------------------------------------------------------------------------
// Runs a script through the live path and reports the result.
//
// The proof instrument: it shows that the block the adapter builds runs.
//
// This is a proof instrument. It is not part of the shipped tree.
//---------------------------------------------------------------------------
extern "C" int tjs2_proof_run(const uint16_t *text, uint32_t length,
                               char *report, uint32_t reportSize) {
    std::string result;

    EnsureLoggers();

    tTJS *engine = new tTJS();
    std::vector<tjs_char> script;
    script.reserve(length + 1);
    for(uint32_t index = 0; index < length; index++)
        script.push_back((tjs_char)text[index]);
    script.push_back(0);

    try {
        tTJSVariant value;
        engine->ExecScript(script.data(), &value, nullptr, 0);
        if(value.Type() == tvtInteger) {
            result = "integer " + std::to_string((long long)value.AsInteger());
        } else if(value.Type() == tvtReal) {
            result = "real " + std::to_string((double)value.AsReal());
        } else if(value.Type() == tvtString) {
            result = "string " + Narrow(value.GetString());
        } else if(value.Type() == tvtVoid) {
            result = "void";
        } else {
            result = "other";
        }
    } catch(eTJSScriptError &error) {
        result = std::string("script error: ") + error.what();
    } catch(eTJSError &error) {
        result = std::string("error: ") + error.what();
    } catch(...) {
        result = "an unknown failure";
    }

    if(report && reportSize) {
        uint32_t size = (uint32_t)result.size();
        if(size >= reportSize)
            size = reportSize - 1;
        memcpy(report, result.data(), size);
        report[size] = 0;
    }

    engine->Release();
    return 0;
}

//---------------------------------------------------------------------------
// Runs a script through the compiler's own path and reports the result.
//
// The proof instrument: the same script, the reference's own generator.
//
// This is a proof instrument. It is not part of the shipped tree.
//---------------------------------------------------------------------------
extern "C" int tjs2_proof_run_reference(const uint16_t *text, uint32_t length,
                                         char *report, uint32_t reportSize) {
    std::string result;

    EnsureLoggers();

    tTJS *engine = new tTJS();
    std::vector<tjs_char> script;
    script.reserve(length + 1);
    for(uint32_t index = 0; index < length; index++)
        script.push_back((tjs_char)text[index]);
    script.push_back(0);

    try {
        tTJSScriptBlock *block = new tTJSScriptBlock(engine);
        block->SetSourceText(script.data());
        block->ParseReference(script.data(), false, false);
        tTJSVariant value;
        block->ExecuteTopLevelScript(&value, nullptr);
        if(value.Type() == tvtInteger) {
            result = "integer " + std::to_string((long long)value.AsInteger());
        } else if(value.Type() == tvtReal) {
            result = "real " + std::to_string((double)value.AsReal());
        } else if(value.Type() == tvtString) {
            result = "string " + Narrow(value.GetString());
        } else if(value.Type() == tvtVoid) {
            result = "void";
        } else {
            result = "other";
        }
        block->Release();
    } catch(eTJSScriptError &error) {
        result = std::string("script error: ") + error.what();
    } catch(eTJSError &error) {
        result = std::string("error: ") + error.what();
    } catch(...) {
        result = "an unknown failure";
    }

    if(report && reportSize) {
        uint32_t size = (uint32_t)result.size();
        if(size >= reportSize)
            size = reportSize - 1;
        memcpy(report, result.data(), size);
        report[size] = 0;
    }

    engine->Release();
    return 0;
}

//---------------------------------------------------------------------------
// The bytecode round trip: the script is compiled to bytecode through the
// adapter's path, and the bytecode is read back and run.
//
// This is a proof instrument. It is not part of the shipped tree.
//---------------------------------------------------------------------------
extern "C" int tjs2_proof_bytecode(const uint16_t *text, uint32_t length,
                                    char *report, uint32_t reportSize) {
    std::string result;

    std::string utf8;
    for(uint32_t index = 0; index < length; index++) {
        if(text[index] < 128)
            utf8 += (char)text[index];
        else
            utf8 += '?';
    }

    const char *path = "/tmp/krkr-vm/adapter/proof-bytecode.tjsc";
    tjs2_engine *engine = tjs2_create();
    char *error = nullptr;
    int status = tjs2_compile_script(engine, utf8.data(), utf8.size(), path, 0,
                                     0, 0, "proof", 0, &error);
    if(status != 0) {
        result = std::string("the compile failed: ") +
                 (error ? error : "(no message)");
        if(error)
            tjs2_free_string(error);
    } else {
        FILE *file = fopen(path, "rb");
        std::string bytes;
        if(file) {
            char buffer[65536];
            size_t count;
            while((count = fread(buffer, 1, sizeof(buffer), file)) > 0)
                bytes.append(buffer, count);
            fclose(file);
        }
        tjs2_value result_value;
        int is_bytecode = 0;
        error = nullptr;
        status = tjs2_load_bytecode(engine, bytes.data(), bytes.size(), "proof",
                                    nullptr, &is_bytecode, &result_value, &error);
        if(status != 0) {
            result = std::string("the load failed: ") +
                     (error ? error : "(no message)");
            if(error)
                tjs2_free_string(error);
        } else {
            result = "the bytecode ran; the bytes: " +
                     std::to_string(bytes.size());
        }
    }
    tjs2_destroy(engine);

    if(report && reportSize) {
        uint32_t size = (uint32_t)result.size();
        if(size >= reportSize)
            size = reportSize - 1;
        memcpy(report, result.data(), size);
        report[size] = 0;
    }
    return 0;
}

//---------------------------------------------------------------------------
// The conditional-compilation values across two compiles.
//
// The first script sets a value; the second reads it. The two scripts run
// in one engine, so the value the first one set is visible to the second
// one — the way the reference's pre-processor makes it visible.
//
// This is a proof instrument. It is not part of the shipped tree.
//---------------------------------------------------------------------------
extern "C" int tjs2_proof_values(const uint16_t *first, uint32_t firstLength,
                                  const uint16_t *second, uint32_t secondLength,
                                  int which, char *report, uint32_t reportSize) {
    std::string result;

    EnsureLoggers();

    tTJS *engine = new tTJS();

    std::vector<tjs_char> one;
    std::vector<tjs_char> two;
    one.reserve(firstLength + 1);
    for(uint32_t index = 0; index < firstLength; index++)
        one.push_back((tjs_char)first[index]);
    one.push_back(0);
    two.reserve(secondLength + 1);
    for(uint32_t index = 0; index < secondLength; index++)
        two.push_back((tjs_char)second[index]);
    two.push_back(0);

    try {
        // the first script: it sets the value
        {
            tTJSScriptBlock *block = new tTJSScriptBlock(engine);
            block->SetSourceText(one.data());
            if(which == 0)
                block->ParseReference(one.data(), false, false);
            else
                block->Parse(one.data(), false, false);
            block->Release();
        }
        // the second script: it reads the value
        tTJSScriptBlock *block = new tTJSScriptBlock(engine);
        block->SetSourceText(two.data());
        if(which == 0)
            block->ParseReference(two.data(), false, false);
        else
            block->Parse(two.data(), false, false);
        tjs_uint size = 0;
        for(auto context : block->GetContextList())
            size += context->GetCodeSize();
        result = "the second script's code words: " + std::to_string(size);
        block->Release();
    } catch(eTJSScriptError &error) {
        result = std::string("script error: ") + error.what();
    } catch(eTJSError &error) {
        result = std::string("error: ") + error.what();
    } catch(...) {
        result = "an unknown failure";
    }

    if(report && reportSize) {
        uint32_t size = (uint32_t)result.size();
        if(size >= reportSize)
            size = reportSize - 1;
        memcpy(report, result.data(), size);
        report[size] = 0;
    }

    engine->Release();
    return 0;
}

//---------------------------------------------------------------------------
// The identity of a constant inline object.
//
// The reference builds the object once, at compile time, and every
// reference to that constant sees the same object: a write to it is seen
// by the next read. The scripts below are the instrument's own; each one
// is run through both paths and the two answers are compared.
//
// The reference's answer is the truth: the gate is that the two paths
// agree.
//---------------------------------------------------------------------------
namespace {

    //---------------------------------------------------------------------------
    // The text of a script, as the tree's own string.
    //---------------------------------------------------------------------------
    std::vector<tjs_char> AsciiText(const char *text) {
        std::vector<tjs_char> result;
        for(const char *at = text; *at; at++)
            result.push_back((tjs_char)(unsigned char)*at);
        result.push_back(0);
        return result;
    }

    //---------------------------------------------------------------------------
    // The result of a script, as text.
    //
    // The reference path is the compiler's own grammar; the other path is
    // the adapter's, the one the game runs.
    //---------------------------------------------------------------------------
    std::string RunScript(const std::vector<tjs_char> &script, bool reference) {
        tTJS *engine = new tTJS();
        std::string result;
        try {
            tTJSScriptBlock *block = new tTJSScriptBlock(engine);
            block->SetSourceText(script.data());
            if(reference)
                block->ParseReference(script.data(), false, false);
            else
                block->Parse(script.data(), false, false);
            tTJSVariant value;
            block->ExecuteTopLevelScript(&value, nullptr);
            // the value the script left in the global object: the script's
            // own result is void, so the observable is the global
            tTJSVariant left;
            iTJSDispatch2 *global = engine->GetGlobalNoAddRef();
            global->PropGet(TJS_MEMBERENSURE, TJS_W("proof_value"), nullptr,
                            &left, global);
            result = ValueText(left, 0);
            block->Release();
        } catch(eTJSScriptError &error) {
            result = std::string("script error: ") + error.what();
        } catch(eTJSError &error) {
            result = std::string("error: ") + error.what();
        } catch(...) {
            result = "an unknown failure";
        }
        engine->Release();
        return result;
    }

    //---------------------------------------------------------------------------
    // The identity cases.
    //---------------------------------------------------------------------------
    struct IdentityCase {
        const char *name;
        const char *script;
    };

    const IdentityCase IDENTITY_CASES[] = {
        {"the constant dictionary is written and read back",
         "var d;\n"
         "for (var i = 0; i < 2; i++) {\n"
         "    d = (const)%[\"x\",1];\n"
         "    if (i == 0) d.x = 42;\n"
         "}\n"
         "var proof_value = d.x;\n"},
        {"the constant array is written and read back",
         "var e;\n"
         "for (var i = 0; i < 2; i++) {\n"
         "    e = (const)[7,8];\n"
         "    if (i == 0) e[0] = 99;\n"
         "}\n"
         "var proof_value = e[0];\n"},
        {"two constant dictionaries are two objects",
         "var a = (const)%[\"x\",1];\n"
         "var b = (const)%[\"x\",1];\n"
         "a.x = 42;\n"
         "var proof_value = b.x;\n"},
        {"the constant dictionary keeps its members",
         "var d = (const)%[\"x\",1,\"y\",2];\n"
         "var proof_value = d.x + d.y;\n"},
        {"the constant array keeps its elements",
         "var e = (const)[3,4,5];\n"
         "var proof_value = e[0] + e[1] + e[2];\n"},
        {"the nested constant array is one object",
         "var a = (const)[(const)[1,2]];\n"
         "a[0][0] = 42;\n"
         "var proof_value = a[0][0];\n"},
        {"the nested constant dictionary is one object",
         "var a = (const)%[\"k\",(const)%[\"x\",1]];\n"
         "a.k.x = 42;\n"
         "var proof_value = a.k.x;\n"},
    };

} // namespace

extern "C" int tjs2_proof_identity(char *report, uint32_t reportSize) {
    EnsureLoggers();

    std::string result;
    int differences = 0;

    for(const auto &one : IDENTITY_CASES) {
        std::vector<tjs_char> script = AsciiText(one.script);
        std::string left = RunScript(script, true);
        std::string right = RunScript(script, false);
        bool same = left == right;
        if(!same)
            differences++;
        result += "  ";
        result += one.name;
        result += "\n";
        result += "    the reference: ";
        result += left;
        result += "\n";
        result += "    the rust: ";
        result += right;
        result += "\n";
        result += same ? "    the same\n" : "    DIFFERENT\n";
    }

    if(report && reportSize) {
        uint32_t size = (uint32_t)result.size();
        if(size >= reportSize)
            size = reportSize - 1;
        memcpy(report, result.data(), size);
        report[size] = 0;
    }

    return differences;
}

//---------------------------------------------------------------------------
// The bytecode export, compared.
//
// The export is the artefact the game loads, so the two paths must write
// the same file. The file is read back field by field: the constants, the
// code, the relations and the member list every context carries.
//---------------------------------------------------------------------------
namespace {

    //---------------------------------------------------------------------------
    // A stream that keeps the bytes in memory.
    //---------------------------------------------------------------------------
    class tMemoryStream : public tTJSBinaryStream {
    public:
        std::vector<tjs_uint8> Buffer;
        tjs_uint64 Position = 0;

        tjs_uint64 Seek(tjs_int64 offset, tjs_int whence) override {
            tjs_int64 base = 0;
            if(whence == SEEK_SET)
                base = 0;
            else if(whence == SEEK_CUR)
                base = (tjs_int64)Position;
            else
                base = (tjs_int64)Buffer.size();
            tjs_int64 target = base + offset;
            if(target < 0)
                target = 0;
            if(target > (tjs_int64)Buffer.size())
                target = (tjs_int64)Buffer.size();
            Position = (tjs_uint64)target;
            return Position;
        }

        tjs_uint Read(void *buffer, tjs_uint read_size) override {
            tjs_uint available = 0;
            if(Position < Buffer.size())
                available = (tjs_uint)(Buffer.size() - Position);
            if(read_size > available)
                read_size = available;
            if(read_size) {
                memcpy(buffer, &Buffer[(size_t)Position], read_size);
                Position += read_size;
            }
            return read_size;
        }

        tjs_uint Write(const void *buffer, tjs_uint write_size) override {
            if(Position + write_size > Buffer.size())
                Buffer.resize((size_t)(Position + write_size), 0);
            memcpy(&Buffer[(size_t)Position], buffer, write_size);
            Position += write_size;
            return write_size;
        }

        tjs_uint64 GetSize() override { return (tjs_uint64)Buffer.size(); }
    };

    //---------------------------------------------------------------------------
    // The bytes of a file, read field by field.
    //---------------------------------------------------------------------------
    struct ByteReader {
        const tjs_uint8 *data = nullptr;
        size_t size = 0;
        size_t position = 0;
        bool ok = true;

        uint32_t U32() {
            if(position + 4 > size) {
                ok = false;
                return 0;
            }
            uint32_t value = (uint32_t)data[position] |
                             ((uint32_t)data[position + 1] << 8) |
                             ((uint32_t)data[position + 2] << 16) |
                             ((uint32_t)data[position + 3] << 24);
            position += 4;
            return value;
        }

        uint16_t U16() {
            if(position + 2 > size) {
                ok = false;
                return 0;
            }
            uint16_t value =
                (uint16_t)((uint16_t)data[position] | ((uint16_t)data[position + 1] << 8));
            position += 2;
            return value;
        }

        void Skip(size_t count) {
            if(position + count > size) {
                ok = false;
                position = size;
                return;
            }
            position += count;
        }

        bool Tag(const char *tag) {
            if(position + 4 > size) {
                ok = false;
                return false;
            }
            return memcmp(data + position, tag, 4) == 0;
        }
    };

    //---------------------------------------------------------------------------
    // One context's record in the export.
    //---------------------------------------------------------------------------
    struct ExportContext {
        // the counts and the relations
        std::vector<int32_t> Fields;
        // the source positions
        int32_t SourceCount = 0;
        std::vector<int32_t> SourceCodePos;
        std::vector<int32_t> SourceSrcPos;
        // the code
        int32_t CodeSize = 0;
        std::vector<uint16_t> Code;
        // the constants
        int32_t DataSize = 0;
        std::vector<uint32_t> Data;
        // the super class getter's places
        int32_t ScgpCount = 0;
        std::vector<int32_t> Scgp;
        // the member list
        int32_t PropertyCount = 0;
        std::vector<uint32_t> Properties;
    };

    const char *const FIELD_NAMES[] = {
        "the parent", "the name", "the kind", "the variable count",
        "the variable reserve count", "the frame count", "the argument count",
        "the unnamed argument base", "the collapse base", "the property setter",
        "the property getter", "the super class getter"};

    //---------------------------------------------------------------------------
    // The export of a block, read back.
    //---------------------------------------------------------------------------
    struct Export {
        bool ok = false;
        std::vector<uint8_t> Data;
        int32_t TopLevel = -1;
        std::vector<ExportContext> Contexts;
        std::vector<uint8_t> Source;
        std::string failure;
    };

    Export ReadExport(const std::vector<tjs_uint8> &bytes) {
        Export result;
        ByteReader reader;
        reader.data = bytes.data();
        reader.size = bytes.size();

        if(bytes.size() < 12) {
            result.failure = "the file is too short";
            return result;
        }
        reader.Skip(8); // the file tag
        reader.U32();  // the file size

        // the constants
        if(!reader.Tag("DATA")) {
            result.failure = "the constants section is missing";
            return result;
        }
        reader.Skip(4);
        uint32_t dataSize = reader.U32();
        // the size the file carries is the whole section: the tag, the
        // size and the bytes
        if(dataSize < 8 || dataSize - 8 > reader.size) {
            result.failure = "the constants section is too large";
            return result;
        }
        result.Data.assign(bytes.begin() + (long)reader.position,
                           bytes.begin() + (long)(reader.position + dataSize - 8));
        reader.Skip(dataSize - 8);

        // the contexts
        if(!reader.Tag("OBJS")) {
            result.failure = "the objects section is missing";
            return result;
        }
        reader.Skip(4);
        reader.U32(); // the section's size
        result.TopLevel = (int32_t)reader.U32();
        uint32_t count = reader.U32();
        for(uint32_t index = 0; index < count; index++) {
            if(!reader.Tag("TJS2")) {
                result.failure = "a context record is missing";
                return result;
            }
            reader.Skip(4);
            uint32_t size = reader.U32();
            ExportContext context;
            for(int field = 0; field < 12; field++)
                context.Fields.push_back((int32_t)reader.U32());
            context.SourceCount = (int32_t)reader.U32();
            for(int32_t at = 0; at < context.SourceCount; at++)
                context.SourceCodePos.push_back((int32_t)reader.U32());
            for(int32_t at = 0; at < context.SourceCount; at++)
                context.SourceSrcPos.push_back((int32_t)reader.U32());
            context.CodeSize = (int32_t)reader.U32();
            for(int32_t at = 0; at < context.CodeSize; at++)
                context.Code.push_back(reader.U16());
            if((context.CodeSize % 2) == 1)
                reader.Skip(2); // the alignment the writer adds
            context.DataSize = (int32_t)reader.U32();
            for(int32_t at = 0; at < context.DataSize; at++) {
                reader.U16();
                context.Data.push_back(reader.U16());
            }
            context.ScgpCount = (int32_t)reader.U32();
            for(int32_t at = 0; at < context.ScgpCount; at++)
                context.Scgp.push_back((int32_t)reader.U32());
            context.PropertyCount = (int32_t)reader.U32();
            for(int32_t at = 0; at < context.PropertyCount; at++) {
                context.Properties.push_back(reader.U32());
                context.Properties.push_back(reader.U32());
            }
            result.Contexts.push_back(context);
        }

        // the source section
        if(!reader.Tag("SRCS")) {
            result.failure = "the source section is missing";
            return result;
        }
        reader.Skip(4);
        uint32_t sourceSize = reader.U32();
        if(sourceSize > reader.size) {
            result.failure = "the source section is too large";
            return result;
        }
        result.Source.assign(bytes.begin() + (long)reader.position,
                             bytes.begin() + (long)(reader.position + sourceSize));

        result.ok = reader.ok;
        return result;
    }

    //---------------------------------------------------------------------------
    // The export of a block, as bytes.
    //---------------------------------------------------------------------------
    std::vector<tjs_uint8> ExportBytes(tTJSScriptBlock *block,
                                         bool outputdebug) {
        tMemoryStream stream;
        block->ExportByteCode(outputdebug, &stream);
        return stream.Buffer;
    }

    //---------------------------------------------------------------------------
    // The export of both paths.
    //---------------------------------------------------------------------------
    bool ExportBoth(const uint16_t *text, uint32_t length,
                    std::vector<tjs_uint8> *cppBytes,
                    std::vector<tjs_uint8> *rustBytes, std::string *failure) {
        std::vector<tjs_char> script;
        script.reserve(length + 1);
        for(uint32_t index = 0; index < length; index++)
            script.push_back((tjs_char)text[index]);
        script.push_back(0);

        // the export fills the member list, so the flag is set for the
        // compile, as the reference's own export entry sets it
        try {
            tTJS *cppEngine = new tTJS();
            tTJSScriptBlock *cpp = new tTJSScriptBlock(cppEngine);
            cpp->SetSourceText(script.data());
            tTJSInterCodeContext::IsBytecodeCompile = true;
            try {
                cpp->ParseReference(script.data(), false, false);
            } catch(...) {
                tTJSInterCodeContext::IsBytecodeCompile = false;
                throw;
            }
            tTJSInterCodeContext::IsBytecodeCompile = false;
            *cppBytes = ExportBytes(cpp, true);
            cpp->Release();
            cppEngine->Release();

            tTJS *rustEngine = new tTJS();
            tTJSScriptBlock *rust = new tTJSScriptBlock(rustEngine);
            rust->SetSourceText(script.data());
            tTJSInterCodeContext::IsBytecodeCompile = true;
            try {
                rust->Parse(script.data(), false, false);
            } catch(...) {
                tTJSInterCodeContext::IsBytecodeCompile = false;
                throw;
            }
            tTJSInterCodeContext::IsBytecodeCompile = false;
            *rustBytes = ExportBytes(rust, true);
            rust->Release();
            rustEngine->Release();
            return true;
        } catch(eTJSScriptError &error) {
            *failure = std::string("script error: ") + error.what();
        } catch(eTJSError &error) {
            *failure = std::string("error: ") + error.what();
        } catch(...) {
            *failure = "an unknown failure";
        }
        return false;
    }

} // namespace

extern "C" int tjs2_proof_bytecode_compare(const uint16_t *text, uint32_t length,
                                            char *report, uint32_t reportSize) {
    EnsureLoggers();

    std::string result;
    std::vector<tjs_uint8> cppBytes;
    std::vector<tjs_uint8> rustBytes;
    std::string failure;

    if(!ExportBoth(text, length, &cppBytes, &rustBytes, &failure)) {
        result = "  the export failed: " + failure + "\n";
        if(report && reportSize) {
            uint32_t size = (uint32_t)result.size();
            if(size >= reportSize)
                size = reportSize - 1;
            memcpy(report, result.data(), size);
            report[size] = 0;
        }
        return -1;
    }

    Export cpp = ReadExport(cppBytes);
    Export rust = ReadExport(rustBytes);

    int differences = 0;
    if(!cpp.ok || !rust.ok) {
        result += "  the export could not be read: ";
        result += cpp.ok ? rust.failure : cpp.failure;
        result += "\n";
        differences = -1;
    } else {
        if(cpp.Data != rust.Data) {
            result += "  the constants section differs\n";
            differences++;
        }
        if(cpp.TopLevel != rust.TopLevel) {
            result += "  the top level differs: cpp=" +
                      std::to_string(cpp.TopLevel) +
                      " rust=" + std::to_string(rust.TopLevel) + "\n";
            differences++;
        }
        if(cpp.Contexts.size() != rust.Contexts.size()) {
            result += "  the context count differs: cpp=" +
                      std::to_string(cpp.Contexts.size()) +
                      " rust=" + std::to_string(rust.Contexts.size()) + "\n";
            differences++;
        }
        size_t count = cpp.Contexts.size() < rust.Contexts.size()
                           ? cpp.Contexts.size()
                           : rust.Contexts.size();
        for(size_t index = 0; index < count; index++) {
            const ExportContext &a = cpp.Contexts[index];
            const ExportContext &b = rust.Contexts[index];
            for(int field = 0; field < 12; field++) {
                if(a.Fields[field] != b.Fields[field]) {
                    result += "  context " + std::to_string(index) + " " +
                              FIELD_NAMES[field] + ": cpp=" +
                              std::to_string(a.Fields[field]) +
                              " rust=" + std::to_string(b.Fields[field]) + "\n";
                    differences++;
                }
            }
            if(a.SourceCount != b.SourceCount ||
               a.SourceCodePos != b.SourceCodePos ||
               a.SourceSrcPos != b.SourceSrcPos) {
                result += "  context " + std::to_string(index) +
                          " the source positions differ\n";
                differences++;
            }
            if(a.CodeSize != b.CodeSize || a.Code != b.Code) {
                result += "  context " + std::to_string(index) +
                          " the code differs\n";
                differences++;
            }
            if(a.DataSize != b.DataSize || a.Data != b.Data) {
                result += "  context " + std::to_string(index) +
                          " the constants differ\n";
                differences++;
            }
            if(a.ScgpCount != b.ScgpCount || a.Scgp != b.Scgp) {
                result += "  context " + std::to_string(index) +
                          " the super class getter's places differ\n";
                differences++;
            }
            if(a.PropertyCount != b.PropertyCount ||
               a.Properties != b.Properties) {
                result += "  context " + std::to_string(index) +
                          " the member list differs: cpp=" +
                          std::to_string(a.PropertyCount) +
                          " rust=" + std::to_string(b.PropertyCount) + "\n";
                differences++;
            }
        }
        if(differences == 0)
            result += "  the two exports are the same\n";
    }

    if(report && reportSize) {
        uint32_t size = (uint32_t)result.size();
        if(size >= reportSize)
            size = reportSize - 1;
        memcpy(report, result.data(), size);
        report[size] = 0;
    }

    return differences;
}

//---------------------------------------------------------------------------
// The line every code position resolves to.
//
// The source positions are the diagnostics: the line a runtime error
// reports is the line the code position resolves to. The two paths are
// walked over the same code positions and the lines are compared.
//---------------------------------------------------------------------------
extern "C" int tjs2_proof_lines(const uint16_t *text, uint32_t length,
                                char *report, uint32_t reportSize) {
    EnsureLoggers();

    std::string result;
    std::vector<tjs_char> script;
    script.reserve(length + 1);
    for(uint32_t index = 0; index < length; index++)
        script.push_back((tjs_char)text[index]);
    script.push_back(0);

    tTJS *cppEngine = new tTJS();
    tTJS *rustEngine = new tTJS();
    tTJSScriptBlock *cpp = nullptr;
    tTJSScriptBlock *rust = nullptr;
    std::string cppError;
    std::string rustError;

    try {
        cpp = new tTJSScriptBlock(cppEngine);
        cpp->SetSourceText(script.data());
        cpp->ParseReference(script.data(), false, false);
    } catch(eTJSScriptError &error) {
        cppError = error.what();
    } catch(eTJSError &error) {
        cppError = error.what();
    } catch(...) {
        cppError = "an unknown failure";
    }

    try {
        rust = new tTJSScriptBlock(rustEngine);
        rust->SetSourceText(script.data());
        rust->Parse(script.data(), false, false);
    } catch(eTJSScriptError &error) {
        rustError = error.what();
    } catch(eTJSError &error) {
        rustError = error.what();
    } catch(...) {
        rustError = "an unknown failure";
    }

    int contexts = 0;
    int positions = 0;
    int differences = 0;

    if(!cppError.empty() || !rustError.empty()) {
        result += "  the cpp path: " +
                  (cppError.empty() ? std::string("ok") : cppError) + "\n";
        result += "  the rust path: " +
                  (rustError.empty() ? std::string("ok") : rustError) + "\n";
        differences = -1;
    } else {
        const std::list<tTJSInterCodeContext *> &left = cpp->GetContextList();
        const std::list<tTJSInterCodeContext *> &right = rust->GetContextList();
        auto at = left.begin();
        auto other = right.begin();
        int index = 0;
        while(at != left.end() && other != right.end()) {
            tTJSInterCodeContext *a = *at;
            tTJSInterCodeContext *b = *other;
            ++at;
            ++other;
            contexts++;
            tjs_uint size = a->GetCodeSize();
            if(b->GetCodeSize() < size)
                size = b->GetCodeSize();
            for(tjs_uint position = 0; position < size; position++) {
                positions++;
                tjs_int left_line =
                    cpp->SrcPosToLine(a->CodePosToSrcPos((tjs_int)position));
                tjs_int right_line =
                    rust->SrcPosToLine(b->CodePosToSrcPos((tjs_int)position));
                if(left_line != right_line) {
                    differences++;
                    if(differences <= 20) {
                        result += "  context " + std::to_string(index) +
                                  " the code position " +
                                  std::to_string(position) +
                                  ": cpp=" + std::to_string(left_line) +
                                  " rust=" + std::to_string(right_line) + "\n";
                    }
                }
            }
            index++;
        }
        result = "  the contexts: " + std::to_string(contexts) +
                 ", the code positions: " + std::to_string(positions) +
                 ", the lines that differ: " + std::to_string(differences) +
                 "\n" + result;
    }

    if(report && reportSize) {
        uint32_t size = (uint32_t)result.size();
        if(size >= reportSize)
            size = reportSize - 1;
        memcpy(report, result.data(), size);
        report[size] = 0;
    }

    if(cpp)
        cpp->Release();
    if(rust)
        rust->Release();
    cppEngine->Release();
    rustEngine->Release();

    return differences;
}

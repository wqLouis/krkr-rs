//---------------------------------------------------------------------------
/*
        TJS2 Script Engine
        Copyright (C) 2000 W.Dee <dee@kikyou.info> and contributors

        See details of license at "license.txt"
*/
//---------------------------------------------------------------------------
// Script Block Management
//---------------------------------------------------------------------------
#include "tjsCommHead.h"

#include "tjsScriptBlock.h"
#include "tjsInterCodeGen.h"
#include "tjsConstArrayData.h"
#include "tjs.h"
#include "tjsArray.h"
#include "tjsDictionary.h"

#include "tjs2_codegen.h"

#include <cstring>

namespace TJS {
    //---------------------------------------------------------------------------
    // tTJSScriptBlock
    //---------------------------------------------------------------------------
    tTJSScriptBlock::tTJSScriptBlock(tTJS *owner) {
        RefCount = 1;
        Owner = owner;
        Owner->AddRef();

        Script = nullptr;
        Name = nullptr;

        InterCodeContext = nullptr;
        TopLevelContext = nullptr;
        LexicalAnalyzer = nullptr;

        UsingPreProcessor = false;

        LineOffset = 0;

        Owner->AddScriptBlock(this);
    }

    //---------------------------------------------------------------------------
    // for Bytecode
    tTJSScriptBlock::tTJSScriptBlock(tTJS *owner, const tjs_char *name,
                                     tjs_int lineoffset) {
        RefCount = 1;
        Owner = owner;
        Owner->AddRef();
        Name = nullptr;
        if(name) {
            Name = new tjs_char[TJS_strlen(name) + 1];
            TJS_strcpy(Name, name);
        }
        LineOffset = lineoffset;
        Script = nullptr;

        InterCodeContext = nullptr;
        TopLevelContext = nullptr;
        LexicalAnalyzer = nullptr;

        UsingPreProcessor = false;

        Owner->AddScriptBlock(this);
    }

    //---------------------------------------------------------------------------
    tTJSScriptBlock::~tTJSScriptBlock() {
        if(TopLevelContext)
            TopLevelContext->Release(), TopLevelContext = nullptr;
        while(!ContextStack.empty()) {
            ContextStack.top()->Release();
            ContextStack.pop();
        }

        Owner->RemoveScriptBlock(this);

        delete LexicalAnalyzer;
        delete[] Script;
        delete[] Name;

        Owner->Release();
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::AddRef() { RefCount++; }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::Release() {
        if(RefCount <= 1)
            delete this;
        else
            RefCount--;
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::SetName(const tjs_char *name, tjs_int lineofs) {
        if(Name)
            delete[] Name, Name = nullptr;
        if(name) {
            LineOffset = lineofs;
            Name = new tjs_char[TJS_strlen(name) + 1];
            TJS_strcpy(Name, name);
        }
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::Add(tTJSInterCodeContext *cntx) {
        InterCodeContextList.push_back(cntx);
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::Remove(tTJSInterCodeContext *cntx) {
        InterCodeContextList.remove(cntx);
    }

    //---------------------------------------------------------------------------
    tjs_uint tTJSScriptBlock::GetTotalVMCodeSize() const {
        tjs_uint size = 0;

        std::list<tTJSInterCodeContext *>::const_iterator i;
        for(i = InterCodeContextList.begin(); i != InterCodeContextList.end();
            i++) {
            size += (*i)->GetCodeSize();
        }
        return size;
    }

    //---------------------------------------------------------------------------
    tjs_uint tTJSScriptBlock::GetTotalVMDataSize() const {
        tjs_uint size = 0;

        std::list<tTJSInterCodeContext *>::const_iterator i;
        for(i = InterCodeContextList.begin(); i != InterCodeContextList.end();
            i++) {
            size += (*i)->GetDataSize();
        }
        return size;
    }

    //---------------------------------------------------------------------------
    const tjs_char *tTJSScriptBlock::GetLine(tjs_int line,
                                             tjs_int *linelength) const {
        if(Script == nullptr) {
            *linelength = 10;
            return TJS_W("Bytecode.");
        }
        // note that this function DOES matter LineOffset
        line -= LineOffset;
        if(linelength)
            *linelength = LineLengthVector[line];
        return Script + LineVector[line];
    }

    //---------------------------------------------------------------------------
    tjs_int tTJSScriptBlock::SrcPosToLine(tjs_int pos) const {
        tjs_uint s = 0;
        auto e = (tjs_uint)LineVector.size();
        while(true) {
            if(e - s <= 1)
                return s + LineOffset; // LineOffset is added
            tjs_uint m = s + (e - s) / 2;
            if(LineVector[m] > pos)
                e = m;
            else
                s = m;
        }
    }

    //---------------------------------------------------------------------------
    tjs_int tTJSScriptBlock::LineToSrcPos(tjs_int line) const {
        // assumes line is added by LineOffset
        line -= LineOffset;
        return LineVector[line];
    }

    //---------------------------------------------------------------------------
    ttstr tTJSScriptBlock::GetLineDescriptionString(tjs_int pos) const {
        // get short description, like "mainwindow.tjs(321)"
        // pos is in character count from the first of the script
        tjs_int line = SrcPosToLine(pos) + 1;
        ttstr name;
        if(Name) {
            name = Name;
        } else {
            ttstr ptr{ fmt::format("0x{:p}", static_cast<const void *>(this)) };
            name = ttstr(TJS_W("anonymous@")) + ptr;
        }

        return name + TJS_W("(") + ttstr(line) + TJS_W(")");
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::ConsoleOutput(const tjs_char *msg, void *data) {
        auto *blk = (tTJSScriptBlock *)data;
        blk->Owner->OutputToConsole(msg);
    }
    //---------------------------------------------------------------------------

    //---------------------------------------------------------------------------
    // Sets the script text of this block and builds its line table from it.
    //---------------------------------------------------------------------------
    void tTJSScriptBlock::SetSourceText(const tjs_char *text) {
        if(!text)
            return;

        delete[] Script;
        Script = nullptr;
        LineVector.clear();
        LineLengthVector.clear();

        Script = new tjs_char[TJS_strlen(text) + 1];
        TJS_strcpy(Script, text);

        // calculation of line-count
        tjs_char *ls = Script;
        tjs_char *p = Script;
        while(*p) {
            if(*p == TJS_W('\r') || *p == TJS_W('\n')) {
                LineVector.push_back(int(ls - Script));
                LineLengthVector.push_back(int(p - ls));
                if(*p == TJS_W('\r') && p[1] == TJS_W('\n'))
                    p++;
                p++;
                ls = p;
            } else {
                p++;
            }
        }

        if(p != ls) {
            LineVector.push_back(int(ls - Script));
            LineLengthVector.push_back(int(p - ls));
        }
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::SetText(tTJSVariant *result, const tjs_char *text,
                                  iTJSDispatch2 *context, bool isexpression) {

        // compiles text and executes its global level scripts.
        // the script will be compiled as an expression if isexpressn
        // is true.
        if(!text)
            return;
        if(!text[0])
            return;

        TJS_D((TJS_W("Counting lines ...\n")))

        SetSourceText(text);

        try {

            // parse and execute
#ifdef TJS_DEBUG_PROFILE_TIME
            {
                tTJSTimeProfiler p(parsetime);
#endif

                Parse(text, isexpression, result != nullptr);

#ifdef TJS_DEBUG_PROFILE_TIME
            }

            {
                char buf[256];
                TJS_nsprintf(buf, "parsing : %d", parsetime);
                OutputDebugString(buf);
                if(parsetime) {
                    TJS_nsprintf(buf, "Commit : %d (%d%%)", time_Commit,
                                 time_Commit * 100 / parsetime);
                    OutputDebugString(buf);
                    TJS_nsprintf(buf, "yylex : %d (%d%%)", time_yylex,
                                 time_yylex * 100 / parsetime);
                    OutputDebugString(buf);
                    TJS_nsprintf(buf, "MakeNP : %d (%d%%)", time_make_np,
                                 time_make_np * 100 / parsetime);
                    OutputDebugString(buf);
                    TJS_nsprintf(buf, "GenNodeCode : %d (%d%%)",
                                 time_GenNodeCode,
                                 time_GenNodeCode * 100 / parsetime);
                    OutputDebugString(buf);
                    TJS_nsprintf(buf, "  PutCode : %d (%d%%)", time_PutCode,
                                 time_PutCode * 100 / parsetime);
                    OutputDebugString(buf);
                    TJS_nsprintf(buf, "  PutData : %d (%d%%)", time_PutData,
                                 time_PutData * 100 / parsetime);
                    OutputDebugString(buf);
                    TJS_nsprintf(buf, "  this_proxy : %d (%d%%)",
                                 time_this_proxy,
                                 time_this_proxy * 100 / parsetime);
                    OutputDebugString(buf);

                    TJS_nsprintf(buf, "ns::Push : %d (%d%%)", time_ns_Push,
                                 time_ns_Push * 100 / parsetime);
                    OutputDebugString(buf);
                    TJS_nsprintf(buf, "ns::Pop : %d (%d%%)", time_ns_Pop,
                                 time_ns_Pop * 100 / parsetime);
                    OutputDebugString(buf);
                    TJS_nsprintf(buf, "ns::Find : %d (%d%%)", time_ns_Find,
                                 time_ns_Find * 100 / parsetime);
                    OutputDebugString(buf);
                    TJS_nsprintf(buf, "ns::Remove : %d (%d%%)", time_ns_Remove,
                                 time_ns_Remove * 100 / parsetime);
                    OutputDebugString(buf);
                    TJS_nsprintf(buf, "ns::Commit : %d (%d%%)", time_ns_Commit,
                                 time_ns_Commit * 100 / parsetime);
                    OutputDebugString(buf);
                }
            }
#endif

#ifdef TJS_DEBUG_DISASM
            std::list<tTJSInterCodeContext *>::iterator i =
                InterCodeContextList.begin();
            while(i != InterCodeContextList.end()) {
                ConsoleOutput(TJS_W(""), (void *)this);
                ConsoleOutput((*i)->GetName(), (void *)this);
                (*i)->Disassemble(ConsoleOutput, (void *)this);
                i++;
            }
#endif

            // execute global level script
            ExecuteTopLevelScript(result, context);
        } catch(...) {
            if(InterCodeContextList.size() != 1) {
                if(TopLevelContext)
                    TopLevelContext->Release(), TopLevelContext = nullptr;
                while(ContextStack.size()) {
                    ContextStack.top()->Release();
                    ContextStack.pop();
                }
            }
            throw;
        }

        if(InterCodeContextList.size() != 1) {
            // this is not a single-context script block
            // (may hook itself)
            // release all contexts and global at this time
            if(TopLevelContext)
                TopLevelContext->Release(), TopLevelContext = nullptr;
            while(ContextStack.size()) {
                ContextStack.top()->Release();
                ContextStack.pop();
            }
        }
    }

    //---------------------------------------------------------------------------
    // for Bytecode
    void tTJSScriptBlock::ExecuteTopLevel(tTJSVariant *result,
                                          iTJSDispatch2 *context) {
        try {
#ifdef TJS_DEBUG_DISASM
            std::list<tTJSInterCodeContext *>::iterator i =
                InterCodeContextList.begin();
            while(i != InterCodeContextList.end()) {
                ConsoleOutput(TJS_W(""), (void *)this);
                ConsoleOutput((*i)->GetName(), (void *)this);
                (*i)->Disassemble(ConsoleOutput, (void *)this);
                i++;
            }
#endif

            // execute global level script
            ExecuteTopLevelScript(result, context);
        } catch(...) {
            if(InterCodeContextList.size() != 1) {
                if(TopLevelContext)
                    TopLevelContext->Release(), TopLevelContext = nullptr;
                while(ContextStack.size()) {
                    ContextStack.top()->Release();
                    ContextStack.pop();
                }
            }
            throw;
        }

        if(InterCodeContextList.size() != 1) {
            // this is not a single-context script block
            // (may hook itself)
            // release all contexts and global at this time
            if(TopLevelContext)
                TopLevelContext->Release(), TopLevelContext = nullptr;
            while(ContextStack.size()) {
                ContextStack.top()->Release();
                ContextStack.pop();
            }
        }
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::ExecuteTopLevelScript(tTJSVariant *result,
                                                iTJSDispatch2 *context) {
        if(TopLevelContext) {
#ifdef TJS_DEBUG_PROFILE_TIME
            clock_t start = clock();
#endif
            TopLevelContext->FuncCall(0, nullptr, nullptr, result, 0, nullptr,
                                      context);
#ifdef TJS_DEBUG_PROFILE_TIME
            tjs_char str[100];
            TJS_sprintf(str, TJS_W("%d"), clock() - start);
            ConsoleOutput(str, (void *)this);
#endif
        }
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::PushContextStack(const tjs_char *name,
                                           tTJSContextType type) {
        tTJSInterCodeContext *cntx;
        cntx = new tTJSInterCodeContext(InterCodeContext, name, this, type);
        if(InterCodeContext == nullptr) {
            if(TopLevelContext)
                TJS_eTJSError(TJSInternalError);
            TopLevelContext = cntx;
            TopLevelContext->AddRef();
        }
        ContextStack.push(cntx);
        InterCodeContext = cntx;
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::PopContextStack() {
        InterCodeContext->Commit();
        InterCodeContext->Release();
        ContextStack.pop();
        if(!ContextStack.empty())
            InterCodeContext = ContextStack.top();
        else
            InterCodeContext = nullptr;
    }

    //---------------------------------------------------------------------------
    // The adapter.
    //
    // The image is the compiler's description of the block: the code
    // stream, the constant area, the source positions and the relations of
    // every context. The adapter copies it into the objects the executor
    // runs and resolves the two indirections the image cannot carry: the
    // constants that hold a context, and the members a context contributes
    // to its parent object.
    //
    // The model is the bytecode loader, which builds a block from an
    // external description in the same way: it copies the code and the
    // constants into the buffers the context frees, links the contexts to
    // each other, replays the member registrations on the parent objects
    // and resolves the constants that hold a context.
    //---------------------------------------------------------------------------
    namespace {
        //---------------------------------------------------------------------------
        // The text of a name or of a string, as the tree's own string.
        //
        // The image carries the code units and their number; the tree's
        // strings are null-terminated.
        //---------------------------------------------------------------------------
        std::vector<tjs_char> MakeText(const uint16_t *units, int32_t length) {
            std::vector<tjs_char> text;
            if(units != nullptr && length > 0) {
                text.reserve((size_t)length + 1);
                for(int32_t index = 0; index < length; index++)
                    text.push_back((tjs_char)units[index]);
            }
            text.push_back(0);
            return text;
        }

        //---------------------------------------------------------------------------
        // The constant inline objects of the block.
        //
        // The image's table of the constant objects is the block's, and
        // one constant object is one object: the reference builds it once,
        // at compile time, and every reference to that constant sees it.
        // The table holds the objects the adapter has built, one per
        // index of the image's table.
        //---------------------------------------------------------------------------
        class tConstantObjects {
        public:
            explicit tConstantObjects(size_t count)
                : Objects(count, nullptr) {}

            ~tConstantObjects() {
                for(iTJSDispatch2 *object : Objects)
                    if(object)
                        object->Release();
            }

            // The object of the given index, or null when it is not built
            // yet.
            iTJSDispatch2 *Get(size_t index) const { return Objects[index]; }

            // The block holds the object of the given index from here on.
            void Put(size_t index, iTJSDispatch2 *object) {
                Objects[index] = object;
            }

        private:
            std::vector<iTJSDispatch2 *> Objects;
        };

        // The constant inline array and the constant inline dictionary.
        //---------------------------------------------------------------------------
        tTJSVariant MakeConstantObject(const tjs2_codegen_value &value,
                                        const tjs2_codegen_block *image,
                                        tTJSScriptBlock *block,
                                        tConstantObjects &objects);

        //---------------------------------------------------------------------------
        // One constant of the image becomes one value of the constant area.
        //
        // The constant that holds a context is the one the image cannot
        // carry: it is resolved after every context exists, in the adapter's
        // last pass.
        //---------------------------------------------------------------------------
        tTJSVariant MakeConstant(const tjs2_codegen_value &value,
                                  const tjs2_codegen_block *image,
                                  tTJSScriptBlock *block,
                                  tConstantObjects &objects) {
            switch(value.kind) {
                case TJS2_CG_VALUE_VOID:
                    return tTJSVariant();
                case TJS2_CG_VALUE_INTEGER:
                    return tTJSVariant((tjs_int64)value.integer);
                case TJS2_CG_VALUE_REAL:
                    return tTJSVariant((tjs_real)value.real);
                case TJS2_CG_VALUE_STRING: {
                    std::vector<tjs_char> text =
                        MakeText(value.string, value.string_length);
                    return tTJSVariant(text.data());
                }
                case TJS2_CG_VALUE_OCTET:
                    return tTJSVariant(value.octet,
                                        (tjs_uint)value.octet_length);
                case TJS2_CG_VALUE_NULL:
                    return tTJSVariant((iTJSDispatch2 *)nullptr);
                case TJS2_CG_VALUE_OBJECT:
                    // the null object is the one the image names with the
                    // zero identity; an object the image cannot describe is
                    // a failure, not a guess
                    if(value.object == 0)
                        return tTJSVariant((iTJSDispatch2 *)nullptr);
                    TJS_eTJSScriptError(
                        TJS_W("The compiled script block holds an object it "
                              "cannot build"),
                        block, 0);
                    return tTJSVariant();
                case TJS2_CG_VALUE_CONTEXT:
                    // the context is resolved in the adapter's last pass
                    return tTJSVariant();
                case TJS2_CG_VALUE_ARRAY:
                case TJS2_CG_VALUE_DICTIONARY:
                    return MakeConstantObject(value, image, block, objects);
                default:
                    break;
            }
            TJS_eTJSScriptError(
                TJS_W("The compiled script block holds a value of an unknown "
                      "kind"),
                block, 0);
            return tTJSVariant();
        }

        //---------------------------------------------------------------------------
        // The constant inline array and the constant inline dictionary.
        //
        // The grammar builds the object while it parses it: the array is
        // made and its elements are added one by one, and the dictionary is
        // made and its members are set one by one. The image carries the
        // kind, the elements and their number; the object is built here, the
        // way the grammar builds it.
        //---------------------------------------------------------------------------
        tTJSVariant MakeConstantObject(const tjs2_codegen_value &value,
                                        const tjs2_codegen_block *image,
                                        tTJSScriptBlock *block,
                                        tConstantObjects &objects) {
            // the object the constant names: the index is the constant's
            // identity
            if(value.object >= (uint64_t)image->object_count)
                TJS_eTJSScriptError(
                    TJS_W("The compiled script block names a constant object "
                          "that does not exist"),
                    block, 0);
            size_t index = (size_t)value.object;

            // the object of this index, built once.
            //
            // The image's table of the constant objects is the block's,
            // and one constant object is one object: the reference builds
            // it once, at compile time, and every reference to that
            // constant sees it. The object of an index is built the first
            // time the index is named, and every later naming of it is
            // that same object.
            iTJSDispatch2 *built = objects.Get(index);
            if(built != nullptr)
                return tTJSVariant(built, built);

            const tjs2_codegen_object &object = image->objects[index];
            if(value.kind == TJS2_CG_VALUE_ARRAY) {
                built = TJSCreateArrayObject();
            } else {
                built = TJSCreateDictionaryObject();
            }
            // the block holds the object from here on: a name of this
            // index that comes while the object is filled is that object
            objects.Put(index, built);

            if(value.kind == TJS2_CG_VALUE_ARRAY) {
                // the elements are added with the array's own "add"
                tTJSString add_name(TJS_W("add"));
                for(int32_t at = 0; at < object.element_count; at++) {
                    tTJSVariant item =
                        MakeConstant(object.values[at], image, block, objects);
                    tTJSVariant *args[1] = { &item };
                    tTJSVariant holder(built, built);
                    holder.AsObjectClosureNoAddRef().FuncCall(
                        0, add_name.c_str(), add_name.GetHint(), nullptr, 1, args,
                        nullptr);
                }
            } else {
                for(int32_t at = 0; at < object.element_count; at++) {
                    tTJSVariant item =
                        MakeConstant(object.values[at], image, block, objects);
                    tTJSString name(
                        MakeConstant(object.keys[at], image, block, objects));
                    tTJSVariant holder(built, built);
                    holder.AsObjectClosureNoAddRef().PropSet(
                        TJS_MEMBERENSURE, name.c_str(), name.GetHint(), &item,
                        nullptr);
                }
            }
            // the block holds the object, and the value holds it too
            return tTJSVariant(built, built);
        }

        //---------------------------------------------------------------------------
        // The compiled script: the handle owns the image, and it is released
        // when the adapter is done with it.
        //---------------------------------------------------------------------------
        class tTJSCompiledScript {
            tjs2_script *handle;

        public:
            explicit tTJSCompiledScript(tjs2_script *handle)
                : handle(handle) {}

            ~tTJSCompiledScript() {
                if(handle)
                    tjs2_script_free(handle);
            }

            tTJSCompiledScript(const tTJSCompiledScript &) = delete;
            tTJSCompiledScript &operator=(const tTJSCompiledScript &) = delete;

            [[nodiscard]] tjs2_script *get() const { return handle; }
        };
    } // namespace

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::BuildFromRustImage(const tjs_char *script,
                                              bool isexpr, bool resultneeded) {
        // The conditional-compilation values.
        //
        // The values live in the engine and they outlive a script: a value
        // one script sets is visible to the next one. The compile takes the
        // values in and gives them back, so the block's caller keeps them
        // across the compiles: the values the engine holds go into the
        // compile, and the values the compile leaves behind go back into the
        // engine.
        std::vector<std::pair<ttstr, tjs_int32>> engine_values;
        Owner->GetPPValues(&engine_values);

        std::vector<std::vector<tjs_char>> value_names;
        std::vector<tjs2_pp_value> values;
        value_names.reserve(engine_values.size());
        values.reserve(engine_values.size());
        for(auto &entry : engine_values) {
            const tjs_char *text = entry.first.c_str();
            std::vector<tjs_char> name = MakeText(
                reinterpret_cast<const uint16_t *>(text),
                (int32_t)TJS_strlen(text));
            value_names.push_back(name);
            values.push_back(tjs2_pp_value{
                reinterpret_cast<const uint16_t *>(value_names.back().data()),
                (int32_t)(name.size() - 1),
                (int64_t)entry.second });
        }
        tjs2_pp_values pp_values;
        pp_values.values = values.empty() ? nullptr : values.data();
        pp_values.count = (int32_t)values.size();

        // the compile
        tTJSCompiledScript compiled(tjs2_script_compile(
            reinterpret_cast<const uint16_t *>(script),
            (uint32_t)TJS_strlen(script), isexpr ? 1 : 0, resultneeded ? 1 : 0,
            &pp_values));
        tjs2_script *handle = compiled.get();
        if(handle == nullptr)
            TJS_eTJSError(TJSInternalError);

        // The values the compile left behind go back into the engine, so
        // that the next compile — of this script or of another one — reads
        // them.
        const tjs2_pp_values *left = tjs2_script_pp_values(handle);
        if(left != nullptr) {
            for(int32_t index = 0; index < left->count; index++) {
                std::vector<tjs_char> name =
                    MakeText(left->values[index].name,
                             left->values[index].name_length);
                Owner->SetPPValue(name.data(),
                                  (tjs_int32)left->values[index].integer);
            }
        }

        // the failure: the first failure and the number of the failures the
        // compiler recovered from are recorded, and the parse raises
        // afterwards, with the first failure's message and position
        if(tjs2_script_status(handle) != TJS2_SCRIPT_OK) {
            const char *message = tjs2_script_error_message(handle);
            const int32_t position = tjs2_script_error_position(handle);
            ttstr text(message != nullptr ? std::string(message)
                                          : std::string());
            SetFirstError(text.c_str(), position);
            CompileErrorCount = tjs2_script_error_count(handle);
            return;
        }

        // the conditional compilation: the block is not reusable when the
        // script used one
        if(tjs2_script_using_preprocessor(handle))
            NotifyUsingPreProcessor();

        // the image
        const tjs2_codegen_block *image = tjs2_script_block_image(handle);
        if(image == nullptr)
            TJS_eTJSScriptError(
                TJS_W("The compiled script block has no image"), this, 0);

        // the line table: the block's own table is the one its own text
        // gives, and the image's table must agree with it
        if(image->line_count != (int32_t)LineVector.size())
            TJS_eTJSScriptError(
                TJS_W("The compiled script block's line table does not match "
                      "the script's"),
                this, 0);
        for(int32_t index = 0; index < image->line_count; index++) {
            if(image->lines[index].position != LineVector[index] ||
               image->lines[index].length != LineLengthVector[index])
                TJS_eTJSScriptError(
                    TJS_W("The compiled script block's line table does not "
                          "match the script's"),
                    this, 0);
        }

        const int32_t count = image->context_count;
        // the constant inline objects of the block: one per index of the
        // image's table, built once and shared by every naming of them
        tConstantObjects constant_objects((size_t)image->object_count);
        std::vector<tTJSInterCodeContext *> contexts((size_t)count, nullptr);
        // the constant areas: the adapter fills them, and the contexts take
        // them over
        std::vector<tTJSVariant *> data_areas((size_t)count, nullptr);

        // the context the image names: the image's index is the identity
        // every relation is resolved against
        auto context_of = [&](int32_t index) -> tTJSInterCodeContext * {
            if(index < 0 || index >= count)
                TJS_eTJSScriptError(
                    TJS_W("The compiled script block names a context that "
                          "does not exist"),
                    this, 0);
            return contexts[(size_t)index];
        };

        // the contexts: their buffers are copies of the image's, and the
        // context frees them
        for(int32_t index = 0; index < count; index++) {
            const tjs2_codegen_context &c = image->contexts[index];

            // the code
            tjs_int32 *code = nullptr;
            if(c.code_size > 0) {
                code = (tjs_int32 *)TJS_malloc((size_t)c.code_size *
                                                sizeof(tjs_int32));
                if(code == nullptr)
                    TJS_eTJSScriptError(TJSInsufficientMem, this, 0);
                memcpy(code, c.code, (size_t)c.code_size * sizeof(tjs_int32));
            }

            // the constants
            //
            // the constant inline objects of the block are built once,
            // one per index of the image's table
            tTJSVariant *data = nullptr;
            if(c.constant_count > 0) {
                data = new tTJSVariant[c.constant_count];
                data_areas[(size_t)index] = data;
                for(int32_t position = 0; position < c.constant_count;
                    position++)
                    data[position] = MakeConstant(c.constants[position], image,
                                                 this, constant_objects);
            }

            // the source positions: the image's table is sorted by code
            // position, and the context is told so
            tTJSInterCodeContext::tSourcePos *positions = nullptr;
            if(c.label_count > 0) {
                positions = (tTJSInterCodeContext::tSourcePos *)TJS_malloc(
                    (size_t)c.label_count *
                    sizeof(tTJSInterCodeContext::tSourcePos));
                if(positions == nullptr)
                    TJS_eTJSScriptError(TJSInsufficientMem, this, 0);
                for(int32_t position = 0; position < c.label_count; position++) {
                    positions[position].CodePos = c.labels[position].code_pos;
                    positions[position].SourcePos =
                        c.labels[position].source_pos;
                }
            }

            // the places the super class is evaluated
            std::vector<tjs_int> super_class_getter_pointer;
            if(c.super_class_getter_pointer_count > 0) {
                super_class_getter_pointer.assign(
                    c.super_class_getter_pointer,
                    c.super_class_getter_pointer +
                        c.super_class_getter_pointer_count);
            }

            // the name
            std::vector<tjs_char> name = MakeText(c.name, c.name_length);

            contexts[(size_t)index] = new tTJSInterCodeContext(
                this, c.name != nullptr ? name.data() : nullptr,
                (tTJSContextType)c.kind, code, c.code_size, data,
                c.constant_count, c.variable_count, c.variable_reserve_count,
                c.max_frame_count, c.func_arg_count, c.func_unnamed_arg_base,
                c.func_collapse_base, true, positions, c.label_count,
                super_class_getter_pointer);
        }

        // the relations between the contexts
        for(int32_t index = 0; index < count; index++) {
            const tjs2_codegen_context &c = image->contexts[index];
            contexts[(size_t)index]->SetCodeObject(
                c.parent >= 0 ? context_of(c.parent) : nullptr,
                c.prop_setter >= 0 ? context_of(c.prop_setter) : nullptr,
                c.prop_getter >= 0 ? context_of(c.prop_getter) : nullptr,
                c.super_class_getter >= 0 ? context_of(c.super_class_getter)
                                          : nullptr);
        }

        // the members the contexts contribute to their parent object: the
        // registration the compiler makes on the parent's object.
        //
        // The image carries the list on the parent, the registration is
        // the child's: the child registers itself on the parent, and the
        // list the bytecode export writes is the child's. The order is the
        // image's, which is the order the compiler registered them in.
        for(int32_t index = 0; index < count; index++) {
            const tjs2_codegen_context &c = image->contexts[index];
            for(int32_t position = 0; position < c.property_count; position++) {
                const tjs2_codegen_property &property = c.properties[position];
                tTJSInterCodeContext *member = context_of(property.member);
                // the name the image gives and the name the member carries
                // must be the same: the registration uses the member's own
                std::vector<tjs_char> name =
                    MakeText(property.name, property.name_length);
                const tjs_char *member_name = member->GetName();
                if(member_name == nullptr ||
                   TJS_strcmp(member_name, name.data()) != 0)
                    TJS_eTJSScriptError(
                        TJS_W("The compiled script block names a member "
                              "differently from the member itself"),
                        this, 0);
                member->RegisterMember();
            }
        }

        // the constants that hold a context: the image names the context, and
        // the context's own object takes its place
        for(int32_t index = 0; index < count; index++) {
            const tjs2_codegen_context &c = image->contexts[index];
            if(data_areas[(size_t)index] == nullptr)
                continue;
            for(int32_t position = 0; position < c.constant_count; position++) {
                if(c.constants[position].kind != TJS2_CG_VALUE_CONTEXT)
                    continue;
                data_areas[(size_t)index][position] =
                    context_of((int32_t)c.constants[position].object);
            }
        }

        // the block's own state: the top level and the context list
        tTJSInterCodeContext *top = nullptr;
        if(image->top_level >= 0)
            top = context_of(image->top_level);
        std::vector<tTJSInterCodeContext *> created = contexts;
        SetObjects(top, contexts, count);

        // the compiler's own reference of a context is released once the
        // parent holds it: the parent keeps the context alive, and the
        // context releases the block when it dies. The super class proxy is
        // the one exception: the class that owns it releases it.
        for(int32_t index = 0; index < count; index++) {
            tTJSInterCodeContext *context = created[(size_t)index];
            if(context == top)
                continue;
            if(context->GetContextType() == ctSuperClassGetter)
                continue;
            context->Release();
        }
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::Parse(const tjs_char *script, bool isexpr,
                                bool resultneeded) {
        TJS_F_TRACE("tTJSScriptBlock::Parse");

        if(!script)
            return;

        CompileErrorCount = 0;

        BuildFromRustImage(script, isexpr, resultneeded);

        if(CompileErrorCount) {
            TJS_eTJSScriptError(FirstError, this, FirstErrorPos);
        }
    }

    //---------------------------------------------------------------------------
    // PROOF INSTRUMENT (not part of the shipped tree): the compiler's own
    // path, for the comparison with the adapter's.
    //---------------------------------------------------------------------------
    void tTJSScriptBlock::ParseReference(const tjs_char *script, bool isexpr,
                                         bool resultneeded) {
        if(!script)
            return;

        CompileErrorCount = 0;

        LexicalAnalyzer =
            new tTJSLexicalAnalyzer(this, script, isexpr, resultneeded);

        try {
            parser{ this }.parse();
        } catch(...) {
            delete LexicalAnalyzer;
            LexicalAnalyzer = nullptr;
            throw;
        }

        delete LexicalAnalyzer;
        LexicalAnalyzer = nullptr;

        if(CompileErrorCount) {
            TJS_eTJSScriptError(FirstError, this, FirstErrorPos);
        }
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::SetFirstError(const tjs_char *error, tjs_int pos) {
        if(CompileErrorCount == 0) {
            FirstError = error;
            FirstErrorPos = pos;
        }
    }

    //---------------------------------------------------------------------------
    ttstr tTJSScriptBlock::GetNameInfo() const {
        if(LineOffset == 0) {
            return { Name };
        }
        return ttstr(Name) + TJS_W("(line +") + ttstr(LineOffset) + TJS_W(")");
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::Dump() const {
        auto i = InterCodeContextList.begin();
        while(i != InterCodeContextList.end()) {
            ConsoleOutput(TJS_W(""), (void *)this);
            ttstr ptr{ fmt::format(" {}", static_cast<void *>((*i))) };
            ConsoleOutput((ttstr(TJS_W("(")) +
                           ttstr((*i)->GetContextTypeName()) + TJS_W(") ") +
                           ttstr((*i)->GetName()) + ptr)
                              .c_str(),
                          (void *)this);
            (*i)->Disassemble(ConsoleOutput, (void *)this);
            ++i;
        }
    }

    void tTJSScriptBlock::Dump(tTJSBinaryStream *stream) const {
        struct TmpTJSConsoleOutput : public iTJSConsoleOutput {
            tTJSBinaryStream *_stream;
            explicit TmpTJSConsoleOutput(tTJSBinaryStream *s) : _stream(s) {}

            void ExceptionPrint(const tjs_char *msg) override { Print(msg); }

            void Print(const tjs_char *msg) override {
                _stream->Write(msg, TJS_strlen(msg));
            }
        } output{ stream };
        auto i = InterCodeContextList.begin();
        while(i != InterCodeContextList.end()) {
            output.Print(TJS_W(""));
            ttstr ptr{ fmt::format(" {}", static_cast<void *>((*i))) };
            output.Print((ttstr(TJS_W("(")) +
                          ttstr((*i)->GetContextTypeName()) + TJS_W(") ") +
                          ttstr((*i)->GetName()) + ptr)
                             .c_str());
            (*i)->Disassemble([&](const tjs_char *msg,
                                  void *data) -> void { output.Print(msg); },
                              (void *)this);
            ++i;
        }
    }

    //---------------------------------------------------------------------------
    // for Bytecode
    tjs_int
    tTJSScriptBlock::GetCodeIndex(const tTJSInterCodeContext *ctx) const {
        tjs_int index = 0;
        for(auto i = InterCodeContextList.begin();
            i != InterCodeContextList.end(); ++i, index++) {
            if((*i) == ctx) {
                return index;
            }
        }
        return -1;
    }

    //---------------------------------------------------------------------------
    const tjs_uint8
        tTJSScriptBlock::BYTECODE_FILE_TAG[BYTECODE_FILE_TAG_SIZE] = {
            'T', 'J', 'S', '2', '1', '0', '0', 0
        };
    const tjs_uint8 tTJSScriptBlock::BYTECODE_CODE_TAG[BYTECODE_TAG_SIZE] = {
        'T', 'J', 'S', '2'
    };
    const tjs_uint8 tTJSScriptBlock::BYTECODE_OBJ_TAG[BYTECODE_TAG_SIZE] = {
        'O', 'B', 'J', 'S'
    };
    const tjs_uint8 tTJSScriptBlock::BYTECODE_DATA_TAG[BYTECODE_TAG_SIZE] = {
        'D', 'A', 'T', 'A'
    };
    const tjs_uint8 tTJSScriptBlock::BYTECODE_SRC_TAG[BYTECODE_TAG_SIZE] = {
        'S', 'R', 'C', 'S'
    };

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::ExportByteCode(bool outputdebug,
                                         tTJSBinaryStream *output) {
        const int count = (int)InterCodeContextList.size();
        std::vector<std::vector<tjs_uint8> *> objarray;
        objarray.reserve(count * 2);
        auto *constarray = new tjsConstArrayData();
        int objsize = 0;
        for(auto obj : InterCodeContextList) {
            std::vector<tjs_uint8> *buf =
                obj->ExportByteCode(outputdebug, this, *constarray);
            objarray.push_back(buf);
            objsize += (int)buf->size() + BYTECODE_TAG_SIZE +
                BYTECODE_CHUNK_SIZE_LEN; // tag + size
        }

        objsize += BYTECODE_TAG_SIZE + BYTECODE_CHUNK_SIZE_LEN + 4 +
            4; // OBJS tag + size + toplevel + count
        std::vector<tjs_uint8> *dataarea = constarray->ExportBuffer();
        int datasize = (int)dataarea->size() + BYTECODE_TAG_SIZE +
            BYTECODE_CHUNK_SIZE_LEN; // DATA tag + size

        // The source section: the text this block was compiled from, and the
        // source positions of its contexts when the objects section does not
        // carry them. A block that is read back from it reports the same
        // lines as the source it was compiled from.
        const int sourceCharCount = Script ? (int)TJS_strlen(Script) : 0;
        std::vector<tjs_uint8> sourcebuf;
        sourcebuf.reserve(sourceCharCount * 2 + 64);
        auto append4byte = [&sourcebuf](int value) {
            sourcebuf.push_back((tjs_uint8)((value >> 0) & 0xff));
            sourcebuf.push_back((tjs_uint8)((value >> 8) & 0xff));
            sourcebuf.push_back((tjs_uint8)((value >> 16) & 0xff));
            sourcebuf.push_back((tjs_uint8)((value >> 24) & 0xff));
        };
        // the flags: bit 0 tells that the source positions follow the text
        append4byte(outputdebug ? 0 : 1);
        append4byte(sourceCharCount);
        for(int i = 0; i < sourceCharCount; i++) {
            const tjs_uint16 c = (tjs_uint16)Script[i];
            sourcebuf.push_back((tjs_uint8)((c >> 0) & 0xff));
            sourcebuf.push_back((tjs_uint8)((c >> 8) & 0xff));
        }
        // The source positions: the objects section carries them when the
        // debug information was asked for, and this section carries them
        // otherwise. They are written once, never twice.
        if(!outputdebug) {
            append4byte(count);
            for(auto obj : InterCodeContextList) {
                const tTJSInterCodeContext::tSourcePos *positions =
                    obj->GetSourcePosArray();
                const tjs_int positionCount = obj->GetSourcePosArraySize();
                append4byte((int)positionCount);
                for(tjs_int i = 0; i < positionCount; i++) {
                    append4byte((int)positions[i].CodePos);
                }
                for(tjs_int i = 0; i < positionCount; i++) {
                    append4byte((int)positions[i].SourcePos);
                }
            }
        }
        const int sourcePayloadSize = (int)sourcebuf.size();
        const int sourcesize =
            BYTECODE_TAG_SIZE + BYTECODE_CHUNK_SIZE_LEN + sourcePayloadSize;

        int filesize = objsize + datasize + sourcesize + BYTECODE_FILE_TAG_SIZE +
            BYTECODE_CHUNK_SIZE_LEN; // TJS2 tag + file size
        int toplevel = -1;
        if(TopLevelContext != nullptr) {
            toplevel = GetCodeIndex(TopLevelContext);
        }
        tjs_uint8 tmp[4];
        output->Write(BYTECODE_FILE_TAG, 8);
        Write4Byte(tmp, filesize);
        output->Write(tmp, 4);
        output->Write(BYTECODE_DATA_TAG, 4);
        Write4Byte(tmp, datasize);
        output->Write(tmp, 4);
        output->Write(&((*dataarea)[0]), (tjs_uint)dataarea->size());
        output->Write(BYTECODE_OBJ_TAG, 4);
        Write4Byte(tmp, objsize);
        output->Write(tmp, 4);
        Write4Byte(tmp, toplevel);
        output->Write(tmp, 4);
        Write4Byte(tmp, count);
        output->Write(tmp, 4);
        for(int i = 0; i < count; i++) {
            std::vector<tjs_uint8> *buf = objarray[i];
            int size = (int)buf->size();
            output->Write(BYTECODE_CODE_TAG, 4);
            Write4Byte(tmp, size);
            output->Write(tmp, 4);
            output->Write(&((*buf)[0]), size);
            delete buf;
            objarray[i] = nullptr;
        }
        objarray.clear();

        // the source section
        output->Write(BYTECODE_SRC_TAG, BYTECODE_TAG_SIZE);
        Write4Byte(tmp, sourcePayloadSize);
        output->Write(tmp, 4);
        output->Write(&sourcebuf[0], (tjs_uint)sourcebuf.size());

        delete constarray;
        delete dataarea;
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::Compile(const tjs_char *text, bool isexpression,
                                  bool isresultneeded, bool outputdebug,
                                  tTJSBinaryStream *output) {
        if(!text)
            return;
        if(!text[0])
            return;

        tTJSInterCodeContext::IsBytecodeCompile = true;
        try {
            SetSourceText(text);

            Parse(text, isexpression, isresultneeded);

            ExportByteCode(outputdebug, output);
        } catch(...) {
            if(InterCodeContextList.size() != 1) {
                if(TopLevelContext)
                    TopLevelContext->Release(), TopLevelContext = nullptr;
                while(!ContextStack.empty()) {
                    ContextStack.top()->Release();
                    ContextStack.pop();
                }
            }
            tTJSInterCodeContext::IsBytecodeCompile = false;
            throw;
        }
        if(InterCodeContextList.size() != 1) {
            if(TopLevelContext)
                TopLevelContext->Release(), TopLevelContext = nullptr;
            while(!ContextStack.empty()) {
                ContextStack.top()->Release();
                ContextStack.pop();
            }
        }
        tTJSInterCodeContext::IsBytecodeCompile = false;
    }

    //---------------------------------------------------------------------------
    void tTJSScriptBlock::TranslateCodeAddress(tjs_int32 *code,
                                               const tjs_int32 codeSize) {
        // The bytecode writer turns the VM addresses of the code into the
        // indices the bytecode file carries. The layout of every opcode is
        // the one table the loader reads the file back with, so an
        // instruction the writer did not understand is a hard error instead
        // of a silently skipped operand.
        tjs_int i = 0;
        while(i < codeSize) {
            bool broken = false;
            const tjs_int size = TranslateVMInstruction(
                code, codeSize, i, tTJSVMAddrDir::ToIndex, &broken);
            if(broken) {
                TJS_eTJSScriptError(TJSInternalError, this, 0);
            }
            i += size;
        }

        if(codeSize != i) {
            TJS_eTJSScriptError(TJSInternalError, this, 0);
        }
    }

} // namespace TJS

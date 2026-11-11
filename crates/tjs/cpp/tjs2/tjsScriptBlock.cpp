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
    void tTJSScriptBlock::Parse(const tjs_char *script, bool isexpr,
                                bool resultneeded) {
        TJS_F_TRACE("tTJSScriptBlock::Parse");

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

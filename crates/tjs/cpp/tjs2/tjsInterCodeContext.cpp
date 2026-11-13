//---------------------------------------------------------------------------
/*
        TJS2 Script Engine
        Copyright (C) 2000 W.Dee <dee@kikyou.info> and contributors

        See details of license at "license.txt"
*/
//---------------------------------------------------------------------------
// Intermediate Code Context
//---------------------------------------------------------------------------
#include "tjsCommHead.h"

#include "tjsInterCodeContext.h"
#include "tjsScriptBlock.h"

#include "tjsError.h"
#include "tjsUtils.h"
#include "tjsDebug.h"
#include "tjsConstArrayData.h"

namespace TJS {
    //---------------------------------------------------------------------------
    static tjs_int TJSGetContextHashSize(tTJSContextType type) {
        switch(type) {
            case ctTopLevel:
                return 0;
            case ctFunction:
                return 1;
            case ctExprFunction:
                return 1;
            case ctProperty:
                return 1;
            case ctPropertySetter:
                return 0;
            case ctPropertyGetter:
                return 0;
            case ctClass:
                return TJS_NAMESPACE_DEFAULT_HASH_BITS;
            case ctSuperClassGetter:
                return 0;
            default:
                return TJS_NAMESPACE_DEFAULT_HASH_BITS;
        }
    }

    //---------------------------------------------------------------------------
    // is bytecode export
    bool tTJSInterCodeContext::IsBytecodeCompile = false;

    //---------------------------------------------------------------------------
    // for Byte code
    tTJSInterCodeContext::tTJSInterCodeContext(
        tTJSScriptBlock *block, const tjs_char *name, tTJSContextType type,
        tjs_int32 *code, tjs_int codeSize, tTJSVariant *data, tjs_int dataSize,
        tjs_int varcount, tjs_int verrescount, tjs_int maxframe,
        tjs_int argcount, tjs_int arraybase, tjs_int colbase, bool srcsorted,
        tSourcePos *srcPos, tjs_int srcPosSize,
        std::vector<tjs_int> &superpointer) :
        inherited(TJSGetContextHashSize(type)), Properties(nullptr) {
        inherited::CallFinalize = false;
        Parent = nullptr;
        PropGetter = PropSetter = SuperClassGetter = nullptr;

        CodeArea = code;
        CodeAreaSize = codeSize;

        DataArea = data;
        DataAreaSize = dataSize;

        // copy
        size_t size = superpointer.size();
        SuperClassGetterPointer.reserve(size);
        for(size_t i = 0; i < size; i++) {
            SuperClassGetterPointer.push_back(superpointer[i]);
        }

        MaxFrameCount = maxframe;
        MaxVariableCount = varcount;
        VariableReserveCount = verrescount; //

        FuncDeclArgCount = argcount;
        FuncDeclUnnamedArgArrayBase = arraybase;
        FuncDeclCollapseBase = colbase;

        SourcePosArraySorted = true;
        SourcePosArray = srcPos;
        SourcePosArraySize = srcPosSize;

#ifdef _DEBUG
        DebuggerRegisterArea = nullptr;
#endif // _DEBUG

        if(name) {
            Name = new tjs_char[TJS_strlen(name) + 1];
            TJS_strcpy(Name, name);
        } else {
            Name = nullptr;
        }

        try {
            ContextType = type;
            Block = block;
            TJSVariantArrayStack = block->GetTJS()->GetVariantArrayStack();
        } catch(...) {
            delete[] Name;
            throw;
        }
    }

    //---------------------------------------------------------------------------
    tTJSInterCodeContext::~tTJSInterCodeContext() {
        if(Name)
            delete[] Name;
        Name = nullptr;
    }

    //---------------------------------------------------------------------------
    void tTJSInterCodeContext::Finalize() {
        if(PropSetter)
            PropSetter->Release(), PropSetter = nullptr;
        if(PropGetter)
            PropGetter->Release(), PropGetter = nullptr;
        if(SuperClassGetter)
            SuperClassGetter->Release(), SuperClassGetter = nullptr;

        if(CodeArea)
            TJS_free(CodeArea), CodeArea = nullptr;
        if(DataArea) {
            delete[] DataArea;
            DataArea = nullptr;
        }

        Block->Remove(this);

        if(ContextType != ctTopLevel && Block)
            Block->Release();

        if(SourcePosArray)
            TJS_free(SourcePosArray), SourcePosArray = nullptr;

#ifdef _DEBUG
        if(Parent) {
            Parent->Release();
            Parent = nullptr;
        }
#endif // _DEBUG
        inherited::Finalize();
    }

    //---------------------------------------------------------------------------
    const tjs_char *tTJSInterCodeContext::GetContextTypeName() const {
        switch(ContextType) {
            case ctTopLevel:
                return TJS_W("top level script");
            case ctFunction:
                return TJS_W("function");
            case ctExprFunction:
                return TJS_W("function expression");
            case ctProperty:
                return TJS_W("property");
            case ctPropertySetter:
                return TJS_W("property setter");
            case ctPropertyGetter:
                return TJS_W("property getter");
            case ctClass:
                return TJS_W("class");
            case ctSuperClassGetter:
                return TJS_W("super class getter proxy");
            default:
                return TJS_W("unknown");
        }
    }

    //---------------------------------------------------------------------------
    ttstr tTJSInterCodeContext::GetShortDescription() const {
        ttstr ret(TJS_W("(") + ttstr(GetContextTypeName()) + TJS_W(")"));

        const tjs_char *name;
        if(ContextType == ctPropertySetter || ContextType == ctPropertyGetter) {
            if(Parent)
                name = Parent->Name;
            else
                name = nullptr;
        } else {
            name = Name;
        }

        if(name)
            ret += TJS_W(" ") + ttstr(name);

        return ret;
    }

    //---------------------------------------------------------------------------
    int TJS_USERENTRY tTJSInterCodeContext::tSourcePos::SortFunction(
        const void *a, const void *b) {
        const tSourcePos *aa = (const tSourcePos *)a;
        const tSourcePos *bb = (const tSourcePos *)b;
        return aa->CodePos - bb->CodePos;
    }

    //---------------------------------------------------------------------------
    void tTJSInterCodeContext::SortSourcePos() {
        if(!SourcePosArraySorted) {
            qsort(SourcePosArray, SourcePosArraySize, sizeof(tSourcePos),
                  tSourcePos::SortFunction);
            SourcePosArraySorted = true;
        }
    }


    //---------------------------------------------------------------------------
    void tTJSInterCodeContext::RegisterMember() {
        // The registration of this context as a member of its parent: the
        // list the bytecode export writes, and the member on the parent's
        // object.
        //
        // The reference's RegisterFunction does this at the end of its
        // work; the parts that generate code are the image's, so this is
        // the part that touches the objects.
        if(Parent->ContextType == ctFunction ||
           Parent->ContextType == ctClass) {
            if(IsBytecodeCompile) { // for bytecode export
                if(Properties == nullptr)
                    Properties = new std::vector<tProperty *>();
                Properties->push_back(new tProperty(Name, this));
            }

            // register members to the parent object
            tTJSVariant val = this;
            Parent->PropSet(TJS_MEMBERENSURE | TJS_IGNOREPROP, Name, nullptr,
                            &val, Parent);
        }
    }

    //---------------------------------------------------------------------------
    tjs_int tTJSInterCodeContext::CodePosToSrcPos(tjs_int codepos) const {
        // converts from
        // CodeArea oriented position to source oriented position
        if(!SourcePosArray)
            return 0;

        const_cast<tTJSInterCodeContext *>(this)->SortSourcePos();

        tjs_uint s = 0;
        tjs_uint e = SourcePosArraySize;
        if(e == 0)
            return 0;
        while(true) {
            if(e - s <= 1)
                return SourcePosArray[s].SourcePos;
            tjs_uint m = s + (e - s) / 2;
            if(SourcePosArray[m].CodePos > codepos)
                e = m;
            else
                s = m;
        }
    }

    //---------------------------------------------------------------------------
    tjs_int
    tTJSInterCodeContext::FindSrcLineStartCodePos(tjs_int codepos) const {
        // find code address which is the first instruction of the
        // source line
        if(!SourcePosArray)
            return 0;

        tjs_int srcpos = CodePosToSrcPos(codepos);
        tjs_int line = Block->SrcPosToLine(srcpos);
        srcpos = Block->LineToSrcPos(line);

        tjs_int codeposmin = -1;
        for(tjs_int i = 0; i < SourcePosArraySize; i++) {
            if(SourcePosArray[i].SourcePos >= srcpos) {
                if(codeposmin == -1 || SourcePosArray[i].CodePos < codeposmin)
                    codeposmin = SourcePosArray[i].CodePos;
            }
        }
        if(codeposmin < 0)
            codeposmin = 0;
        return codeposmin;
    }

    //---------------------------------------------------------------------------
    ttstr
    tTJSInterCodeContext::GetPositionDescriptionString(tjs_int codepos) const {
        return Block->GetLineDescriptionString(CodePosToSrcPos(codepos)) +
            TJS_W("[") + GetShortDescription() + TJS_W("]");
    }

    //---------------------------------------------------------------------------
    const tjs_char *tTJSInterCodeContext::GetName() const { return Name; }

    //---------------------------------------------------------------------------

    /**
     * バイトコードを出力する
     * @return
     */
    std::vector<tjs_uint8> *
    tTJSInterCodeContext::ExportByteCode(bool outputdebug,
                                         tTJSScriptBlock *block,
                                         tjsConstArrayData &constarray) const {
        int parent = -1;
        if(Parent != nullptr) {
            parent = block->GetCodeIndex(Parent);
        }
        int propSetter = -1;
        if(PropSetter != nullptr) {
            propSetter = block->GetCodeIndex(PropSetter);
        }
        int propGetter = -1;
        if(PropGetter != nullptr) {
            propGetter = block->GetCodeIndex(PropGetter);
        }
        int superClassGetter = -1;
        if(SuperClassGetter != nullptr) {
            superClassGetter = block->GetCodeIndex(SuperClassGetter);
        }
        int name = -1;
        if(Name != nullptr) {
            name = constarray.PutString(Name);
        }
        // 13 * 4 データ部分のサイズ
        int count = 0;
        if(outputdebug) {
            count = SourcePosArraySize;
        }
        int srcpossize = count * 8;
        int codesize =
            (CodeAreaSize % 2) == 1 ? CodeAreaSize * 2 + 2 : CodeAreaSize * 2;
        int datasize = DataAreaSize * 4;
        int scgpsize = (int)(SuperClassGetterPointer.size() * 4);
        int propsize =
            (int)((Properties != nullptr ? Properties->size() * 8 : 0) + 4);
        int size = 12 * 4 + srcpossize + codesize + datasize + scgpsize +
            propsize + 4 * 4;
        std::vector<tjs_uint8> *result = new std::vector<tjs_uint8>();
        result->reserve(size);

        Add4ByteToVector(result, parent);
        Add4ByteToVector(result, name);
        Add4ByteToVector(result, ContextType);
        Add4ByteToVector(result, MaxVariableCount);
        Add4ByteToVector(result, VariableReserveCount);
        Add4ByteToVector(result, MaxFrameCount);
        Add4ByteToVector(result, FuncDeclArgCount);
        Add4ByteToVector(result, FuncDeclUnnamedArgArrayBase);
        Add4ByteToVector(result, FuncDeclCollapseBase);
        Add4ByteToVector(result, propSetter);
        Add4ByteToVector(result, propGetter);
        Add4ByteToVector(result, superClassGetter);

        Add4ByteToVector(result, count);
        if(outputdebug) {
            for(int i = 0; i < count; i++) {
                Add4ByteToVector(result, SourcePosArray[i].CodePos);
            }
            for(int i = 0; i < count; i++) {
                Add4ByteToVector(result, SourcePosArray[i].SourcePos);
            }
        }

        count = CodeAreaSize;
        Add4ByteToVector(result, count);

        block->TranslateCodeAddress(CodeArea, CodeAreaSize);
        for(int i = 0; i < CodeAreaSize; i++) {
            Add2ByteToVector(result, CodeArea[i]);
        }
        if((count % 2) == 1) { // alignment
            Add2ByteToVector(result, 0);
        }

        count = DataAreaSize;
        Add4ByteToVector(result, count);
        for(int i = 0; i < count; i++) {
            tjs_int16 type = constarray.GetType(DataArea[i], block);
            tjs_int16 v = (tjs_int16)constarray.PutVariant(DataArea[i], block);
            Add2ByteToVector(result, type);
            Add2ByteToVector(result, v);
        }
        count = (int)SuperClassGetterPointer.size();
        Add4ByteToVector(result, count);
        for(int i = 0; i < count; i++) {
            int v = SuperClassGetterPointer.at(i);
            Add4ByteToVector(result, v);
        }
        count = 0;
        if(Properties != nullptr) {
            count = (int)Properties->size();
            Add4ByteToVector(result, count);
            if(count > 0) {
                for(int i = 0; i < count; i++) {
                    tProperty *prop = (*Properties).at(i);
                    int propname = constarray.PutString(prop->Name);
                    int propobj = -1;
                    if(prop->Value != nullptr) {
                        propobj = block->GetCodeIndex(prop->Value);
                    }
                    Add4ByteToVector(result, propname);
                    Add4ByteToVector(result, propobj);
                    delete prop;
                }
            }
            delete Properties;
        } else {
            Add4ByteToVector(result, count);
        }
        return result;
    }
    //---------------------------------------------------------------------------
} // namespace TJS

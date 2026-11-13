//---------------------------------------------------------------------------
/*
        TJS2 Script Engine
        Copyright (C) 2000 W.Dee <dee@kikyou.info> and contributors

        See details of license at "license.txt"
*/
//---------------------------------------------------------------------------
// Intermediate Code Context
//---------------------------------------------------------------------------
#ifndef tjsInterCodeContextH
#define tjsInterCodeContextH

#include <functional>
#include <vector>
#include "tjsVariant.h"
#include "tjsInterface.h"
#include "tjsVMCodes.h"
#include "tjsError.h"
#include "tjsObject.h"

#ifdef _DEBUG
#include "tjsDebug.h"
#endif // _DEBUG

namespace TJS {
    class tTJSScriptBlock;
    class tTJSVariantArrayStack;

    //---------------------------------------------------------------------------
    enum tTJSContextType {
        ctTopLevel,
        ctFunction,
        ctExprFunction,
        ctProperty,
        ctPropertySetter,
        ctPropertyGetter,
        ctClass,
        ctSuperClassGetter,
    };

    //---------------------------------------------------------------------------
    //---------------------------------------------------------------------------
    // tTJSInterCodeContext
    //---------------------------------------------------------------------------
    /*
            this class implements iTJSDispatch2;
            the object can be directly treated as function, class,
       property handlers.
    */

    class tTJSInterCodeContext : public tTJSCustomObject {
        typedef tTJSCustomObject inherited;

    public:
        // is bytecode export
        static bool IsBytecodeCompile;

    protected:
        void Finalize() override;

    public:
        struct tSourcePos {
            tjs_int CodePos;
            tjs_int SourcePos;

            static int TJS_USERENTRY SortFunction(const void *a, const void *b);
        };

    private:
        // for Bytecode
        struct tProperty {
            const tjs_char *Name;
            const tTJSInterCodeContext *Value;

            tProperty(const tjs_char *name, const tTJSInterCodeContext *val) {
                Name = name;
                Value = val;
            }
        };

        std::vector<tProperty *> *Properties;

        tjs_int32 *CodeArea;
        tjs_int CodeAreaSize;

        tTJSVariant *DataArea;
        tjs_int DataAreaSize;

        bool SourcePosArraySorted;
        tSourcePos *SourcePosArray;
        tjs_int SourcePosArraySize;

        tjs_int MaxFrameCount;
        tjs_int MaxVariableCount;

        tjs_int VariableReserveCount;

        tjs_int FuncDeclArgCount;
        tjs_int FuncDeclUnnamedArgArrayBase;
        tjs_int FuncDeclCollapseBase;

        std::vector<tjs_int> SuperClassGetterPointer;

        tjs_char *Name;
        tTJSInterCodeContext *Parent;
        tTJSScriptBlock *Block;
        tTJSContextType ContextType;
        tTJSInterCodeContext *PropSetter;
        tTJSInterCodeContext *PropGetter;
        tTJSInterCodeContext *SuperClassGetter;

#ifdef _DEBUG
        ScopeKey DebuggerScopeKey; //!< for exec
        tTJSVariant *DebuggerRegisterArea; //!< for exec
#endif // _DEBUG

    public:
        tTJSContextType GetContextType() const { return ContextType; }

        const tjs_char *GetContextTypeName() const;

        ttstr GetShortDescription() const;

        tTJSScriptBlock *GetBlock() const { return Block; }

        tjs_uint GetCodeSize() const { return CodeAreaSize; }

        tjs_uint GetDataSize() const { return DataAreaSize; }

        tjs_int CodePosToSrcPos(tjs_int codepos) const;

        tjs_int FindSrcLineStartCodePos(tjs_int codepos) const;

        ttstr GetPositionDescriptionString(tjs_int codepos) const;

        const tjs_char *GetName() const;

        //----------------------------------------------------------
        // disassembler
        // implemented in tjsDisassemble.cpp

        static tTJSString GetValueComment(const tTJSVariant &val);

        void Disassemble(
            void (*output_func)(const tjs_char *msg, const tjs_char *comment,
                                tjs_int addr, const tjs_int32 *codestart,
                                tjs_int size, void *data),
            void (*output_func_src)(const tjs_char *msg, const tjs_char *name,
                                    tjs_int line, void *data),
            void *data, tjs_int start = 0, tjs_int end = 0);

        void Disassemble(
            std::function<void(const tjs_char *msg, void *data)> output_func,
            void *data, tjs_int start = 0, tjs_int end = 0);

        void Disassemble(tjs_int start = 0, tjs_int end = 0);

        void DisassembleSrcLine(tjs_int codepos);

        //---------------------------------------------------------
        // execute stuff
        // implemented in InterCodeExec.cpp
    private:
        void DisplayExceptionGeneratedCode(tjs_int codepos,
                                           const tTJSVariant *ra);

        void ThrowScriptException(tTJSVariant &val, tTJSScriptBlock *block,
                                  tjs_int srcpos);

        //	void ExecuteAsGlobal(tTJSVariant *result);
        void ExecuteAsFunction(iTJSDispatch2 *objthis, tTJSVariant **args,
                               tjs_int numargs, tTJSVariant *result,
                               tjs_int start_ip);

        tjs_int ExecuteCode(tTJSVariant *ra, tjs_int startip,
                            tTJSVariant **args, tjs_int numargs,
                            tTJSVariant *result, bool tryCatch = false);

        tjs_int ExecuteCodeInTryBlock(tTJSVariant *ra, tjs_int startip,
                                      tTJSVariant **args, tjs_int numargs,
                                      tTJSVariant *result, tjs_int catchip,
                                      tjs_int exobjreg);

        static void ContinuousClear(tTJSVariant *ra, const tjs_int32 *code);

        void GetPropertyDirect(tTJSVariant *ra, const tjs_int32 *code,
                               tjs_uint32 flags) const;

        void SetPropertyDirect(tTJSVariant *ra, const tjs_int32 *code,
                               tjs_uint32 flags) const;

        static void GetProperty(tTJSVariant *ra, const tjs_int32 *code);

        static void SetProperty(tTJSVariant *ra, const tjs_int32 *code);

        static void GetPropertyIndirect(tTJSVariant *ra, const tjs_int32 *code,
                                        tjs_uint32 flags);

        static void SetPropertyIndirect(tTJSVariant *ra, const tjs_int32 *code,
                                        tjs_uint32 flags);

        void OperatePropertyDirect(tTJSVariant *ra, const tjs_int32 *code,
                                   tjs_uint32 ope) const;

        static void OperatePropertyIndirect(tTJSVariant *ra,
                                            const tjs_int32 *code,
                                            tjs_uint32 ope);

        static void OperateProperty(tTJSVariant *ra, const tjs_int32 *code,
                                    tjs_uint32 ope);

        void OperatePropertyDirect0(tTJSVariant *ra, const tjs_int32 *code,
                                    tjs_uint32 ope) const;

        static void OperatePropertyIndirect0(tTJSVariant *ra,
                                             const tjs_int32 *code,
                                             tjs_uint32 ope);

        static void OperateProperty0(tTJSVariant *ra, const tjs_int32 *code,
                                     tjs_uint32 ope);

        void DeleteMemberDirect(tTJSVariant *ra, const tjs_int32 *code);

        static void DeleteMemberIndirect(tTJSVariant *ra,
                                         const tjs_int32 *code);

        void TypeOfMemberDirect(tTJSVariant *ra, const tjs_int32 *code,
                                tjs_uint32 flags) const;

        static void TypeOfMemberIndirect(tTJSVariant *ra, const tjs_int32 *code,
                                         tjs_uint32 flags);

        static void _output_func(const tjs_char *msg, const tjs_char *comment,
                                 tjs_int addr, const tjs_int32 *codestart,
                                 tjs_int size, void *data);

        static void _output_func_src(const tjs_char *msg, const tjs_char *name,
                                     tjs_int line, void *data);

        tjs_int CallFunction(tTJSVariant *ra, const tjs_int32 *code,
                             tTJSVariant **args, tjs_int numargs);

        tjs_int CallFunctionDirect(tTJSVariant *ra, const tjs_int32 *code,
                                   tTJSVariant **args, tjs_int numargs);

        tjs_int CallFunctionIndirect(tTJSVariant *ra, const tjs_int32 *code,
                                     tTJSVariant **args, tjs_int numargs);

        static void AddClassInstanceInfo(tTJSVariant *ra,
                                         const tjs_int32 *code);

        void ProcessStringFunction(const tjs_char *member, const ttstr &target,
                                   tTJSVariant **args, tjs_int numargs,
                                   tTJSVariant *result);

        void ProcessOctetFunction(const tjs_char *member,
                                  const tTJSVariantOctet *target,
                                  tTJSVariant **args, tjs_int numargs,
                                  tTJSVariant *result);

        static void TypeOf(tTJSVariant &val);

        void Eval(tTJSVariant &val, iTJSDispatch2 *objthis, bool resneed);

        static void CharacterCodeOf(tTJSVariant &val);

        static void CharacterCodeFrom(tTJSVariant &val);

        static void InstanceOf(const tTJSVariant &name, tTJSVariant &targ);

        void RegisterObjectMember(iTJSDispatch2 *dest);

        // for Byte code
        static void Add4ByteToVector(std::vector<tjs_uint8> *array, int value) {
            array->push_back((tjs_uint8)((value >> 0) & 0xff));
            array->push_back((tjs_uint8)((value >> 8) & 0xff));
            array->push_back((tjs_uint8)((value >> 16) & 0xff));
            array->push_back((tjs_uint8)((value >> 24) & 0xff));
        }

        static void Add2ByteToVector(std::vector<tjs_uint8> *array,
                                     tjs_int16 value) {
            array->push_back((tjs_uint8)((value >> 0) & 0xff));
            array->push_back((tjs_uint8)((value >> 8) & 0xff));
        }

    public:
        // iTJSDispatch2 implementation
        tjs_error FuncCall(tjs_uint32 flag, const tjs_char *membername,
                           tjs_uint32 *hint, tTJSVariant *result,
                           tjs_int numparams, tTJSVariant **param,
                           iTJSDispatch2 *objthis) override;

        tjs_error PropGet(tjs_uint32 flag, const tjs_char *membername,
                          tjs_uint32 *hint, tTJSVariant *result,
                          iTJSDispatch2 *objthis) override;

        tjs_error PropSet(tjs_uint32 flag, const tjs_char *membername,
                          tjs_uint32 *hint, const tTJSVariant *param,
                          iTJSDispatch2 *objthis) override;

        tjs_error CreateNew(tjs_uint32 flag, const tjs_char *membername,
                            tjs_uint32 *hint, iTJSDispatch2 **result,
                            tjs_int numparams, tTJSVariant **param,
                            iTJSDispatch2 *objthis) override;

        tjs_error IsInstanceOf(tjs_uint32 flag, const tjs_char *membername,
                               tjs_uint32 *hint, const tjs_char *classname,
                               iTJSDispatch2 *objthis) override;

        tjs_error GetCount(tjs_int *result, const tjs_char *membername,
                           tjs_uint32 *hint, iTJSDispatch2 *objthis) override;

        tjs_error PropSetByVS(tjs_uint32 flag, tTJSVariantString *mambername,
                              const tTJSVariant *param,
                              iTJSDispatch2 *objthis) override {
            return TJS_E_NOTIMPL;
        }

        tjs_error DeleteMember(tjs_uint32 flag, const tjs_char *membername,
                               tjs_uint32 *hint, iTJSDispatch2 *objthis) override;

        tjs_error Invalidate(tjs_uint32 flag, const tjs_char *membername,
                             tjs_uint32 *hint, iTJSDispatch2 *objthis) override;

        tjs_error IsValid(tjs_uint32 flag, const tjs_char *membername,
                          tjs_uint32 *hint, iTJSDispatch2 *objthis) override;

        tjs_error Operation(tjs_uint32 flag, const tjs_char *membername,
                            tjs_uint32 *hint, tTJSVariant *result,
                            const tTJSVariant *param,
                            iTJSDispatch2 *objthis) override;

        // for Byte code
        void SetCodeObject(tTJSInterCodeContext *parent,
                           tTJSInterCodeContext *setter,
                           tTJSInterCodeContext *getter,
                           tTJSInterCodeContext *superclass) {            Parent = parent;
            PropSetter = setter;
            if(setter)
                setter->AddRef();
            PropGetter = getter;
            if(getter)
                getter->AddRef();
            SuperClassGetter = superclass;
#ifdef _DEBUG
            if(Parent)
                Parent->AddRef();
#endif // _DEBUG
        }

        // The registration of this context as a member of its parent: the
        // list the bytecode export writes, and the member on the parent's
        // object.
        //
        // The reference's RegisterFunction does this at the end of its
        // work; the parts that generate code are the image's, so this is
        // the part that touches the objects.
        void RegisterMember();

        tTJSInterCodeContext(tTJSScriptBlock *block, const tjs_char *name,
                             tTJSContextType type, tjs_int32 *code,
                             tjs_int codeSize, tTJSVariant *data,
                             tjs_int dataSize, tjs_int varcount,
                             tjs_int verrescount, tjs_int maxframe,
                             tjs_int argcount, tjs_int arraybase,
                             tjs_int colbase, bool srcsorted,
                             tSourcePos *srcPos, tjs_int srcPosSize,
                             std::vector<tjs_int> &superpointer);

        ~tTJSInterCodeContext() override;

        // The source positions of this context: the code positions and the
        // source positions they belong to, sorted by code position. The
        // bytecode writer reads them to write the source section of a
        // bytecode file.
        [[nodiscard]] const tSourcePos *GetSourcePosArray() const {
            const_cast<tTJSInterCodeContext *>(this)->SortSourcePos();
            return SourcePosArray;
        }

        [[nodiscard]] tjs_int GetSourcePosArraySize() const {
            return SourcePosArraySize;
        }

        // Replaces the source position table. The bytecode loader reads the
        // table from the source section of the file, which carries it when
        // the objects section does not. The context takes the table over.
        void SetSourcePosArray(tSourcePos *srcPos, tjs_int size) {
            if(SourcePosArray)
                TJS_free(SourcePosArray);
            SourcePosArray = srcPos;
            SourcePosArraySize = size;
            SourcePosArraySorted = true;
        }

        std::vector<tjs_uint8> *
        ExportByteCode(bool outputdebug, tTJSScriptBlock *block,
                       class tjsConstArrayData &constarray) const;

    protected:
        void TJSVariantArrayStackAddRef();

        void TJSVariantArrayStackRelease();

        tTJSVariantArrayStack *TJSVariantArrayStack = nullptr;

    private:
        void SortSourcePos();
    };
    //---------------------------------------------------------------------------
} // namespace TJS
#endif

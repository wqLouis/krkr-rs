//---------------------------------------------------------------------------
/*
        TJS2 Script Engine( Byte Code )
        Copyright (c), Takenori Imoto

        See details of license at "license.txt"
*/
//---------------------------------------------------------------------------
#include "tjsCommHead.h"

#include "tjs.h"
#include "tjsScriptBlock.h"
#include "tjsByteCodeLoader.h"
#include "tjsGlobalStringMap.h"

namespace TJS {

    bool tTJSByteCodeLoader::IsTJS2ByteCode(const tjs_uint8 *buff) {
        // TJS2
        int tag = read4byte(buff);
        if(tag != FILE_TAG_LE)
            return false;
        // 100'\0'
        int ver = read4byte(&(buff[4]));
        if(ver != VER_TAG_LE)
            return false;
        return true;
    }

    tTJSScriptBlock *tTJSByteCodeLoader::ReadByteCode(tTJS *owner,
                                                      const tjs_char *name,
                                                      const tjs_uint8 *buf,
                                                      size_t size) {
        ReadBuffer = buf;
        ReadIndex = 0;
        ReadSize = (tjs_uint32)size;

        const tjs_uint8 *databuff = ReadBuffer;
        const size_t fileSize = size;

        // TJS2
        int tag = read4byte(databuff);
        if(tag != FILE_TAG_LE)
            return nullptr;
        // 100'\0'
        int ver = read4byte(&(databuff[4]));
        if(ver != VER_TAG_LE)
            return nullptr;

        int filesize = read4byte(&(databuff[8]));
        if(filesize != size)
            return nullptr;

        //// DATA
        tag = read4byte(&(databuff[12]));
        if(tag != DATA_TAG_LE)
            return nullptr;
        size = read4byte(&(databuff[16]));
        ReadDataArea(databuff, 20, size);

        int offset = (int)(12 + size); // これがデータエリア後の位置
        // OBJS
        tag = read4byte(&(databuff[offset]));
        offset += 4;
        if(tag != OBJ_TAG_LE)
            return nullptr;
        // int objsize = ibuff.get();
        int objsize = read4byte(&(databuff[offset]));
        offset += 4;
        const int objectsOffset = offset;

        auto *block = new tTJSScriptBlock(owner, name, 0);
        std::vector<tTJSInterCodeContext *> objs;
        tTJSInterCodeContext *top = nullptr;
        int objectsEnd = objectsOffset;
        ReadObjects(block, databuff, objectsOffset, objsize, objs, top,
                    objectsEnd);

        // The source section: the text the block was compiled from and the
        // source positions of its contexts. It follows the objects, whose
        // end the objects section itself tells.
        if(!ReadSourceSection(block, objs, databuff, objectsEnd, fileSize)) {
            // the block takes the contexts, then goes away with them
            block->SetObjects(top, objs, (int)objs.size());
            block->Release();
            return nullptr;
        }

        block->SetObjects(top, objs, (int)objs.size());
        return block;
    }

    //---------------------------------------------------------------------------
    // Reads the source section: the text the block was compiled from and the
    // source positions of its contexts.
    //---------------------------------------------------------------------------
    // The section is optional: a file the original compiler wrote has none,
    // and the block it produces then carries no line information. A section
    // that is there but malformed rejects the file.
    bool tTJSByteCodeLoader::ReadSourceSection(
        tTJSScriptBlock *block,
        std::vector<tTJSInterCodeContext *> &objs, const tjs_uint8 *buff,
        int offset, size_t fileSize) {
        if(offset < 0 || (size_t)offset + 8 > fileSize)
            return true; // no source section
        if(read4byte(&(buff[offset])) != SRC_TAG_LE)
            return true; // no source section
        const int payloadSize = read4byte(&(buff[offset + 4]));
        if(payloadSize < 8 ||
           (size_t)offset + 8 + (size_t)payloadSize > fileSize)
            return false;
        const int payloadEnd = offset + 8 + payloadSize;
        int p = offset + 8;

        const int flags = read4byte(&(buff[p]));
        p += 4;
        const int charCount = read4byte(&(buff[p]));
        p += 4;
        if(charCount < 0 || p + (size_t)charCount * 2 > (size_t)payloadEnd)
            return false;
        std::vector<tjs_char> text((size_t)charCount + 1);
        for(int i = 0; i < charCount; i++) {
            text[i] = (tjs_char)read2byte(&(buff[p + i * 2]));
        }
        text[charCount] = 0;
        p += charCount * 2;
        block->SetSourceText(text.data());

        if(flags & 1) {
            // the source positions of every context
            if(p + 4 > payloadEnd)
                return false;
            const int contextCount = read4byte(&(buff[p]));
            p += 4;
            if(contextCount < 0)
                return false;
            for(int c = 0; c < contextCount; c++) {
                if(p + 4 > payloadEnd)
                    return false;
                const int positionCount = read4byte(&(buff[p]));
                p += 4;
                if(positionCount < 0 ||
                   p + (size_t)positionCount * 8 > (size_t)payloadEnd)
                    return false;
                if(c >= (int)objs.size() || !objs[c])
                    return false;
                tTJSInterCodeContext::tSourcePos *table = nullptr;
                if(positionCount > 0) {
                    table = (tTJSInterCodeContext::tSourcePos *)TJS_malloc(
                        sizeof(tTJSInterCodeContext::tSourcePos) *
                        positionCount);
                    if(!table)
                        TJS_eTJSScriptError(TJSInsufficientMem, block, 0);
                    for(int i = 0; i < positionCount; i++) {
                        table[i].CodePos = read4byte(&(buff[p]));
                        p += 4;
                    }
                    for(int i = 0; i < positionCount; i++) {
                        table[i].SourcePos = read4byte(&(buff[p]));
                        p += 4;
                    }
                }
                objs[c]->SetSourcePosArray(table, positionCount);
            }
        }
        return true;
    }

    void tTJSByteCodeLoader::ReadDataArea(const tjs_uint8 *buff, int offset,
                                          size_t size) {
        int count = read4byte(&(buff[offset]));
        offset += 4;
        if(count > 0) {
            ByteArray.set((tjs_int8 *)&buff[offset], count);
            int stride = (count + 3) >> 2;
            offset += stride << 2;
        }
        count = read4byte(&(buff[offset]));
        offset += 4;
        if(count > 0) { // load short
            ShortArray.clear();
            ShortArray.reserve(count);
            for(int i = 0; i < count; i++) {
                ShortArray.push_back(read2byte(&(buff[offset])));
                offset += 2;
            }
            offset += (count & 1) << 1;
        }
        count = read4byte(&(buff[offset]));
        offset += 4;
        if(count > 0) {
            LongArray.clear();
            LongArray.reserve(count);
            for(int i = 0; i < count; i++) {
                LongArray.push_back(read4byte(&(buff[offset])));
                offset += 4;
            }
        }
        count = read4byte(&(buff[offset]));
        offset += 4;
        if(count > 0) { // load long
            LongLongArray.clear();
            LongLongArray.reserve(count);
            for(int i = 0; i < count; i++) {
                LongLongArray.push_back(read8byte(&(buff[offset])));
                offset += 8;
            }
        }
        count = read4byte(&(buff[offset]));
        offset += 4;
        if(count > 0) { // load double
            DoubleArray.clear();
            DoubleArray.reserve(count);
            for(int i = 0; i < count; i++) {
                tjs_uint64 tmp = read8byte(&(buff[offset]));
                DoubleArray.push_back(*(double *)&tmp);
                offset += 8;
            }
        }
        count = read4byte(&(buff[offset]));
        offset += 4;
        if(count > 0) {
            StringArray.clear();
            StringArray.reserve(count);
            for(int i = 0; i < count; i++) {
                int len = read4byte(&(buff[offset]));
                offset += 4;
                std::vector<tjs_uint16> ch(len + 1);
                ch[len] = 0;
                for(int j = 0; j < len; j++) {
                    ch[j] = read2byte(&(buff[offset]));
                    offset += 2;
                }
                StringArray.push_back(
                    TJSMapGlobalStringMap((const tjs_char *)&(ch[0])));
                offset += (len & 1) << 1;
            }
        }
        count = read4byte(&(buff[offset]));
        offset += 4;
        if(count > 0) {
            OctetArray.clear();
            OctetArray.reserve(count);
            for(int i = 0; i < count; i++) {
                int len = read4byte(&(buff[offset]));
                offset += 4;
                auto *octet = new tTJSVariantOctet(&(buff[offset]),
                                                   len); // データはコピーされる
                OctetArray.push_back(octet);
                offset += ((len + 3) >> 2) << 2;
            }
        }
    }

    void tTJSByteCodeLoader::ReadObjects(
        tTJSScriptBlock *block, const tjs_uint8 *buff, int offset, int size,
        std::vector<tTJSInterCodeContext *> &objs,
        tTJSInterCodeContext *&outTopLevel, int &outObjectsEnd) {
        int toplevel = read4byte(&(buff[offset]));
        offset += 4;
        int objcount = read4byte(&(buff[offset]));
        offset += 4;

        // tTJSInterCodeContext** objs = new
        // tTJSInterCodeContext*[objcount];
        objs.resize(objcount);
        std::vector<VariantRepalace> work;
        std::vector<int> parent(objcount);
        std::vector<int> propSetter(objcount);
        std::vector<int> propGetter(objcount);
        std::vector<int> superClassGetter(objcount);
        std::vector<std::vector<int>> properties(objcount);
        for(int o = 0; o < objcount; o++) {
            int tag = read4byte(&(buff[offset]));
            offset += 4;
            if(tag != FILE_TAG_LE) {
                // throw new TJSException(Error.ByteCodeBroken);
                TJS_eTJSScriptError(TJSByteCodeBroken, block, 0);
            }
            // int objsize = read4byte( &(buff[offset]) );
            offset += 4;
            parent[o] = read4byte(&(buff[offset]));
            offset += 4;
            int name = read4byte(&(buff[offset]));
            offset += 4;
            int contextType = read4byte(&(buff[offset]));
            offset += 4;
            int maxVariableCount = read4byte(&(buff[offset]));
            offset += 4;
            int variableReserveCount = read4byte(&(buff[offset]));
            offset += 4;
            int maxFrameCount = read4byte(&(buff[offset]));
            offset += 4;
            int funcDeclArgCount = read4byte(&(buff[offset]));
            offset += 4;
            int funcDeclUnnamedArgArrayBase = read4byte(&(buff[offset]));
            offset += 4;
            int funcDeclCollapseBase = read4byte(&(buff[offset]));
            offset += 4;
            propSetter[o] = read4byte(&(buff[offset]));
            offset += 4;
            propGetter[o] = read4byte(&(buff[offset]));
            offset += 4;
            superClassGetter[o] = read4byte(&(buff[offset]));
            offset += 4;

            int count = read4byte(&(buff[offset]));
            offset += 4;

            // The source positions of this context. The objects section
            // carries them when the debug information was asked for; the
            // source section carries them otherwise, and replaces the ones
            // read here.
            tTJSInterCodeContext::tSourcePos *srcPos = nullptr;
            tjs_int srcPosArraySize = 0;
            if(count > 0) {
                srcPos = (tTJSInterCodeContext::tSourcePos *)TJS_malloc(
                    sizeof(tTJSInterCodeContext::tSourcePos) * count);
                if(!srcPos)
                    TJS_eTJSScriptError(TJSInsufficientMem, block, 0);
                srcPosArraySize = count;
                for(int i = 0; i < count; i++) {
                    srcPos[i].CodePos = read4byte(&(buff[offset]));
                    offset += 4;
                }
                for(int i = 0; i < count; i++) {
                    srcPos[i].SourcePos = read4byte(&(buff[offset]));
                    offset += 4;
                }
            } else {
                offset += count << 3;
            }

            count = read4byte(&(buff[offset]));
            const tjs_int codeSize = count;
            offset += 4;
            tjs_int32 *code =
                (tjs_int32 *)TJS_malloc(count * sizeof(tjs_int32));
            for(int i = 0; i < count; i++) {
                tjs_int16 c = (tjs_int16)read2byte(&(buff[offset]));
                code[i] = c;
                offset += 2;
            }
            TranslateCodeAddress(block, code, codeSize);
            offset += (count & 1) << 1;

            count = read4byte(&(buff[offset]));
            offset += 4;
            int vcount = count << 1;
            std::vector<short> data(vcount);
            for(int i = 0; i < vcount; i++) {
                data[i] = read2byte(&(buff[offset]));
                offset += 2;
            }

            auto *vdata = new tTJSVariant[count];
            const tjs_int datacount = count;
            for(int i = 0; i < datacount; i++) {
                int pos = i << 1;
                int type = data[pos];
                int index = data[pos + 1];
                switch(type) {
                    case TYPE_VOID:
                        vdata[i].Clear();
                        break;
                    case TYPE_OBJECT:
                        vdata[i] = (iTJSDispatch2 *)nullptr;
                        break;
                    case TYPE_INTER_OBJECT:
                        work.emplace_back(&(vdata[i]), index);
                        break;
                    case TYPE_INTER_GENERATOR:
                        work.emplace_back(&(vdata[i]), index);
                        break;
                    case TYPE_STRING:
                        vdata[i] = StringArray[index].c_str(); // tTJSString
                        break;
                    case TYPE_OCTET:
                        vdata[i] = OctetArray[index]; // tTJSVariantOctet
                        break;
                    case TYPE_REAL:
                        vdata[i] = (tjs_real)DoubleArray[index];
                        break;
                    case TYPE_BYTE:
                        vdata[i] = (tjs_int)ByteArray[index];
                        break;
                    case TYPE_SHORT:
                        vdata[i] = (tjs_int)ShortArray[index];
                        break;
                    case TYPE_INTEGER:
                        vdata[i] = (tjs_int)LongArray[index];
                        break;
                    case TYPE_LONG:
                        vdata[i] = (tjs_int64)LongLongArray[index];
                        break;
                    case TYPE_UNKNOWN:
                    default:
                        vdata[i].Clear();
                        break;
                }
            }
            count = read4byte(&(buff[offset]));
            offset += 4;
            // int* scgetterps = new int[count];
            std::vector<tjs_int> scgetterps(count);
            for(int i = 0; i < count; i++) {
                scgetterps[i] = read4byte(&(buff[offset]));
                offset += 4;
            }
            // properties
            count = read4byte(&(buff[offset]));
            offset += 4;
            if(count > 0) {
                int pcount = count << 1;
                std::vector<int> &props = properties[o];
                props.resize(pcount);
                for(int i = 0; i < pcount; i++) {
                    props[i] = read4byte(&(buff[offset]));
                    offset += 4;
                }
            }

            tTJSInterCodeContext *obj = new tTJSInterCodeContext(
                block, StringArray[name].c_str(), (tTJSContextType)contextType,
                code, codeSize, vdata, datacount, maxVariableCount,
                variableReserveCount, maxFrameCount, funcDeclArgCount,
                funcDeclUnnamedArgArrayBase, funcDeclCollapseBase, true, srcPos,
                srcPosArraySize, scgetterps);
            objs[o] = obj;
        }
        tTJSVariant val;
        for(int o = 0; o < objcount; o++) {
            tTJSInterCodeContext *parentObj = nullptr;
            tTJSInterCodeContext *propSetterObj = nullptr;
            tTJSInterCodeContext *propGetterObj = nullptr;
            tTJSInterCodeContext *superClassGetterObj = nullptr;

            if(parent[o] >= 0) {
                parentObj = objs[parent[o]];
            }
            if(propSetter[o] >= 0) {
                propSetterObj = objs[propSetter[o]];
            }
            if(propGetter[o] >= 0) {
                propGetterObj = objs[propGetter[o]];
            }
            if(superClassGetter[o] >= 0) {
                superClassGetterObj = objs[superClassGetter[o]];
            }
            objs[o]->SetCodeObject(parentObj, propSetterObj, propGetterObj,
                                   superClassGetterObj);

            if(properties[o].size() > 0) {
                tTJSInterCodeContext *obj = parentObj;
                std::vector<int> &prop = properties[o];
                int length = (int)(prop.size() >> 1);
                for(int i = 0; i < length; i++) {
                    int pos = i << 1;
                    int pname = prop[pos];
                    int pobj = prop[pos + 1];
                    // register members to the parent object
                    val = objs[pobj];
                    obj->PropSet(TJS_MEMBERENSURE | TJS_IGNOREPROP,
                                 StringArray[pname].c_str(), nullptr, &val,
                                 obj);
                }
            }
        }
        int count = (int)work.size();
        for(int i = 0; i < count; i++) {
            VariantRepalace &w = work[i];
            (*w.Work) = objs[w.Index];
        }
        work.clear();
        tTJSInterCodeContext *top = nullptr;
        if(toplevel >= 0 && toplevel < objcount) {
            top = objs[toplevel];
        }
        outTopLevel = top;
        // the source section follows the objects
        outObjectsEnd = offset;
        // delete[] objs;
    }

    //---------------------------------------------------------------------------
    /**
     * The bytecode file carries the operands of an instruction as indices;
     * this turns them back into the VM addresses of the code.
     */
    void tTJSByteCodeLoader::TranslateCodeAddress(tTJSScriptBlock *block,
                                                  tjs_int32 *code,
                                                  const tjs_int32 codeSize) {
        // The layout of every opcode is the one table the writer serialised
        // the file with, so the two never disagree about how many operands
        // an instruction has. An instruction that does not fit the code is
        // a broken file, not a silently skipped operand.
        tjs_int i = 0;
        while(i < codeSize) {
            bool broken = false;
            const tjs_int size = TranslateVMInstruction(
                code, codeSize, i, tTJSVMAddrDir::ToVMAddr, &broken);
            if(broken) {
                TJS_eTJSScriptError(TJSByteCodeBroken, block, 0);
            }
            i += size;
        }
        if(codeSize != i) {
            TJS_eTJSScriptError(TJSByteCodeBroken, block, 0);
        }
    }

} // namespace TJS

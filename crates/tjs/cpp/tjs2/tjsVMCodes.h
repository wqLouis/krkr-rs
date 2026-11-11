//---------------------------------------------------------------------------
/*
        TJS2 Script Engine
        Copyright (C) 2000 W.Dee <dee@kikyou.info> and contributors

        See details of license at "license.txt"
*/
//---------------------------------------------------------------------------
// VM opcodes and the layout of their operands
//---------------------------------------------------------------------------
#ifndef tjsVMCodesH
#define tjsVMCodesH

#include "tjsCommHead.h"

#include <array>
#include <initializer_list>

namespace TJS {
    //---------------------------------------------------------------------------
    // The VM opcodes
    //---------------------------------------------------------------------------
#define TJS_NORMAL_AND_PROPERTY_ACCESSER(x) x, x##PD, x##PI, x##P

    enum tTJSVMCodes {

        VM_NOP,
        VM_CONST,
        VM_CP,
        VM_CL,
        VM_CCL,
        VM_TT,
        VM_TF,
        VM_CEQ,
        VM_CDEQ,
        VM_CLT,
        VM_CGT,
        VM_SETF,
        VM_SETNF,
        VM_LNOT,
        VM_NF,
        VM_JF,
        VM_JNF,
        VM_JMP,

        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_INC),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_DEC),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_LOR),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_LAND),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_BOR),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_BXOR),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_BAND),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_SAR),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_SAL),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_SR),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_ADD),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_SUB),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_MOD),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_DIV),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_IDIV),
        TJS_NORMAL_AND_PROPERTY_ACCESSER(VM_MUL),

        VM_BNOT,
        VM_TYPEOF,
        VM_TYPEOFD,
        VM_TYPEOFI,
        VM_EVAL,
        VM_EEXP,
        VM_CHKINS,
        VM_ASC,
        VM_CHR,
        VM_NUM,
        VM_CHS,
        VM_INV,
        VM_CHKINV,
        VM_INT,
        VM_REAL,
        VM_STR,
        VM_OCTET,
        VM_CALL,
        VM_CALLD,
        VM_CALLI,
        VM_NEW,
        VM_GPD,
        VM_SPD,
        VM_SPDE,
        VM_SPDEH,
        VM_GPI,
        VM_SPI,
        VM_SPIE,
        VM_GPDS,
        VM_SPDS,
        VM_GPIS,
        VM_SPIS,
        VM_SETP,
        VM_GETP,
        VM_DELD,
        VM_DELI,
        VM_SRV,
        VM_RET,
        VM_ENTRY,
        VM_EXTRY,
        VM_THROW,
        VM_CHGTHIS,
        VM_GLOBAL,
        VM_ADDCI,
        VM_REGMEMBER,
        VM_DEBUGGER,

        __VM_LAST /* = last mark ; this is not a real operation code
                   */
    };

#undef TJS_NORMAL_AND_PROPERTY_ACCESSER
    //---------------------------------------------------------------------------
    // The address encoding of the VM code
    //---------------------------------------------------------------------------
#define TJS_TO_VM_CODE_ADDR(x) ((x) * (tjs_int)sizeof(tjs_uint32))
#define TJS_TO_VM_REG_ADDR(x) ((x) * (tjs_int)sizeof(tTJSVariant))
#define TJS_FROM_VM_CODE_ADDR(x) ((tjs_int)(x) / (tjs_int)sizeof(tjs_uint32))
#define TJS_FROM_VM_REG_ADDR(x) ((tjs_int)(x) / (tjs_int)sizeof(tTJSVariant))
#define TJS_ADD_VM_CODE_ADDR(dest, x) ((*(char **)&(dest)) += (x))
#define TJS_GET_VM_REG_ADDR(base, x)                                           \
    ((tTJSVariant *)((char *)(base) + (tjs_int)(x)))
#define TJS_GET_VM_REG(base, x) (*(TJS_GET_VM_REG_ADDR(base, x)))
    //---------------------------------------------------------------------------
    // The argument passing flags of a call instruction
    //---------------------------------------------------------------------------
    enum tTJSFuncArgType { fatNormal, fatExpand, fatUnnamedExpand };
    //---------------------------------------------------------------------------
    // The operand layout of one opcode
    //---------------------------------------------------------------------------
    // The kind of one operand slot of an instruction.
    enum class tTJSVMArgKind : tjs_uint8 {
        Plain = 0, // a value that is not an address
        Reg = 1,   // a register address
        Code = 2,  // a code address
    };

    // How the operand list of an instruction ends.
    enum class tTJSVMTailKind : tjs_uint8 {
        None = 0,
        // A count slot followed by that many register operands. A count of
        // -1 names no arguments; a count of -2 names expanded arguments: the
        // next slot holds the number of (flag, operand) pairs.
        RegList = 1,
    };

    // The operand layout of one opcode: the fixed operand slots that follow
    // the opcode, and the shape of the operand list that follows them.
    struct tTJSVMOpLayout {
        tTJSVMArgKind fixed[4];
        tjs_uint8 fixedCount;
        tTJSVMTailKind tail;
        bool valid;

        [[nodiscard]] constexpr bool IsValid() const { return valid; }
    };
    //---------------------------------------------------------------------------
    namespace detail {
        // Builds the layout of an instruction with the given operand slots.
        constexpr tTJSVMOpLayout
        VMOpLayout(std::initializer_list<tTJSVMArgKind> kinds,
                   tTJSVMTailKind tail = tTJSVMTailKind::None) {
            tTJSVMOpLayout layout{};
            layout.fixedCount = 0;
            layout.tail = tail;
            layout.valid = true;
            for(auto kind : kinds) {
                layout.fixed[layout.fixedCount++] = kind;
            }
            return layout;
        }
    } // namespace detail
    //---------------------------------------------------------------------------
    // The layout of every opcode
    //---------------------------------------------------------------------------
    // This is the one table that describes the VM code: the bytecode writer
    // serialises an instruction with it and the bytecode loader reads it back
    // with it. An opcode without a layout here has no defined encoding, so
    // the table is checked at compile time to cover every opcode.
    inline constexpr std::array<tTJSVMOpLayout, __VM_LAST> MakeVMOpLayouts() {
        std::array<tTJSVMOpLayout, __VM_LAST> table{};

        // instructions without an operand
        table[VM_NOP] = detail::VMOpLayout({});
        table[VM_NF] = detail::VMOpLayout({});
        table[VM_RET] = detail::VMOpLayout({});
        table[VM_EXTRY] = detail::VMOpLayout({});
        table[VM_REGMEMBER] = detail::VMOpLayout({});
        table[VM_DEBUGGER] = detail::VMOpLayout({});

        // instructions with one register operand
        table[VM_TT] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_TF] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_SETF] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_SETNF] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_LNOT] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_BNOT] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_ASC] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_CHR] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_NUM] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_CHS] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_CL] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_INV] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_CHKINV] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_TYPEOF] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_EVAL] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_EEXP] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_INT] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_REAL] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_STR] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_OCTET] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_SRV] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_THROW] = detail::VMOpLayout({tTJSVMArgKind::Reg});
        table[VM_GLOBAL] = detail::VMOpLayout({tTJSVMArgKind::Reg});

        // instructions with one code address operand
        table[VM_JF] = detail::VMOpLayout({tTJSVMArgKind::Code});
        table[VM_JNF] = detail::VMOpLayout({tTJSVMArgKind::Code});
        table[VM_JMP] = detail::VMOpLayout({tTJSVMArgKind::Code});

        // instructions with two register operands
        table[VM_CP] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                            tTJSVMArgKind::Reg});
        table[VM_CEQ] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg});
        table[VM_CDEQ] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg});
        table[VM_CLT] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg});
        table[VM_CGT] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg});
        table[VM_CHKINS] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                                tTJSVMArgKind::Reg});
        table[VM_CONST] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                               tTJSVMArgKind::Reg});
        table[VM_SETP] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg});
        table[VM_GETP] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg});
        table[VM_ADDCI] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                               tTJSVMArgKind::Reg});
        table[VM_CHGTHIS] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                                 tTJSVMArgKind::Reg});

        // instructions with three register operands
        table[VM_GPD] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg});
        table[VM_GPDS] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg});
        table[VM_SPD] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg});
        table[VM_SPDE] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg});
        table[VM_SPDEH] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                               tTJSVMArgKind::Reg,
                                               tTJSVMArgKind::Reg});
        table[VM_SPDS] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg});
        table[VM_GPI] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg});
        table[VM_GPIS] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg});
        table[VM_SPI] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg});
        table[VM_SPIE] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                               tTJSVMArgKind::Reg,
                                               tTJSVMArgKind::Reg});
        table[VM_SPIS] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg});
        table[VM_DELD] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg});
        table[VM_TYPEOFD] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                                 tTJSVMArgKind::Reg,
                                                 tTJSVMArgKind::Reg});
        table[VM_DELI] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg,
                                              tTJSVMArgKind::Reg});
        table[VM_TYPEOFI] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                                 tTJSVMArgKind::Reg,
                                                 tTJSVMArgKind::Reg});

        // the class instance clear: a register and a plain count
        table[VM_CCL] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Plain});

        // the try-protected block entry: a code address and a register
        table[VM_ENTRY] = detail::VMOpLayout({tTJSVMArgKind::Code,
                                               tTJSVMArgKind::Reg});

        // The increment and decrement families take one register, their
        // direct and indirect property variants take three and their
        // property variant takes two.
#define TJS_VM_INCDEC_FAMILY(base)                                               \
    table[base] = detail::VMOpLayout({tTJSVMArgKind::Reg});                      \
    table[base##PD] = detail::VMOpLayout(                                        \
        {tTJSVMArgKind::Reg, tTJSVMArgKind::Reg, tTJSVMArgKind::Reg});           \
    table[base##PI] = detail::VMOpLayout(                                        \
        {tTJSVMArgKind::Reg, tTJSVMArgKind::Reg, tTJSVMArgKind::Reg});           \
    table[base##P] = detail::VMOpLayout(                                         \
        {tTJSVMArgKind::Reg, tTJSVMArgKind::Reg})

        TJS_VM_INCDEC_FAMILY(VM_INC);
        TJS_VM_INCDEC_FAMILY(VM_DEC);

#undef TJS_VM_INCDEC_FAMILY
        // The operator families take two registers, their direct and indirect
        // property variants take four and their property variant takes three.
#define TJS_VM_OP2_FAMILY(base)                                                 \
    table[base] = detail::VMOpLayout({tTJSVMArgKind::Reg,                        \
                                       tTJSVMArgKind::Reg});                     \
    table[base##PD] = detail::VMOpLayout(                                        \
        {tTJSVMArgKind::Reg, tTJSVMArgKind::Reg, tTJSVMArgKind::Reg,             \
         tTJSVMArgKind::Reg});                                                    \
    table[base##PI] = detail::VMOpLayout(                                        \
        {tTJSVMArgKind::Reg, tTJSVMArgKind::Reg, tTJSVMArgKind::Reg,             \
         tTJSVMArgKind::Reg});                                                    \
    table[base##P] = detail::VMOpLayout(                                         \
        {tTJSVMArgKind::Reg, tTJSVMArgKind::Reg, tTJSVMArgKind::Reg})

        TJS_VM_OP2_FAMILY(VM_LOR);
        TJS_VM_OP2_FAMILY(VM_LAND);
        TJS_VM_OP2_FAMILY(VM_BOR);
        TJS_VM_OP2_FAMILY(VM_BXOR);
        TJS_VM_OP2_FAMILY(VM_BAND);
        TJS_VM_OP2_FAMILY(VM_SAR);
        TJS_VM_OP2_FAMILY(VM_SAL);
        TJS_VM_OP2_FAMILY(VM_SR);
        TJS_VM_OP2_FAMILY(VM_ADD);
        TJS_VM_OP2_FAMILY(VM_SUB);
        TJS_VM_OP2_FAMILY(VM_MOD);
        TJS_VM_OP2_FAMILY(VM_DIV);
        TJS_VM_OP2_FAMILY(VM_IDIV);
        TJS_VM_OP2_FAMILY(VM_MUL);

#undef TJS_VM_OP2_FAMILY
        // The call instructions take the result register and the object
        // register, the direct and indirect call variants take the member
        // register as well, and all of them end with the argument list.
        table[VM_CALL] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                               tTJSVMArgKind::Reg},
                                              tTJSVMTailKind::RegList);
        table[VM_NEW] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                             tTJSVMArgKind::Reg},
                                            tTJSVMTailKind::RegList);
        table[VM_CALLD] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                                tTJSVMArgKind::Reg,
                                                tTJSVMArgKind::Reg},
                                               tTJSVMTailKind::RegList);
        table[VM_CALLI] = detail::VMOpLayout({tTJSVMArgKind::Reg,
                                                tTJSVMArgKind::Reg,
                                                tTJSVMArgKind::Reg},
                                               tTJSVMTailKind::RegList);

        return table;
    }

    inline constexpr std::array<tTJSVMOpLayout, __VM_LAST>
        VMOpLayouts = MakeVMOpLayouts();

    namespace detail {
        constexpr bool AllVMOpLayoutsValid(
            const std::array<tTJSVMOpLayout, __VM_LAST> &table) {
            for(const auto &layout : table) {
                if(!layout.IsValid())
                    return false;
            }
            return true;
        }
    } // namespace detail

    // An opcode without a layout has no defined encoding: the writer could
    // not serialise it and the loader could not read it back. Adding an
    // opcode without adding its layout fails the build here.
    static_assert(detail::AllVMOpLayoutsValid(VMOpLayouts),
                  "every VM opcode needs an operand layout");
    //---------------------------------------------------------------------------
    // The direction of an address translation
    //---------------------------------------------------------------------------
    enum class tTJSVMAddrDir : tjs_uint8 {
        ToIndex = 0,  // a VM address becomes an index
        ToVMAddr = 1, // an index becomes a VM address
    };
    //---------------------------------------------------------------------------
    // Translates the address operands of the instruction at `code[i]` and
    // returns the number of slots the instruction occupies.
    //---------------------------------------------------------------------------
    // `outBroken` is set when the instruction cannot be translated: an opcode
    // with no layout, or an operand slot outside the code array. The caller
    // reports it; the translation never skips an operand silently.
    inline tjs_int TranslateVMInstruction(tjs_int32 *code, tjs_int codeSize,
                                           tjs_int i, tTJSVMAddrDir dir,
                                           bool *outBroken) {
        *outBroken = false;
        if(i < 0 || i >= codeSize) {
            *outBroken = true;
            return 0;
        }
        const tjs_int opcode = code[i];
        if(opcode < 0 || opcode >= __VM_LAST) {
            *outBroken = true;
            return 0;
        }
        const tTJSVMOpLayout &layout = VMOpLayouts[opcode];
        if(!layout.IsValid()) {
            *outBroken = true;
            return 0;
        }

        auto convert = [dir](tjs_int32 &value, tTJSVMArgKind kind) {
            switch(kind) {
                case tTJSVMArgKind::Reg:
                    value = dir == tTJSVMAddrDir::ToIndex
                                ? (tjs_int32)TJS_FROM_VM_REG_ADDR(value)
                                : (tjs_int32)TJS_TO_VM_REG_ADDR(value);
                    break;
                case tTJSVMArgKind::Code:
                    value = dir == tTJSVMAddrDir::ToIndex
                                ? (tjs_int32)TJS_FROM_VM_CODE_ADDR(value)
                                : (tjs_int32)TJS_TO_VM_CODE_ADDR(value);
                    break;
                case tTJSVMArgKind::Plain:
                    break;
            }
        };

        tjs_int slot = i + 1;
        for(tjs_int k = 0; k < layout.fixedCount; k++) {
            if(slot >= codeSize) {
                *outBroken = true;
                return 0;
            }
            convert(code[slot], layout.fixed[k]);
            slot++;
        }

        if(layout.tail == tTJSVMTailKind::RegList) {
            if(slot >= codeSize) {
                *outBroken = true;
                return 0;
            }
            const tjs_int count = code[slot];
            slot++;
            if(count == -1) {
                // no argument
            } else if(count == -2) {
                // expanded arguments: a count and that many (flag, operand)
                // pairs
                if(slot >= codeSize) {
                    *outBroken = true;
                    return 0;
                }
                const tjs_int pairCount = code[slot];
                slot++;
                if(pairCount < 0) {
                    *outBroken = true;
                    return 0;
                }
                for(tjs_int j = 0; j < pairCount; j++) {
                    if(slot + 1 >= codeSize) {
                        *outBroken = true;
                        return 0;
                    }
                    const tjs_int flag = code[slot];
                    if(flag == fatNormal || flag == fatExpand) {
                        convert(code[slot + 1], tTJSVMArgKind::Reg);
                    }
                    slot += 2;
                }
            } else {
                if(count < 0) {
                    *outBroken = true;
                    return 0;
                }
                for(tjs_int j = 0; j < count; j++) {
                    if(slot >= codeSize) {
                        *outBroken = true;
                        return 0;
                    }
                    convert(code[slot], tTJSVMArgKind::Reg);
                    slot++;
                }
            }
        }

        return slot - i;
    }
    //---------------------------------------------------------------------------
} // namespace TJS
#endif

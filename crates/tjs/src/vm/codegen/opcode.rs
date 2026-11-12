//! The opcodes of the intermediate VM.
//!
//! The opcodes are the intermediate VM's, in the VM's order: the
//! executor reads the stream the generator writes, so the numbering must
//! match the executor's exactly.

/// The opcode of an instruction.
pub type Opcode = i32;

/// The opcodes of the intermediate VM.
///
/// The list is the executor's, in the same order and with the same
/// numbers.
pub mod vm {
    use super::Opcode;

    /// The `NOP` instruction.
    pub const NOP: Opcode = 0;
    /// The `CONST` instruction.
    pub const CONST: Opcode = 1;
    /// The `CP` instruction.
    pub const CP: Opcode = 2;
    /// The `CL` instruction.
    pub const CL: Opcode = 3;
    /// The `CCL` instruction.
    pub const CCL: Opcode = 4;
    /// The `TT` instruction.
    pub const TT: Opcode = 5;
    /// The `TF` instruction.
    pub const TF: Opcode = 6;
    /// The `CEQ` instruction.
    pub const CEQ: Opcode = 7;
    /// The `CDEQ` instruction.
    pub const CDEQ: Opcode = 8;
    /// The `CLT` instruction.
    pub const CLT: Opcode = 9;
    /// The `CGT` instruction.
    pub const CGT: Opcode = 10;
    /// The `SETF` instruction.
    pub const SETF: Opcode = 11;
    /// The `SETNF` instruction.
    pub const SETNF: Opcode = 12;
    /// The `LNOT` instruction.
    pub const LNOT: Opcode = 13;
    /// The `NF` instruction.
    pub const NF: Opcode = 14;
    /// The `JF` instruction.
    pub const JF: Opcode = 15;
    /// The `JNF` instruction.
    pub const JNF: Opcode = 16;
    /// The `JMP` instruction.
    pub const JMP: Opcode = 17;

    /// The `INC` instruction.
    pub const INC: Opcode = 18;
    /// The `INC_PD` instruction.
    pub const INC_PD: Opcode = 19;
    /// The `INC_PI` instruction.
    pub const INC_PI: Opcode = 20;
    /// The `INC_P` instruction.
    pub const INC_P: Opcode = 21;
    /// The `DEC` instruction.
    pub const DEC: Opcode = 22;
    /// The `DEC_PD` instruction.
    pub const DEC_PD: Opcode = 23;
    /// The `DEC_PI` instruction.
    pub const DEC_PI: Opcode = 24;
    /// The `DEC_P` instruction.
    pub const DEC_P: Opcode = 25;
    /// The `LOR` instruction.
    pub const LOR: Opcode = 26;
    /// The `LOR_PD` instruction.
    pub const LOR_PD: Opcode = 27;
    /// The `LOR_PI` instruction.
    pub const LOR_PI: Opcode = 28;
    /// The `LOR_P` instruction.
    pub const LOR_P: Opcode = 29;
    /// The `LAND` instruction.
    pub const LAND: Opcode = 30;
    /// The `LAND_PD` instruction.
    pub const LAND_PD: Opcode = 31;
    /// The `LAND_PI` instruction.
    pub const LAND_PI: Opcode = 32;
    /// The `LAND_P` instruction.
    pub const LAND_P: Opcode = 33;
    /// The `BOR` instruction.
    pub const BOR: Opcode = 34;
    /// The `BOR_PD` instruction.
    pub const BOR_PD: Opcode = 35;
    /// The `BOR_PI` instruction.
    pub const BOR_PI: Opcode = 36;
    /// The `BOR_P` instruction.
    pub const BOR_P: Opcode = 37;
    /// The `BXOR` instruction.
    pub const BXOR: Opcode = 38;
    /// The `BXOR_PD` instruction.
    pub const BXOR_PD: Opcode = 39;
    /// The `BXOR_PI` instruction.
    pub const BXOR_PI: Opcode = 40;
    /// The `BXOR_P` instruction.
    pub const BXOR_P: Opcode = 41;
    /// The `BAND` instruction.
    pub const BAND: Opcode = 42;
    /// The `BAND_PD` instruction.
    pub const BAND_PD: Opcode = 43;
    /// The `BAND_PI` instruction.
    pub const BAND_PI: Opcode = 44;
    /// The `BAND_P` instruction.
    pub const BAND_P: Opcode = 45;
    /// The `SAR` instruction.
    pub const SAR: Opcode = 46;
    /// The `SAR_PD` instruction.
    pub const SAR_PD: Opcode = 47;
    /// The `SAR_PI` instruction.
    pub const SAR_PI: Opcode = 48;
    /// The `SAR_P` instruction.
    pub const SAR_P: Opcode = 49;
    /// The `SAL` instruction.
    pub const SAL: Opcode = 50;
    /// The `SAL_PD` instruction.
    pub const SAL_PD: Opcode = 51;
    /// The `SAL_PI` instruction.
    pub const SAL_PI: Opcode = 52;
    /// The `SAL_P` instruction.
    pub const SAL_P: Opcode = 53;
    /// The `SR` instruction.
    pub const SR: Opcode = 54;
    /// The `SR_PD` instruction.
    pub const SR_PD: Opcode = 55;
    /// The `SR_PI` instruction.
    pub const SR_PI: Opcode = 56;
    /// The `SR_P` instruction.
    pub const SR_P: Opcode = 57;
    /// The `ADD` instruction.
    pub const ADD: Opcode = 58;
    /// The `ADD_PD` instruction.
    pub const ADD_PD: Opcode = 59;
    /// The `ADD_PI` instruction.
    pub const ADD_PI: Opcode = 60;
    /// The `ADD_P` instruction.
    pub const ADD_P: Opcode = 61;
    /// The `SUB` instruction.
    pub const SUB: Opcode = 62;
    /// The `SUB_PD` instruction.
    pub const SUB_PD: Opcode = 63;
    /// The `SUB_PI` instruction.
    pub const SUB_PI: Opcode = 64;
    /// The `SUB_P` instruction.
    pub const SUB_P: Opcode = 65;
    /// The `MOD` instruction.
    pub const MOD: Opcode = 66;
    /// The `MOD_PD` instruction.
    pub const MOD_PD: Opcode = 67;
    /// The `MOD_PI` instruction.
    pub const MOD_PI: Opcode = 68;
    /// The `MOD_P` instruction.
    pub const MOD_P: Opcode = 69;
    /// The `DIV` instruction.
    pub const DIV: Opcode = 70;
    /// The `DIV_PD` instruction.
    pub const DIV_PD: Opcode = 71;
    /// The `DIV_PI` instruction.
    pub const DIV_PI: Opcode = 72;
    /// The `DIV_P` instruction.
    pub const DIV_P: Opcode = 73;
    /// The `IDIV` instruction.
    pub const IDIV: Opcode = 74;
    /// The `IDIV_PD` instruction.
    pub const IDIV_PD: Opcode = 75;
    /// The `IDIV_PI` instruction.
    pub const IDIV_PI: Opcode = 76;
    /// The `IDIV_P` instruction.
    pub const IDIV_P: Opcode = 77;
    /// The `MUL` instruction.
    pub const MUL: Opcode = 78;
    /// The `MUL_PD` instruction.
    pub const MUL_PD: Opcode = 79;
    /// The `MUL_PI` instruction.
    pub const MUL_PI: Opcode = 80;
    /// The `MUL_P` instruction.
    pub const MUL_P: Opcode = 81;

    /// The `BNOT` instruction.
    pub const BNOT: Opcode = 82;
    /// The `TYPEOF` instruction.
    pub const TYPEOF: Opcode = 83;
    /// The `TYPEOFD` instruction.
    pub const TYPEOFD: Opcode = 84;
    /// The `TYPEOFI` instruction.
    pub const TYPEOFI: Opcode = 85;
    /// The `EVAL` instruction.
    pub const EVAL: Opcode = 86;
    /// The `EEXP` instruction.
    pub const EEXP: Opcode = 87;
    /// The `CHKINS` instruction.
    pub const CHKINS: Opcode = 88;
    /// The `ASC` instruction.
    pub const ASC: Opcode = 89;
    /// The `CHR` instruction.
    pub const CHR: Opcode = 90;
    /// The `NUM` instruction.
    pub const NUM: Opcode = 91;
    /// The `CHS` instruction.
    pub const CHS: Opcode = 92;
    /// The `INV` instruction.
    pub const INV: Opcode = 93;
    /// The `CHKINV` instruction.
    pub const CHKINV: Opcode = 94;
    /// The `INT` instruction.
    pub const INT: Opcode = 95;
    /// The `REAL` instruction.
    pub const REAL: Opcode = 96;
    /// The `STR` instruction.
    pub const STR: Opcode = 97;
    /// The `OCTET` instruction.
    pub const OCTET: Opcode = 98;
    /// The `CALL` instruction.
    pub const CALL: Opcode = 99;
    /// The `CALLD` instruction.
    pub const CALLD: Opcode = 100;
    /// The `CALLI` instruction.
    pub const CALLI: Opcode = 101;
    /// The `NEW` instruction.
    pub const NEW: Opcode = 102;
    /// The `GPD` instruction.
    pub const GPD: Opcode = 103;
    /// The `SPD` instruction.
    pub const SPD: Opcode = 104;
    /// The `SPDE` instruction.
    pub const SPDE: Opcode = 105;
    /// The `SPDEH` instruction.
    pub const SPDEH: Opcode = 106;
    /// The `GPI` instruction.
    pub const GPI: Opcode = 107;
    /// The `SPI` instruction.
    pub const SPI: Opcode = 108;
    /// The `SPIE` instruction.
    pub const SPIE: Opcode = 109;
    /// The `GPDS` instruction.
    pub const GPDS: Opcode = 110;
    /// The `SPDS` instruction.
    pub const SPDS: Opcode = 111;
    /// The `GPIS` instruction.
    pub const GPIS: Opcode = 112;
    /// The `SPIS` instruction.
    pub const SPIS: Opcode = 113;
    /// The `SETP` instruction.
    pub const SETP: Opcode = 114;
    /// The `GETP` instruction.
    pub const GETP: Opcode = 115;
    /// The `DELD` instruction.
    pub const DELD: Opcode = 116;
    /// The `DELI` instruction.
    pub const DELI: Opcode = 117;
    /// The `SRV` instruction.
    pub const SRV: Opcode = 118;
    /// The `RET` instruction.
    pub const RET: Opcode = 119;
    /// The `ENTRY` instruction.
    pub const ENTRY: Opcode = 120;
    /// The `EXTRY` instruction.
    pub const EXTRY: Opcode = 121;
    /// The `THROW` instruction.
    pub const THROW: Opcode = 122;
    /// The `CHGTHIS` instruction.
    pub const CHGTHIS: Opcode = 123;
    /// The `GLOBAL` instruction.
    pub const GLOBAL: Opcode = 124;
    /// The `ADDCI` instruction.
    pub const ADDCI: Opcode = 125;
    /// The `REGMEMBER` instruction.
    pub const REGMEMBER: Opcode = 126;
    /// The `DEBUGGER` instruction.
    pub const DEBUGGER: Opcode = 127;

    /// The mark after the last real opcode.
    /// The `LAST` instruction.
    pub const LAST: Opcode = 128;
}

/// The size of a variant, in bytes.
///
/// The register address multiplies a register number by the size of a
/// variant, and a variant is 20 bytes on the platforms the VM runs on:
/// an eight-byte pointer, a four-byte type, and an eight-byte payload.
pub const VARIANT_SIZE: i32 = 20;

/// The address of a register, in the units the code stream uses.
///
/// The address is the register number times the size of a variant.
pub fn vm_reg_addr(register: i32) -> i32 {
    register * VARIANT_SIZE
}

/// The register an address names.
pub fn vm_reg_from_addr(address: i32) -> i32 {
    address / VARIANT_SIZE
}

/// The address of a code position, in the units the code stream uses.
///
/// The address is the code position times the size of a word.
pub fn vm_code_addr(position: i32) -> i32 {
    position * 4
}

/// The code position an address names.
pub fn vm_code_from_addr(address: i32) -> i32 {
    address / 4
}

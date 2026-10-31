//! The VM's opcode table.

/// The opcode names, in the order the code generator numbers them.
pub const NAMES: [&str; 128] = [
    "nop",
    "const",
    "cp",
    "cl",
    "ccl",
    "tt",
    "tf",
    "ceq",
    "cdeq",
    "clt",
    "cgt",
    "setf",
    "setnf",
    "lnot",
    "nf",
    "jf",
    "jnf",
    "jmp",
    "inc",
    "incpd",
    "incpi",
    "incp",
    "dec",
    "decpd",
    "decpi",
    "decp",
    "lor",
    "lorpd",
    "lorpi",
    "lorp",
    "land",
    "landpd",
    "landpi",
    "landp",
    "bor",
    "borpd",
    "borpi",
    "borp",
    "bxor",
    "bxorpd",
    "bxorpi",
    "bxorp",
    "band",
    "bandpd",
    "bandpi",
    "bandp",
    "sar",
    "sarpd",
    "sarpi",
    "sarp",
    "sal",
    "salpd",
    "salpi",
    "salp",
    "sr",
    "srpd",
    "srpi",
    "srp",
    "add",
    "addpd",
    "addpi",
    "addp",
    "sub",
    "subpd",
    "subpi",
    "subp",
    "mod",
    "modpd",
    "modpi",
    "modp",
    "div",
    "divpd",
    "divpi",
    "divp",
    "idiv",
    "idivpd",
    "idivpi",
    "idivp",
    "mul",
    "mulpd",
    "mulpi",
    "mulp",
    "bnot",
    "typeof",
    "typeofd",
    "typeofi",
    "eval",
    "eexp",
    "chkins",
    "asc",
    "chr",
    "num",
    "chs",
    "inv",
    "chkinv",
    "int",
    "real",
    "str",
    "octet",
    "call",
    "calld",
    "calli",
    "new",
    "gpd",
    "spd",
    "spde",
    "spdeh",
    "gpi",
    "spi",
    "spie",
    "gpds",
    "spds",
    "gpis",
    "spis",
    "setp",
    "getp",
    "deld",
    "deli",
    "srv",
    "ret",
    "entry",
    "extry",
    "throw",
    "chgthis",
    "global",
    "addci",
    "regmember",
    "debugger",
];

/// The opcodes that open a four-way group.
///
/// Every group holds the plain form first, then the property-dispatch, the
/// property-invoke and the property forms.
const GROUP_BASES: [&str; 16] = [
    "inc", "dec", "lor", "land", "bor", "bxor", "band", "sar", "sal", "sr", "add", "sub", "mod",
    "div", "idiv", "mul",
];

/// The opcode number of `name`.
pub const fn opcode(name: &str) -> u16 {
    let mut index = 0usize;
    while index < NAMES.len() {
        if name_eq(NAMES[index], name) {
            return index as u16;
        }
        index += 1;
    }
    panic!("the opcode table has no such name")
}

/// Whether two names are the same.
const fn name_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0usize;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// The name of an opcode, when the table has one.
pub fn name_of(value: u16) -> Option<&'static str> {
    let index = usize::from(value);
    NAMES.get(index).copied()
}

/// The group an opcode belongs to, and the variant inside it.
pub fn group(value: u16) -> Option<(&'static str, u16)> {
    for (index, base) in GROUP_BASES.iter().enumerate() {
        let base_value = opcode(base);
        if value >= base_value && value < base_value + 4 {
            return Some((GROUP_BASES[index], value - base_value));
        }
    }
    None
}

/// The plain `nop`.
pub const NOP: u16 = opcode("nop");
/// The constant load.
pub const CONST: u16 = opcode("const");
/// The register copy.
pub const CP: u16 = opcode("cp");
/// The class load.
pub const CL: u16 = opcode("cl");
/// The class-array load.
pub const CCL: u16 = opcode("ccl");
/// The truth test.
pub const TT: u16 = opcode("tt");
/// The false test.
pub const TF: u16 = opcode("tf");
/// The equality test.
pub const CEQ: u16 = opcode("ceq");
/// The inequality test.
pub const CDEQ: u16 = opcode("cdeq");
/// The less-than test.
pub const CLT: u16 = opcode("clt");
/// The greater-than test.
pub const CGT: u16 = opcode("cgt");
/// The flag set.
pub const SETF: u16 = opcode("setf");
/// The negated flag set.
pub const SETNF: u16 = opcode("setnf");
/// The logical not.
pub const LNOT: u16 = opcode("lnot");
/// The "no value" marker.
pub const NF: u16 = opcode("nf");
/// The conditional jump.
pub const JF: u16 = opcode("jf");
/// The negated conditional jump.
pub const JNF: u16 = opcode("jnf");
/// The unconditional jump.
pub const JMP: u16 = opcode("jmp");
/// The bitwise not.
pub const BNOT: u16 = opcode("bnot");
/// The type name.
pub const TYPEOF: u16 = opcode("typeof");
/// The property type name.
pub const TYPEOFD: u16 = opcode("typeofd");
/// The indexed type name.
pub const TYPEOFI: u16 = opcode("typeofi");
/// The evaluator.
pub const EVAL: u16 = opcode("eval");
/// The expression evaluator.
pub const EEXP: u16 = opcode("eexp");
/// The instance-of test.
pub const CHKINS: u16 = opcode("chkins");
/// The character code.
pub const ASC: u16 = opcode("asc");
/// The character.
pub const CHR: u16 = opcode("chr");
/// The numeric value.
pub const NUM: u16 = opcode("num");
/// The character string.
pub const CHS: u16 = opcode("chs");
/// The inverse.
pub const INV: u16 = opcode("inv");
/// The negated inverse.
pub const CHKINV: u16 = opcode("chkinv");
/// The integer value.
pub const INT: u16 = opcode("int");
/// The real value.
pub const REAL: u16 = opcode("real");
/// The string value.
pub const STR: u16 = opcode("str");
/// The octet value.
pub const OCTET: u16 = opcode("octet");
/// The call.
pub const CALL: u16 = opcode("call");
/// The delegated call.
pub const CALLD: u16 = opcode("calld");
/// The invoked call.
pub const CALLI: u16 = opcode("calli");
/// The construction.
pub const NEW: u16 = opcode("new");
/// The property get.
pub const GPD: u16 = opcode("gpd");
/// The property set.
pub const SPD: u16 = opcode("spd");
/// The property set with a deletion.
pub const SPDE: u16 = opcode("spde");
/// The property set with a deletion and a hidden flag.
pub const SPDEH: u16 = opcode("spdeh");
/// The indexed property get.
pub const GPI: u16 = opcode("gpi");
/// The indexed property set.
pub const SPI: u16 = opcode("spi");
/// The indexed property set with an expansion.
pub const SPIE: u16 = opcode("spie");
/// The delegated property get.
pub const GPDS: u16 = opcode("gpds");
/// The delegated property set.
pub const SPDS: u16 = opcode("spds");
/// The delegated indexed property get.
pub const GPIS: u16 = opcode("gpis");
/// The delegated indexed property set.
pub const SPIS: u16 = opcode("spis");
/// The property set.
pub const SETP: u16 = opcode("setp");
/// The property get.
pub const GETP: u16 = opcode("getp");
/// The delegated property delete.
pub const DELD: u16 = opcode("deld");
/// The indexed property delete.
pub const DELI: u16 = opcode("deli");
/// The super-class call.
pub const SRV: u16 = opcode("srv");
/// The return.
pub const RET: u16 = opcode("ret");
/// The protected-block entry.
pub const ENTRY: u16 = opcode("entry");
/// The protected-block exit.
pub const EXTRY: u16 = opcode("extry");
/// The throw.
pub const THROW: u16 = opcode("throw");
/// The `this` change.
pub const CHGTHIS: u16 = opcode("chgthis");
/// The global access.
pub const GLOBAL: u16 = opcode("global");
/// The class-member addition.
pub const ADDCI: u16 = opcode("addci");
/// The member registration.
pub const REGMEMBER: u16 = opcode("regmember");
/// The debugger break.
pub const DEBUGGER: u16 = opcode("debugger");

#[cfg(test)]
mod tests {
    use super::*;

    /// The table numbers the opcodes in the order the code generator lists
    /// them, and the four-way groups follow the plain opcodes.
    #[test]
    fn the_table_numbers_the_opcodes_in_order() {
        assert_eq!(NAMES.len(), 128);
        assert_eq!(opcode("nop"), 0);
        assert_eq!(opcode("const"), 1);
        assert_eq!(opcode("jmp"), 17);
        assert_eq!(opcode("inc"), 18);
        assert_eq!(opcode("dec"), 22);
        assert_eq!(opcode("mul"), 78);
        assert_eq!(opcode("bnot"), 82);
        assert_eq!(opcode("call"), 99);
        assert_eq!(opcode("debugger"), 127);
    }

    /// Every name in the table is the name of its own number.
    #[test]
    fn every_name_matches_its_number() {
        for (index, name) in NAMES.iter().enumerate() {
            assert_eq!(opcode(name), index as u16, "{name}");
        }
    }

    /// The four-way groups hold the plain form first, then the
    /// property-dispatch, the property-invoke and the property forms.
    #[test]
    fn the_four_way_groups_hold_four_opcodes_each() {
        assert_eq!(group(opcode("inc")), Some(("inc", 0)));
        assert_eq!(group(opcode("incpd")), Some(("inc", 1)));
        assert_eq!(group(opcode("incpi")), Some(("inc", 2)));
        assert_eq!(group(opcode("incp")), Some(("inc", 3)));
        assert_eq!(group(opcode("add")), Some(("add", 0)));
        assert_eq!(group(opcode("addp")), Some(("add", 3)));
        assert_eq!(group(opcode("const")), None);
    }
}

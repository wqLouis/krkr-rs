# `tjs::vm::parser` — the parser

Source: `crates/tjs/src/vm/parser/` · module `tjs::vm::parser` · L4

The parser turns the analyser's token stream into the expression tree
the code generator walks. It is the second of the three Rust script
columns (the lexer is the first, the code generator the third); the
column it replaces is the grammar in
`reference/krkr2/cpp/core/tjs2/bison/tjs.y` and the parser it feeds in
`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp`.

The parser makes the **same nodes** the generator walks: an opcode, a
place in the script, a value and a list of children
(`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:178-236`,
`tTJSExprNode`). The tree is the parser's own type
(`crates/tjs/src/vm/codegen/node.rs`), so the tree is never copied
across the boundary between the two columns.

## 1. What the parser does

The parser follows the grammar's productions: the same expressions, the
same statements, the same commands to the generator. The commands are
the generator's, and the parser reaches them through the `Commands`
trait — the trait is the command surface the grammar needs, and the
generator's own wrapper is one implementation of it.

Two things are worth stating plainly, because both are easy to get
wrong:

* **The place of a node is the analyser's own place**, counted in code
  units from the start of the script, and it is the place the generator
  gives a node (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:25`,
  `LEX_POS`). The place therefore depends on when the parser reads the
  token that follows the expression: the parser reads the lookahead
  before it reduces a rule whose last symbol is an expression, and it
  reduces without a lookahead when the rule's last symbol is a token.
* **The node building folds the constant expressions**, the way the
  generator's own node building does
  (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3812-3985`): the
  folding rules are the generator's, and the parser uses them.

## 2. The rules the parser implements

Every rule below is the grammar's own behaviour, with the place it is
defined. The rules are what the column is written against; the tests
that prove them cite the same lines.

### 2.1 The grammar's productions — `parser/mod.rs`

| rule | reference |
|---|---|
| the parser follows the grammar's productions: the same expressions, the same statements, the same commands to the generator | `reference/krkr2/cpp/core/tjs2/bison/tjs.y` (the grammar) |
| the place of a node is the analyser's own place, counted in code units from the start of the script | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:25` (`LEX_POS`) |
| the parser keeps the generator's place current where it reads a token, and that place is the analyser's read cursor - the place just past the last token read | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1656-1658` (`Current - Script`) |
| the lookahead is read before a rule whose last symbol is an expression is reduced, and not before a rule whose last symbol is a token | `reference/krkr2/cpp/core/tjs2/bison/tjs.y` (the grammar's lookahead discipline) |
| the names of the commands are the generator's names | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:216-861` (the grammar's actions) |
| an argument is made a node only when it is an expression; the empty argument and the expanding argument are made when the list is reduced | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:769-775` |
| a function expression holds the function it made | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:648-657` |
| the recovery of a failure: the tokens are discarded up to the `;`, and the parse goes on | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:227-231` (the `def_list error ";"` rule) |
| the same recovery, at the place that discards the tokens | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:227-231` |
| the types are ignored | `reference/krkr2/cpp/core/tjs2/bison/tjs.y` (the grammar has no type rules) |
| a multiplication takes the operand that follows it, and not the whole expression on its right | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:596-604` |
| the star that ends an argument is told apart from a multiplication by the token that follows the star | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:772` |
| the same token tells the multiplication from the star that ends an argument | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:772` |
| the cast reads the type that follows the parenthesis, so the parser reads the token that follows it too | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:614-619` |
| the cast reads the type that follows the parenthesis, and the parenthesis that follows the type | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:614-619` |
| a call takes a whole priority expression on its left | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:757-758` |
| the token that follows the argument is read before the node that holds it is made | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:769-775` |
| the token that follows the argument is read before the argument is reduced, so the node the argument makes is placed after that token | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:769-775` |
| the token that follows the name tells the two forms of the element apart | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:803-809` |
| the name and the value of an element are told apart by the `:` that follows the name | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:803-809` |
| the constant inline array and the constant inline dictionary: the `(const)` form, which is the one a live object is built for | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:796-806`, `:831-841` |

### 2.2 The tree — `parser/nodes.rs`

| rule | reference |
|---|---|
| the node building folds the constant expressions | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3812-3985` (`MakeNP1`, `MakeNP2`) |
| a child may be absent: the grammar makes nodes with a null child — the empty call argument and the empty array element | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:116-121` (`Add`) |
| the nodes are kept in the order they were made | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3805-3814` |
| the node with an opcode and a place | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3805-3814` (`MakeNP0`) |
| the node with one child folds when the child is a constant | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3817-3890` (`MakeNP1`) |
| the node with two children folds when both children are constants | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3893-3985` (`MakeNP2`) |
| a node with three children is not folded | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3988-4004` (`MakeNP3`) |
| the value of a node | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:97-101` (`SetValue`) |
| the opcode of a node | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:93` (`SetOpcode`) |
| the child of a node | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:116-121` (`Add`) |

## 3. The proofs the column still owes

* The corpus comparison: the grammar's tree and the parser's, node for
  node, over the game's own scripts.
* The tests that cite the rules above: each rule in §2 wants a test that
  cites its line. The column's tests are owed; the rules are the
  specification they are written against.

## 4. The module map

| page | covers |
|---|---|
| [`columns.md`](columns.md) | the three script columns and the seam between them |
| [`generator.md`](generator.md) | the code generator and its rules |
| [`parser.md`](parser.md) | this page: the parser and the grammar's productions |
| [`seam.md`](seam.md), [`seam-fields.md`](seam-fields.md) | the compile entry, the C ABI and the field-by-field table of the script block image |

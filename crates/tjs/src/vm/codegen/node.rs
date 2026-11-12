//! The expression tree the generator walks.
//!
//! A node is an opcode, a place in the script, a value and a list of
//! children. The generator walks the tree and emits the intermediate
//! code; the tree itself is the parser's, and the parser builds it with
//! the shape the generator walks.
//!
//! The nodes are held behind a shared handle, so the tree is a graph of
//! shared nodes rather than a tree of owned ones.

use std::rc::Rc;

use super::value::Value;

/// An expression node.
#[derive(Clone, Debug)]
pub struct Node {
    /// The opcode of the node.
    pub opcode: i32,
    /// The place of the node in the script, counted in code units.
    pub position: i32,
    /// The value of the node, when it has one.
    pub value: Option<Value>,
    /// The children of the node.
    pub children: Vec<Option<Rc<Node>>>,
}

impl Node {
    /// A node with an opcode and a place.
    pub fn new(opcode: i32, position: i32) -> Rc<Node> {
        Rc::new(Self {
            opcode,
            position,
            value: None,
            children: Vec::new(),
        })
    }

    /// A node with an opcode, a place and a value.
    pub fn with_value(opcode: i32, position: i32, value: Value) -> Rc<Node> {
        Rc::new(Self {
            opcode,
            position,
            value: Some(value),
            children: Vec::new(),
        })
    }

    /// The number of children the node has.
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Whether the node has no children.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// The child at the given place.
    pub fn child(&self, index: usize) -> Option<&Rc<Node>> {
        self.children.get(index).and_then(Option::as_ref)
    }

    /// The value of the node, when it has one.
    pub fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }

    /// Whether the node is a constant.
    pub fn is_const(&self) -> bool {
        self.opcode == super::token::T_CONSTVAL
    }
}

/// Adds a child to a node.
///
/// The node is shared, so the child is added through a copy-on-write
/// handle.
pub fn add_child(node: &mut Rc<Node>, child: Rc<Node>) {
    Rc::make_mut(node).children.push(Some(child));
}

/// Sets the value of a node.
pub fn set_value(node: &mut Rc<Node>, value: Value) {
    Rc::make_mut(node).value = Some(value);
}

/// Sets the opcode of a node.
pub fn set_opcode(node: &mut Rc<Node>, opcode: i32) {
    Rc::make_mut(node).opcode = opcode;
}

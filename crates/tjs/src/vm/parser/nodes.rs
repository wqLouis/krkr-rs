//! The tree the parser builds.
//!
//! A node is the generator's node: an opcode, a place in the script, a
//! value and a list of children. The parser makes the nodes the grammar's
//! actions make, and the generator walks them.
//!
//! The node building folds the constant expressions: the folding rules
//! are the generator's, and the parser uses them.
//!
//! A child may be absent: the grammar makes nodes with a null child — the
//! empty call argument and the empty array element.

use std::rc::Rc;

use crate::vm::codegen::folding;
use crate::vm::codegen::node::{self, Node};
use crate::vm::codegen::token;
use crate::vm::codegen::value::Value;

/// The nodes the parser made.
///
/// The nodes are kept in the order they were made.
#[derive(Clone, Debug, Default)]
pub struct Nodes {
    nodes: Vec<Rc<Node>>,
}

impl Nodes {
    /// The nodes that were made.
    pub fn all(&self) -> &[Rc<Node>] {
        &self.nodes
    }

    /// The number of nodes that were made.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether no node was made.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The node at the given place.
    pub fn node(&self, index: usize) -> Rc<Node> {
        self.nodes[index].clone()
    }

    /// The node at the given place, for a change.
    pub fn node_mut(&mut self, index: usize) -> &mut Rc<Node> {
        &mut self.nodes[index]
    }

    /// Keeps a node and names its place.
    fn keep(&mut self, node: Rc<Node>) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// Makes a node with an opcode and a place.
    pub fn make_np0(&mut self, opcode: i32, position: i32) -> usize {
        self.keep(Node::new(opcode, position))
    }

    /// Makes a node with an opcode, a place and one child.
    ///
    /// The expression folds when the child is a constant.
    pub fn make_np1(&mut self, opcode: i32, position: i32, child: Option<&Rc<Node>>) -> usize {
        if let Some(folded) = fold_unary(opcode, child) {
            return self.keep(Node::with_value(token::T_CONSTVAL, position, folded));
        }
        let mut node = Node::new(opcode, position);
        Rc::make_mut(&mut node).children.push(child.cloned());
        self.keep(node)
    }

    /// Makes a node with an opcode, a place and two children.
    ///
    /// The expression folds when both children are constants.
    pub fn make_np2(
        &mut self,
        opcode: i32,
        position: i32,
        first: Option<&Rc<Node>>,
        second: Option<&Rc<Node>>,
    ) -> usize {
        if let Some(folded) = fold_binary(opcode, first, second) {
            return self.keep(Node::with_value(token::T_CONSTVAL, position, folded));
        }
        let mut node = Node::new(opcode, position);
        {
            let children = &mut Rc::make_mut(&mut node).children;
            children.push(first.cloned());
            children.push(second.cloned());
        }
        self.keep(node)
    }

    /// Makes a node with an opcode, a place and three children.
    ///
    /// A node with three children is not folded.
    pub fn make_np3(
        &mut self,
        opcode: i32,
        position: i32,
        first: Option<&Rc<Node>>,
        second: Option<&Rc<Node>>,
        third: Option<&Rc<Node>>,
    ) -> usize {
        let mut node = Node::new(opcode, position);
        {
            let children = &mut Rc::make_mut(&mut node).children;
            children.push(first.cloned());
            children.push(second.cloned());
            children.push(third.cloned());
        }
        self.keep(node)
    }

    /// Gives a node a value.
    pub fn set_value(&mut self, index: usize, value: Value) {
        node::set_value(&mut self.nodes[index], value);
    }

    /// Gives a node an opcode.
    pub fn set_opcode(&mut self, index: usize, opcode: i32) {
        node::set_opcode(&mut self.nodes[index], opcode);
    }

    /// Adds a child to a node.
    pub fn add_child(&mut self, index: usize, child: Rc<Node>) {
        node::add_child(&mut self.nodes[index], child);
    }
}

/// The folded value of a unary operation, when it folds.
fn fold_unary(opcode: i32, child: Option<&Rc<Node>>) -> Option<Value> {
    let child = child?;
    if child.opcode != token::T_CONSTVAL {
        return None;
    }
    let value = child.value()?;
    folding::fold_unary(opcode, value)
}

/// The folded value of a binary operation, when it folds.
fn fold_binary(opcode: i32, first: Option<&Rc<Node>>, second: Option<&Rc<Node>>) -> Option<Value> {
    let first = first?;
    let second = second?;
    if first.opcode != token::T_CONSTVAL || second.opcode != token::T_CONSTVAL {
        return None;
    }
    let left = first.value()?;
    let right = second.value()?;
    folding::fold_binary(opcode, left, right)
}

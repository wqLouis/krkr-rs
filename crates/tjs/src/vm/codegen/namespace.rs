//! The compile-time name space of a context.
//!
//! The generator gives every local variable a register. The name space
//! is what maps a name to that register: a name that is added gets the
//! next free register of the level it was added in, and a name that is
//! removed frees its register again — the register is not reused, so a
//! a name that is added, removed and added again gets a *new* register.

/// The symbols of one level of the name space.
///
/// A removed symbol leaves a hole: the list keeps its place so that the
/// registers of the symbols after it do not move.
#[derive(Clone, Debug, Default)]
pub struct SymbolList {
    /// The name of each symbol; a removed symbol has no name.
    symbols: Vec<Option<String>>,
    /// The number of local variables the level started with.
    local_count_start: i32,
}

impl SymbolList {
    /// A level that starts after `local_count_start` local variables.
    pub fn new(local_count_start: i32) -> Self {
        Self {
            symbols: Vec::new(),
            local_count_start,
        }
    }

    /// The number of local variables the level started with.
    pub fn local_count_start(&self) -> i32 {
        self.local_count_start
    }

    /// The number of symbols the level holds.
    ///
    /// The count includes the symbols that were removed, because their
    /// registers are still taken.
    pub fn count(&self) -> i32 {
        self.symbols.len() as i32
    }

    /// The place of a name in the level, or `-1` when it is not there.
    pub fn find(&self, name: &str) -> i32 {
        for (index, symbol) in self.symbols.iter().enumerate() {
            if let Some(symbol) = symbol
                && symbol == name
            {
                return index as i32;
            }
        }
        -1
    }

    /// Adds a name to the level.
    ///
    /// A name that is already there is not added again. A name that was
    /// removed before takes the hole it left.
    pub fn add(&mut self, name: &str) {
        if self.find(name) != -1 {
            return;
        }
        for symbol in self.symbols.iter_mut() {
            if symbol.is_none() {
                *symbol = Some(name.to_string());
                return;
            }
        }
        self.symbols.push(Some(name.to_string()));
    }

    /// Removes a name from the level.
    pub fn remove(&mut self, name: &str) {
        let index = self.find(name);
        if index != -1 {
            self.symbols[index as usize] = None;
        }
    }
}

/// The name space of a context.
#[derive(Clone, Debug, Default)]
pub struct LocalNamespace {
    /// The levels of the name space.
    levels: Vec<SymbolList>,
    /// The greatest number of local variables the name space held.
    pub max_count: i32,
}

impl LocalNamespace {
    /// A name space with no level.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of local variables the name space holds.
    pub fn count(&self) -> i32 {
        self.levels.iter().map(SymbolList::count).sum()
    }

    /// The place of a name in the name space, or `-1` when it is not there.
    pub fn find(&self, name: &str) -> i32 {
        for level in self.levels.iter().rev() {
            let index = level.find(name);
            if index != -1 {
                return index + level.local_count_start();
            }
        }
        -1
    }

    /// The depth of the name space.
    pub fn level(&self) -> i32 {
        self.levels.len() as i32
    }

    /// Adds a name to the top level.
    ///
    /// A name space with no level is the global one; the name goes
    /// nowhere.
    pub fn add(&mut self, name: &str) {
        let Some(top) = self.levels.last_mut() else {
            return;
        };
        top.add(name);
    }

    /// Removes a name from the level that holds it.
    pub fn remove(&mut self, name: &str) {
        for level in self.levels.iter_mut().rev() {
            if level.find(name) != -1 {
                level.remove(name);
                return;
            }
        }
    }

    /// Records the current count as a candidate for the greatest count.
    pub fn commit(&mut self) {
        let count = self.count();
        if self.max_count < count {
            self.max_count = count;
        }
    }

    /// Opens a level.
    pub fn push(&mut self) {
        let count = self.count();
        self.levels.push(SymbolList::new(count));
    }

    /// Closes the top level.
    pub fn pop(&mut self) {
        if self.levels.is_empty() {
            return;
        }
        // the count is taken with the level still there: the variables of
        // the level took their registers, so they are counted
        self.commit();
        self.levels.pop();
    }

    /// Closes every level.
    pub fn clear(&mut self) {
        while !self.levels.is_empty() {
            self.pop();
        }
    }

    /// The names of the local variables, in the order the name space
    /// holds them.
    pub fn variable_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for level in &self.levels {
            names.extend(level.symbols.iter().flatten().cloned());
        }
        names
    }
}

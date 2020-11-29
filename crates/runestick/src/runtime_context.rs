use crate::collections::HashMap;
use crate::context::Handler;
use crate::{Hash, Item, TypeCheck};
use std::fmt;
use std::sync::Arc;

/// Static run context visible to the virtual machine.
///
/// This contains:
/// * Declared functions.
/// * Declared instance functions.
/// * Built-in type checks.
#[derive(Default)]
pub struct RuntimeContext {
    /// Registered native function handlers.
    pub(crate) functions: HashMap<Hash, Arc<Handler>>,
    /// Registered types.
    pub(crate) types: HashMap<Hash, TypeCheck>,
}

impl RuntimeContext {
    /// Construct a new empty collection of functions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Use the specified type check.
    pub fn type_check_for(&self, item: &Item) -> Option<TypeCheck> {
        Some(*self.types.get(&Hash::type_hash(item))?)
    }

    /// Lookup the given native function handler in the context.
    pub fn lookup(&self, hash: Hash) -> Option<&Arc<Handler>> {
        self.functions.get(&hash)
    }
}

impl fmt::Debug for RuntimeContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RuntimeContext")
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeContext;

    fn assert_send_sync<T>()
    where
        T: Send + Sync,
    {
    }

    #[test]
    fn assert_thread_safe_context() {
        assert_send_sync::<RuntimeContext>();
    }
}

use crate::worker::{Import, WildcardImport};
use runestick::{CompileMod, SourceId};
use std::path::PathBuf;
use std::sync::Arc;

/// A single task that can be fed to the worker.
#[derive(Debug)]
pub(crate) enum Task {
    /// Load a file.
    LoadFile {
        /// The kind of loaded file.
        kind: LoadFileKind,
        /// The source id of the item being loaded.
        source_id: SourceId,
        /// The item of the file to load.
        mod_item: Arc<CompileMod>,
    },
    /// Expand a single import.
    ExpandImport(Import),
    /// Deferred action, since it requires all modules to be loaded to be able
    /// to discover all modules.
    ExpandWildcardImport(WildcardImport),
}

/// The kind of the loaded module.
#[derive(Debug)]
pub(crate) enum LoadFileKind {
    /// A root file, which determined a URL root.
    Root,
    /// A loaded module, which inherits its root from the file it was loaded
    /// from.
    Module { root: Option<PathBuf> },
}

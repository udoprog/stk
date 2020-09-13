use crate::ast;
use crate::collections::HashMap;
use crate::compile_visitor::NoopCompileVisitor;
use crate::error::CompileError;
use crate::error::CompileResult;
use crate::index_scopes::IndexScopes;
use crate::items::Items;
use crate::loops::Loops;
use crate::query::{Build, BuildEntry, Query};
use crate::scopes::{Scope, ScopeGuard, Scopes};
use crate::traits::Compile as _;
use crate::worker::{Expanded, IndexAst, Task, Worker};
use crate::{
    Assembly, CompileVisitor, LoadError, LoadErrorKind, Options, Resolve as _, Sources, Storage,
    UnitBuilder, Warnings,
};
use runestick::{
    CompileMeta, CompileMetaKind, Context, Inst, Item, Label, Source, Span, TypeCheck,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

/// A needs hint for an expression.
/// This is used to contextually determine what an expression is expected to
/// produce.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Needs {
    Type,
    Value,
    None,
}

impl Needs {
    /// Test if any sort of value is needed.
    pub(crate) fn value(self) -> bool {
        matches!(self, Self::Type | Self::Value)
    }
}

/// Compile the given source with default options.
pub fn compile(
    context: &Context,
    sources: &mut Sources,
    unit: &Rc<RefCell<UnitBuilder>>,
    warnings: &mut Warnings,
) -> Result<(), LoadError> {
    let mut visitor = NoopCompileVisitor::new();
    compile_with_options(
        context,
        sources,
        unit,
        warnings,
        &Default::default(),
        &mut visitor,
    )?;
    Ok(())
}

/// Encode the given object into a collection of asm.
pub fn compile_with_options(
    context: &Context,
    sources: &mut Sources,
    unit: &Rc<RefCell<UnitBuilder>>,
    warnings: &mut Warnings,
    options: &Options,
    visitor: &mut dyn CompileVisitor,
) -> Result<(), LoadError> {
    // Global storage.
    let storage = Storage::new();
    // Worker queue.
    let mut queue = VecDeque::new();

    while let Some((item, source_id)) = sources.next_source() {
        let source = match sources.get(source_id).cloned() {
            Some(source) => source,
            None => return Err(LoadError::internal("missing queued source by id")),
        };

        let file = match crate::parse_all::<ast::File>(source.as_str()) {
            Ok(file) => file,
            Err(error) => {
                return Err(LoadError::from(LoadErrorKind::ParseError {
                    source_id,
                    error,
                }))
            }
        };

        let items = Items::new(item.clone().into_vec());

        queue.push_back(Task::Index {
            item,
            items,
            source_id,
            source,
            scopes: IndexScopes::new(),
            impl_items: Default::default(),
            ast: IndexAst::File(file),
        });
    }

    // The worker queue.
    let mut worker = Worker::new(
        queue,
        context,
        sources,
        options,
        unit.clone(),
        warnings,
        storage.clone(),
    );

    worker.run()?;
    verify_imports(context, &mut *unit.borrow_mut())?;

    while let Some(entry) = worker.query.queue.pop_front() {
        let source_id = entry.source_id;

        if let Err(error) = compile_entry(CompileEntryArgs {
            context,
            options,
            storage: &storage,
            unit,
            warnings: worker.warnings,
            query: &mut worker.query,
            entry,
            expanded: &worker.expanded,
            visitor,
        }) {
            return Err(LoadError::from(LoadErrorKind::CompileError {
                source_id,
                error,
            }));
        }
    }

    Ok(())
}

struct CompileEntryArgs<'a> {
    context: &'a Context,
    options: &'a Options,
    storage: &'a Storage,
    unit: &'a Rc<RefCell<UnitBuilder>>,
    warnings: &'a mut Warnings,
    query: &'a mut Query,
    entry: BuildEntry,
    expanded: &'a HashMap<Item, Expanded>,
    visitor: &'a mut dyn CompileVisitor,
}

fn compile_entry(args: CompileEntryArgs<'_>) -> Result<(), CompileError> {
    let CompileEntryArgs {
        context,
        options,
        storage,
        unit,
        warnings,
        query,
        entry,
        expanded,
        visitor,
    } = args;

    let BuildEntry {
        item,
        build,
        source,
        source_id,
    } = entry;

    let mut asm = unit.borrow().new_assembly(source_id);

    let mut compiler = Compiler {
        storage,
        source_id,
        source: source.clone(),
        context,
        query,
        asm: &mut asm,
        items: Items::new(item.as_vec()),
        unit: unit.clone(),
        scopes: Scopes::new(),
        contexts: vec![],
        loops: Loops::new(),
        options,
        warnings,
        expanded,
        visitor,
    };

    match build {
        Build::Function(f) => {
            let args = format_fn_args(storage, &*source, f.ast.args.items.iter().map(|(a, _)| a))?;

            let span = f.ast.span();
            let count = f.ast.args.items.len();
            compiler.contexts.push(span);
            compiler.compile((f.ast, false))?;

            unit.borrow_mut()
                .new_function(source_id, item, count, asm, f.call, args)?;
        }
        Build::InstanceFunction(f) => {
            let args = format_fn_args(storage, &*source, f.ast.args.items.iter().map(|(a, _)| a))?;

            let span = f.ast.span();
            let count = f.ast.args.items.len();
            compiler.contexts.push(span);

            let source = compiler.source.clone();
            let name = f.ast.name.resolve(storage, &*source)?;

            let meta = compiler
                .lookup_meta(&f.impl_item, f.instance_span)?
                .ok_or_else(|| CompileError::MissingType {
                    span: f.instance_span,
                    item: f.impl_item.clone(),
                })?;

            let type_of =
                meta.type_of()
                    .ok_or_else(|| CompileError::UnsupportedInstanceFunction {
                        meta: meta.clone(),
                        span,
                    })?;

            compiler.compile((f.ast, true))?;

            unit.borrow_mut().new_instance_function(
                source_id,
                item,
                type_of,
                name.as_ref(),
                count,
                asm,
                f.call,
                args,
            )?;
        }
        Build::Closure(c) => {
            let args = format_fn_args(
                storage,
                &*source,
                c.ast.args.as_slice().iter().map(|(a, _)| a),
            )?;

            let span = c.ast.span();
            let count = c.ast.args.len();
            compiler.contexts.push(span);
            compiler.compile((c.ast, &c.captures[..]))?;

            unit.borrow_mut()
                .new_function(source_id, item, count, asm, c.call, args)?;
        }
        Build::AsyncBlock(async_block) => {
            let span = async_block.ast.span();
            let args = async_block.captures.len();
            compiler.contexts.push(span);
            compiler.compile((&async_block.ast, &async_block.captures[..]))?;

            unit.borrow_mut().new_function(
                source_id,
                item,
                args,
                asm,
                async_block.call,
                Vec::new(),
            )?;
        }
    }

    Ok(())
}

fn format_fn_args<'a, I>(
    storage: &Storage,
    source: &Source,
    arguments: I,
) -> Result<Vec<String>, CompileError>
where
    I: IntoIterator<Item = &'a ast::FnArg>,
{
    let mut args = Vec::new();

    for arg in arguments {
        match arg {
            ast::FnArg::Self_(..) => {
                args.push(String::from("self"));
            }
            ast::FnArg::Ignore(..) => {
                args.push(String::from("_"));
            }
            ast::FnArg::Ident(ident) => {
                args.push(ident.resolve(storage, source)?.to_string());
            }
        }
    }

    Ok(args)
}

fn verify_imports(context: &Context, unit: &mut UnitBuilder) -> Result<(), LoadError> {
    for (_, entry) in unit.iter_imports() {
        if context.contains_prefix(&entry.item) || unit.contains_prefix(&entry.item) {
            continue;
        }

        if let Some((span, source_id)) = entry.span {
            return Err(LoadError::from(LoadErrorKind::CompileError {
                error: CompileError::MissingModule {
                    span,
                    item: entry.item.clone(),
                },
                source_id,
            }));
        } else {
            return Err(LoadError::from(LoadErrorKind::CompileError {
                error: CompileError::MissingPreludeModule {
                    item: entry.item.clone(),
                },
                source_id: 0,
            }));
        }
    }

    Ok(())
}

pub(crate) struct Compiler<'a> {
    /// The source id of the source.
    pub(crate) source_id: usize,
    /// The source we are compiling for.
    pub(crate) source: Arc<Source>,
    /// The current macro context.
    pub(crate) storage: &'a Storage,
    /// The context we are compiling for.
    context: &'a Context,
    /// Items expanded by macros.
    pub(crate) expanded: &'a HashMap<Item, Expanded>,
    /// Query system to compile required items.
    pub(crate) query: &'a mut Query,
    /// The assembly we are generating.
    pub(crate) asm: &'a mut Assembly,
    /// Item builder.
    pub(crate) items: Items,
    /// The compilation unit we are compiling for.
    pub(crate) unit: Rc<RefCell<UnitBuilder>>,
    /// Scopes defined in the compiler.
    pub(crate) scopes: Scopes,
    /// Context for which to emit warnings.
    pub(crate) contexts: Vec<Span>,
    /// The nesting of loop we are currently in.
    pub(crate) loops: Loops,
    /// Enabled optimizations.
    pub(crate) options: &'a Options,
    /// Compilation warnings.
    pub(crate) warnings: &'a mut Warnings,
    /// Compiler visitor.
    pub(crate) visitor: &'a mut dyn CompileVisitor,
}

impl<'a> Compiler<'a> {
    /// Access the meta for the given language item.
    pub fn lookup_meta(&mut self, name: &Item, span: Span) -> CompileResult<Option<CompileMeta>> {
        log::trace!("lookup meta: {}", name);

        if let Some(meta) = self.context.lookup_meta(name) {
            log::trace!("found in context: {:?}", meta);
            self.visitor.visit_meta(&meta, span);
            return Ok(Some(meta));
        }

        let mut base = self.items.item();

        loop {
            let current = base.join(name);
            log::trace!("lookup meta (query): {}", current);

            if let Some(meta) = self.query.query_meta(&current, span)? {
                log::trace!("found in query: {:?}", meta);
                self.visitor.visit_meta(&meta, span);
                return Ok(Some(meta));
            }

            if base.pop().is_none() {
                break;
            }
        }

        Ok(None)
    }

    /// Pop locals by simply popping them.
    pub(crate) fn locals_pop(&mut self, total_var_count: usize, span: Span) {
        match total_var_count {
            0 => (),
            1 => {
                self.asm.push(Inst::Pop, span);
            }
            count => {
                self.asm.push(Inst::PopN { count }, span);
            }
        }
    }

    /// Clean up local variables by preserving the value that is on top and
    /// popping the rest.
    ///
    /// The clean operation will preserve the value that is on top of the stack,
    /// and pop the values under it.
    pub(crate) fn locals_clean(&mut self, total_var_count: usize, span: Span) {
        match total_var_count {
            0 => (),
            count => {
                self.asm.push(Inst::Clean { count }, span);
            }
        }
    }

    /// Compile an item.
    pub(crate) fn compile_meta(
        &mut self,
        meta: &CompileMeta,
        span: Span,
        needs: Needs,
    ) -> CompileResult<()> {
        log::trace!("CompileMeta => {:?} {:?}", meta, needs);
        if let Needs::Value = needs {
            match &meta.kind {
                CompileMetaKind::Tuple { tuple, .. } if tuple.args == 0 => {
                    self.asm.push_with_comment(
                        Inst::Call {
                            hash: tuple.hash,
                            args: 0,
                        },
                        span,
                        format!("tuple `{}`", tuple.item),
                    );
                }
                CompileMetaKind::TupleVariant {
                    enum_item, tuple, ..
                } if tuple.args == 0 => {
                    self.asm.push_with_comment(
                        Inst::Call {
                            hash: tuple.hash,
                            args: 0,
                        },
                        span,
                        format!("tuple variant `{}::{}`", enum_item, tuple.item),
                    );
                }
                CompileMetaKind::Tuple { tuple, .. } => {
                    self.asm.push_with_comment(
                        Inst::Fn { hash: tuple.hash },
                        span,
                        format!("tuple `{}`", tuple.item),
                    );
                }
                CompileMetaKind::TupleVariant {
                    enum_item, tuple, ..
                } => {
                    self.asm.push_with_comment(
                        Inst::Fn { hash: tuple.hash },
                        span,
                        format!("tuple variant `{}::{}`", enum_item, tuple.item),
                    );
                }
                CompileMetaKind::Function { type_of, item, .. } => {
                    let hash = **type_of;
                    self.asm
                        .push_with_comment(Inst::Fn { hash }, span, format!("fn `{}`", item));
                }
                _ => {
                    return Err(CompileError::UnsupportedValue {
                        span,
                        meta: meta.clone(),
                    });
                }
            }

            return Ok(());
        }

        let type_of = meta
            .type_of()
            .ok_or_else(|| CompileError::UnsupportedType {
                span,
                meta: meta.clone(),
            })?;

        let hash = *type_of;
        self.asm.push(Inst::Type { hash }, span);
        Ok(())
    }

    /// Convert a path to an item.
    pub(crate) fn convert_path_to_item(&self, path: &ast::Path) -> CompileResult<Item> {
        let base = self.items.item();
        self.unit
            .borrow()
            .convert_path(&base, path, &self.storage, &*self.source)
    }

    pub(crate) fn compile_condition(
        &mut self,
        condition: &ast::Condition,
        then_label: Label,
    ) -> CompileResult<Scope> {
        let span = condition.span();
        log::trace!("Condition => {:?}", self.source.source(span));

        match condition {
            ast::Condition::Expr(expr) => {
                let span = expr.span();

                self.compile((&**expr, Needs::Value))?;
                self.asm.jump_if(then_label, span);

                Ok(self.scopes.child(span)?)
            }
            ast::Condition::ExprLet(expr_let) => {
                let span = expr_let.span();

                let false_label = self.asm.new_label("if_condition_false");

                let mut scope = self.scopes.child(span)?;
                self.compile((&*expr_let.expr, Needs::Value))?;

                let load = |_: &mut Assembly| {};

                if self.compile_pat(&mut scope, &expr_let.pat, false_label, &load)? {
                    self.asm.jump(then_label, span);
                    self.asm.label(false_label)?;
                } else {
                    self.asm.jump(then_label, span);
                };

                Ok(scope)
            }
        }
    }

    /// Encode a vector pattern match.
    pub(crate) fn compile_pat_vec(
        &mut self,
        scope: &mut Scope,
        pat_vec: &ast::PatVec,
        false_label: Label,
        load: &dyn Fn(&mut Assembly),
    ) -> CompileResult<()> {
        let span = pat_vec.span();
        log::trace!("PatVec => {:?}", self.source.source(span));

        // Assign the yet-to-be-verified tuple to an anonymous slot, so we can
        // interact with it multiple times.
        load(&mut self.asm);
        let offset = scope.decl_anon(span);

        // Copy the temporary and check that its length matches the pattern and
        // that it is indeed a vector.
        self.asm.push(Inst::Copy { offset }, span);

        self.asm.push(
            Inst::MatchSequence {
                type_check: TypeCheck::Vec,
                len: pat_vec.items.len(),
                exact: pat_vec.open_pattern.is_none(),
            },
            span,
        );

        self.asm
            .pop_and_jump_if_not(scope.local_var_count, false_label, span);

        for (index, (pat, _)) in pat_vec.items.iter().enumerate() {
            let span = pat.span();

            let load = move |asm: &mut Assembly| {
                asm.push(Inst::TupleIndexGetAt { offset, index }, span);
            };

            self.compile_pat(scope, &*pat, false_label, &load)?;
        }

        Ok(())
    }

    /// Encode a vector pattern match.
    pub(crate) fn compile_pat_tuple(
        &mut self,
        scope: &mut Scope,
        pat_tuple: &ast::PatTuple,
        false_label: Label,
        load: &dyn Fn(&mut Assembly),
    ) -> CompileResult<()> {
        let span = pat_tuple.span();
        log::trace!("PatTuple => {:?}", self.source.source(span));

        // Assign the yet-to-be-verified tuple to an anonymous slot, so we can
        // interact with it multiple times.
        load(&mut self.asm);
        let offset = scope.decl_anon(span);

        let type_check = if let Some(path) = &pat_tuple.path {
            let item = self.convert_path_to_item(path)?;

            let (tuple, meta, type_check) =
                if let Some(meta) = self.lookup_meta(&item, path.span())? {
                    match &meta.kind {
                        CompileMetaKind::Tuple { tuple, type_of, .. } => {
                            let type_check = TypeCheck::Type(**type_of);
                            (tuple.clone(), meta, type_check)
                        }
                        CompileMetaKind::TupleVariant { tuple, type_of, .. } => {
                            let type_check = TypeCheck::Variant(**type_of);
                            (tuple.clone(), meta, type_check)
                        }
                        _ => return Err(CompileError::UnsupportedMetaPattern { meta, span }),
                    }
                } else {
                    return Err(CompileError::UnsupportedPattern { span });
                };

            let count = pat_tuple.items.len();
            let is_open = pat_tuple.open_pattern.is_some();

            if !(tuple.args == count || count < tuple.args && is_open) {
                return Err(CompileError::UnsupportedArgumentCount {
                    span,
                    meta,
                    expected: tuple.args,
                    actual: count,
                });
            }

            match self.context.type_check_for(&tuple.item) {
                Some(type_check) => type_check,
                None => type_check,
            }
        } else {
            TypeCheck::Tuple
        };

        self.asm.push(Inst::Copy { offset }, span);
        self.asm.push(
            Inst::MatchSequence {
                type_check,
                len: pat_tuple.items.len(),
                exact: pat_tuple.open_pattern.is_none(),
            },
            span,
        );
        self.asm
            .pop_and_jump_if_not(scope.local_var_count, false_label, span);

        for (index, (pat, _)) in pat_tuple.items.iter().enumerate() {
            let span = pat.span();

            let load = move |asm: &mut Assembly| {
                asm.push(Inst::TupleIndexGetAt { offset, index }, span);
            };

            self.compile_pat(scope, &*pat, false_label, &load)?;
        }

        Ok(())
    }

    /// Encode an object pattern match.
    pub(crate) fn compile_pat_object(
        &mut self,
        scope: &mut Scope,
        pat_object: &ast::PatObject,
        false_label: Label,
        load: &dyn Fn(&mut Assembly),
    ) -> CompileResult<()> {
        let span = pat_object.span();
        log::trace!("PatObject => {:?}", self.source.source(span));

        // NB: bind the loaded variable (once) to an anonymous var.
        // We reduce the number of copy operations by having specialized
        // operations perform the load from the given offset.
        load(&mut self.asm);
        let offset = scope.decl_anon(span);

        let mut string_slots = Vec::new();

        let mut keys_dup = HashMap::new();
        let mut keys = Vec::new();

        for (item, _) in &pat_object.fields {
            let span = item.span();

            let source = self.source.clone();
            let key = item.key.resolve(&self.storage, &*source)?;
            string_slots.push(self.unit.borrow_mut().new_static_string(&*key)?);
            keys.push(key.to_string());

            if let Some(existing) = keys_dup.insert(key.to_string(), span) {
                return Err(CompileError::DuplicateObjectKey {
                    span,
                    existing,
                    object: pat_object.span(),
                });
            }
        }

        let keys = self.unit.borrow_mut().new_static_object_keys(&keys[..])?;

        let type_check = match &pat_object.ident {
            ast::LitObjectIdent::Named(path) => {
                let span = path.span();
                let item = self.convert_path_to_item(path)?;

                let meta = match self.lookup_meta(&item, span)? {
                    Some(meta) => meta,
                    None => {
                        return Err(CompileError::MissingType { span, item });
                    }
                };

                let (object, type_check) = match &meta.kind {
                    CompileMetaKind::Struct {
                        object, type_of, ..
                    } => {
                        let type_check = TypeCheck::Type(**type_of);
                        (object, type_check)
                    }
                    CompileMetaKind::StructVariant {
                        object, type_of, ..
                    } => {
                        let type_check = TypeCheck::Variant(**type_of);
                        (object, type_check)
                    }
                    _ => {
                        return Err(CompileError::UnsupportedMetaPattern { meta, span });
                    }
                };

                let fields = match &object.fields {
                    Some(fields) => fields,
                    None => {
                        // NB: might want to describe that field composition is unknown because it is an external meta item.
                        return Err(CompileError::UnsupportedMetaPattern { meta, span });
                    }
                };

                for (field, _) in &pat_object.fields {
                    let span = field.key.span();
                    let key = field.key.resolve(&self.storage, &*self.source)?;

                    if !fields.contains(&*key) {
                        return Err(CompileError::LitObjectNotField {
                            span,
                            field: key.to_string(),
                            item: object.item.clone(),
                        });
                    }
                }

                type_check
            }
            ast::LitObjectIdent::Anonymous(..) => TypeCheck::Object,
        };

        // Copy the temporary and check that its length matches the pattern and
        // that it is indeed a vector.
        self.asm.push(Inst::Copy { offset }, span);
        self.asm.push(
            Inst::MatchObject {
                type_check,
                slot: keys,
                exact: pat_object.open_pattern.is_none(),
            },
            span,
        );

        self.asm
            .pop_and_jump_if_not(scope.local_var_count, false_label, span);

        for ((item, _), slot) in pat_object.fields.iter().zip(string_slots) {
            let span = item.span();

            let load = move |asm: &mut Assembly| {
                asm.push(Inst::ObjectSlotIndexGetAt { offset, slot }, span);
            };

            if let Some((_, pat)) = &item.binding {
                // load the given vector index and declare it as a local variable.
                self.compile_pat(scope, &*pat, false_label, &load)?;
                continue;
            }

            // NB: only raw identifiers are supported as anonymous bindings
            let ident = match &item.key {
                ast::LitObjectKey::Ident(ident) => ident,
                _ => return Err(CompileError::UnsupportedBinding { span }),
            };

            load(&mut self.asm);
            let name = ident.resolve(&self.storage, &*self.source)?;
            scope.decl_var(name.as_ref(), span);
        }

        Ok(())
    }

    /// Compile a binding name that matches a known meta type.
    ///
    /// Returns `true` if the binding was used.
    pub(crate) fn compile_pat_meta_binding(
        &mut self,
        scope: &mut Scope,
        span: Span,
        meta: &CompileMeta,
        false_label: Label,
        load: &dyn Fn(&mut Assembly),
    ) -> CompileResult<bool> {
        let (tuple, type_check) = match &meta.kind {
            CompileMetaKind::Tuple { tuple, type_of, .. } if tuple.args == 0 => {
                (tuple, TypeCheck::Type(**type_of))
            }
            CompileMetaKind::TupleVariant { tuple, type_of, .. } if tuple.args == 0 => {
                (tuple, TypeCheck::Variant(**type_of))
            }
            _ => return Ok(false),
        };

        let type_check = match self.context.type_check_for(&tuple.item) {
            Some(type_check) => type_check,
            None => type_check,
        };

        load(&mut self.asm);
        self.asm.push(
            Inst::MatchSequence {
                type_check,
                len: tuple.args,
                exact: true,
            },
            span,
        );
        self.asm
            .pop_and_jump_if_not(scope.local_var_count, false_label, span);
        Ok(true)
    }

    /// Encode a pattern.
    ///
    /// Patterns will clean up their own locals and execute a jump to
    /// `false_label` in case the pattern does not match.
    ///
    /// Returns a boolean indicating if the label was used.
    pub(crate) fn compile_pat(
        &mut self,
        scope: &mut Scope,
        pat: &ast::Pat,
        false_label: Label,
        load: &dyn Fn(&mut Assembly),
    ) -> CompileResult<bool> {
        let span = pat.span();
        log::trace!("Pat => {:?}", self.source.source(span));

        match pat {
            ast::Pat::PatPath(path) => {
                let span = path.span();

                let item = self.convert_path_to_item(&path.path)?;

                if let Some(meta) = self.lookup_meta(&item, span)? {
                    if self.compile_pat_meta_binding(scope, span, &meta, false_label, load)? {
                        return Ok(true);
                    }
                }

                let ident = match item.as_local() {
                    Some(ident) => ident,
                    None => {
                        return Err(CompileError::UnsupportedBinding { span });
                    }
                };

                load(&mut self.asm);
                scope.decl_var(&ident, span);
                return Ok(false);
            }
            ast::Pat::PatIgnore(..) => {
                return Ok(false);
            }
            ast::Pat::PatUnit(unit) => {
                load(&mut self.asm);
                self.asm.push(Inst::IsUnit, unit.span());
            }
            ast::Pat::PatByte(lit_byte) => {
                let byte = lit_byte.resolve(&self.storage, &*self.source)?;
                load(&mut self.asm);
                self.asm.push(Inst::EqByte { byte }, lit_byte.span());
            }
            ast::Pat::PatChar(lit_char) => {
                let character = lit_char.resolve(&self.storage, &*self.source)?;
                load(&mut self.asm);
                self.asm
                    .push(Inst::EqCharacter { character }, lit_char.span());
            }
            ast::Pat::PatNumber(number_literal) => {
                let span = number_literal.span();
                let number = number_literal.resolve(&self.storage, &*self.source)?;

                let integer = match number {
                    ast::Number::Integer(integer) => integer,
                    ast::Number::Float(..) => {
                        return Err(CompileError::MatchFloatInPattern { span });
                    }
                };

                load(&mut self.asm);
                self.asm.push(Inst::EqInteger { integer }, span);
            }
            ast::Pat::PatString(pat_string) => {
                let span = pat_string.span();
                let string = pat_string.resolve(&self.storage, &*self.source)?;
                let slot = self.unit.borrow_mut().new_static_string(&*string)?;
                load(&mut self.asm);
                self.asm.push(Inst::EqStaticString { slot }, span);
            }
            ast::Pat::PatVec(pat_vec) => {
                self.compile_pat_vec(scope, pat_vec, false_label, &load)?;
                return Ok(true);
            }
            ast::Pat::PatTuple(pat_tuple) => {
                self.compile_pat_tuple(scope, pat_tuple, false_label, &load)?;
                return Ok(true);
            }
            ast::Pat::PatObject(object) => {
                self.compile_pat_object(scope, object, false_label, &load)?;
                return Ok(true);
            }
        }

        self.asm
            .pop_and_jump_if_not(scope.local_var_count, false_label, span);
        Ok(true)
    }

    /// Clean the last scope.
    pub(crate) fn clean_last_scope(
        &mut self,
        span: Span,
        expected: ScopeGuard,
        needs: Needs,
    ) -> CompileResult<()> {
        let scope = self.scopes.pop(expected, span)?;

        if needs.value() {
            self.locals_clean(scope.local_var_count, span);
        } else {
            self.locals_pop(scope.local_var_count, span);
        }

        Ok(())
    }

    /// Get the latest relevant warning context.
    pub(crate) fn context(&self) -> Option<Span> {
        self.contexts.last().copied()
    }
}

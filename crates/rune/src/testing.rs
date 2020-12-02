//! Utilities used for testing small Rune programs.
//!
//! This module can be disabled through the `testing` feature.

pub use crate::WarningKind::*;
pub use crate::{CompileErrorKind, CompileErrorKind::*};
use crate::{Error, Errors, Sources, UnitBuilder, Warnings};
pub use crate::{ParseErrorKind, ParseErrorKind::*};
pub use crate::{QueryErrorKind, QueryErrorKind::*};
pub use crate::{ResolveErrorKind, ResolveErrorKind::*};
pub use futures_executor::block_on;
pub use runestick::VmErrorKind::*;
pub use runestick::{
    Bytes, CompileMeta, CompileMetaKind, ContextError, FromValue, Function, IntoComponent, Span,
    ToValue, Value, VecTuple, VmError,
};
use runestick::{Item, Source, Unit};
use std::sync::Arc;
use thiserror::Error;

/// An error that can be raised during testing.
#[derive(Debug, Error)]
pub enum RunError {
    /// A load error was raised during testing.
    #[error("load errors")]
    Errors(Errors),
    /// A virtual machine error was raised during testing.
    #[error("vm error")]
    VmError(#[source] VmError),
}

impl RunError {
    /// Unpack into a vm error or panic with the given message.
    pub fn expect_vm_error(self, msg: &str) -> VmError {
        match self {
            Self::VmError(error) => error,
            _ => panic!("{}", msg),
        }
    }
}

/// Compile the given source into a unit and collection of warnings.
pub fn compile_source(
    context: &runestick::Context,
    source: &str,
) -> Result<(Unit, Warnings), Errors> {
    let mut errors = Errors::new();
    let mut warnings = Warnings::new();
    let mut sources = Sources::new();
    sources.insert(Source::new("main", source.to_owned()));
    let unit = UnitBuilder::with_default_prelude();

    if let Err(()) = crate::compile(context, &mut sources, &unit, &mut errors, &mut warnings) {
        return Err(errors);
    }

    let unit = match unit.build() {
        Ok(unit) => unit,
        Err(error) => {
            errors.push(Error::new(0, error));
            return Err(errors);
        }
    };

    Ok((unit, warnings))
}

/// Construct a virtual machine for the given source.
pub fn vm(context: &runestick::Context, source: &str) -> Result<runestick::Vm, RunError> {
    let (unit, _) = compile_source(context, &source).map_err(RunError::Errors)?;
    let context = Arc::new(context.runtime());

    Ok(runestick::Vm::new(context, Arc::new(unit)))
}

/// Call the specified function in the given script.
pub async fn run_async<N, A, T>(
    context: &Arc<runestick::Context>,
    source: &str,
    function: N,
    args: A,
) -> Result<T, RunError>
where
    N: IntoIterator,
    N::Item: IntoComponent,
    A: runestick::Args,
    T: FromValue,
{
    let vm = vm(context, source)?;

    let output = vm
        .execute(&Item::with_item(function), args)
        .map_err(RunError::VmError)?
        .async_complete()
        .await
        .map_err(RunError::VmError)?;

    T::from_value(output).map_err(RunError::VmError)
}

/// Call the specified function in the given script.
pub fn run<N, A, T>(
    context: &Arc<runestick::Context>,
    source: &str,
    function: N,
    args: A,
) -> Result<T, RunError>
where
    N: IntoIterator,
    N::Item: IntoComponent,
    A: runestick::Args,
    T: runestick::FromValue,
{
    block_on(run_async(context, source, function, args))
}

/// Helper function to construct a context and unit from a Rune source for
/// testing purposes.
///
/// This is primarily used in examples.
pub fn build(
    context: &runestick::Context,
    source: &str,
) -> runestick::Result<Arc<runestick::Unit>> {
    let options = crate::Options::default();
    let mut sources = crate::Sources::new();
    sources.insert(runestick::Source::new("source", source));

    let mut warnings = crate::Warnings::new();
    let mut errors = crate::Errors::new();

    let unit = match crate::load_sources(
        &*context,
        &options,
        &mut sources,
        &mut errors,
        &mut warnings,
    ) {
        Ok(unit) => unit,
        Err(error) => {
            let mut writer =
                crate::termcolor::StandardStream::stderr(crate::termcolor::ColorChoice::Always);
            crate::EmitDiagnostics::emit_diagnostics(&errors, &mut writer, &sources)?;
            return Err(error.into());
        }
    };

    if !warnings.is_empty() {
        let mut writer =
            crate::termcolor::StandardStream::stderr(crate::termcolor::ColorChoice::Always);
        crate::EmitDiagnostics::emit_diagnostics(&warnings, &mut writer, &sources)?;
    }

    Ok(std::sync::Arc::new(unit))
}

/// Construct a rune virtual machine from the given program.
///
/// # Examples
///
/// ```rust
/// use rune::testing::*;
/// use runestick::Value;
///
/// # fn main() {
/// let vm = rune::rune_vm!(pub fn main() { true || false });
/// let result = vm.execute(&["main"], ()).unwrap().complete().unwrap();
/// assert_eq!(result.into_bool().unwrap(), true);
/// # }
#[macro_export]
macro_rules! rune_vm {
    ($($tt:tt)*) => {{
        let context = ::rune_modules::default_context().expect("failed to build context");
        let context = std::sync::Arc::new(context);
        $crate::testing::vm(&context, stringify!($($tt)*)).expect("program to compile successfully")
    }};
}

/// Same as [rune_s!] macro, except it takes a Rust token tree. This works
/// fairly well because Rust and Rune has very similar token trees.
///
/// # Examples
///
/// ```rust
/// use rune::testing::*;
///
/// # fn main() {
/// assert_eq! {
///     rune::rune!(bool => pub fn main() { true || false }),
///     true,
/// };
/// # }
#[macro_export]
macro_rules! rune {
    ($ty:ty => $($tt:tt)*) => {{
        let context = ::rune_modules::default_context().expect("failed to build context");
        let context = std::sync::Arc::new(context);

        $crate::testing::run::<_, (), $ty>(&context, stringify!($($tt)*), &["main"], ())
            .expect("program to run successfully")
    }};
}

/// Run the given program and return the expected type from it.
///
/// # Examples
///
/// ```rust
/// use rune::testing::*;
///
/// # fn main() {
/// assert_eq! {
///     rune::rune_s!(bool => "pub fn main() { true || false }"),
///     true,
/// };
/// # }
/// ```
#[macro_export]
macro_rules! rune_s {
    ($ty:ty => $source:expr) => {{
        let context = ::rune_modules::default_context().expect("failed to build context");
        let context = std::sync::Arc::new(context);

        $crate::testing::run::<_, (), $ty>(&context, $source, &["main"], ())
            .expect("program to run successfully")
    }};
}

/// Same as [rune!] macro, except it takes an external context, allowing testing
/// of native Rust data. This also accepts a tuple of arguments in the second
/// position, to pass native objects as arguments to the script.
///
/// # Examples
///
/// ```rust
/// use rune::testing::*;
/// use runestick::Module;
/// fn get_native_module() -> Module {
///     Module::new()
/// }
///
/// # fn main() {
/// assert_eq! {
///     rune::rune_n!(get_native_module(), (), bool => pub fn main() { true || false }),
///     true,
/// };
/// # }
#[macro_export]
macro_rules! rune_n {
    ($module:expr, $args:expr, $ty:ty => $($tt:tt)*) => {{
        let mut context = rune_modules::default_context().expect("failed to build context");
        context.install(&$module).expect("failed to install native module");
        let context = std::sync::Arc::new(context);

        rune::testing::run::<_, _, $ty>(&context, stringify!($($tt)*), &["main"], $args)
            .expect("program to run successfully")
    }};
}

/// Function used during parse testing to take the source, parse it as the given
/// type, tokenize it using [ToTokens][crate::macros::ToTokens], and parse the
/// token stream.
///
/// The results should be identical.
pub fn roundtrip<T>(source: &str) -> T
where
    T: crate::parsing::Parse + crate::macros::ToTokens + PartialEq + Eq + std::fmt::Debug,
{
    let mut parser = crate::parsing::Parser::new(source);
    let ast = parser.parse::<T>().expect("first parse");
    parser.eof().expect("first parse eof");

    let ctx = crate::macros::MacroContext::empty();
    let mut token_stream = crate::macros::TokenStream::new();

    ast.to_tokens(&ctx, &mut token_stream);
    let mut parser = crate::parsing::Parser::from_token_stream(&token_stream);
    let ast2 = parser.parse::<T>().expect("second parse");
    parser.eof().expect("second parse eof");

    assert_eq!(ast, ast2);
    ast
}

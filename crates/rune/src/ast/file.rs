use crate::ast;
use crate::{OptionSpanned as _, Parse, ParseError, ParseErrorKind, Parser, ToTokens};

/// A parsed file.
#[derive(Debug, Clone, ToTokens)]
pub struct File {
    /// Top level "Outer" `#![...]` attributes for the file
    pub attributes: Vec<ast::Attribute>,
    /// All the declarations in a file.
    pub items: Vec<(ast::Item, Option<ast::SemiColon>)>,
}

/// Parse a file.
///
/// # Examples
///
/// ```rust
/// use rune::{parse_all, ast};
///
/// parse_all::<ast::File>(r#"
/// use foo;
///
/// fn foo() {
///     42
/// }
///
/// use bar;
///
/// fn bar(a, b) {
///     a
/// }
/// "#).unwrap();
/// ```
///
/// # Realistic Example
///
/// ```rust
/// use rune::{parse_all, ast};
///
/// parse_all::<ast::File>(r#"
/// use http;
///
/// fn main() {
///     let client = http::client();
///     let response = client.get("https://google.com");
///     let text = response.text();
/// }
/// "#).unwrap();
///```
///
/// # File Attributes Example
///
/// ```rust
/// use rune::{parse_all, ast};
///
/// parse_all::<ast::File>(r#"
/// // NB: Attributes are currently rejected by the compiler
/// #![feature(attributes)]
///
/// fn main() {
///     for x in [1,2,3,4,5,6] {
///         println(`{x}`)
///     }
/// }
/// "#).unwrap();
/// ```
///
// TODO: this is a false positive: https://github.com/rust-lang/rust-clippy/issues/5879
#[allow(clippy::needless_doctest_main)]
impl Parse for File {
    fn parse(parser: &mut Parser<'_>) -> Result<Self, ParseError> {
        let mut attributes = vec![];

        // only allow outer attributes at the top of a file
        while parser.peek::<ast::attribute::OuterAttribute>()? {
            attributes.push(parser.parse()?);
        }

        let mut items = Vec::new();

        let mut item_attributes = parser.parse()?;
        let mut item_visibility = parser.parse()?;

        while ast::Item::peek_as_stmt(parser)? {
            let item: ast::Item =
                ast::Item::parse_with_meta(parser, item_attributes, item_visibility)?;

            let semi_colon = if item.needs_semi_colon() || parser.peek::<ast::SemiColon>()? {
                Some(parser.parse::<ast::SemiColon>()?)
            } else {
                None
            };

            items.push((item, semi_colon));
            item_attributes = parser.parse()?;
            item_visibility = parser.parse()?;
        }

        // meta without items. maybe use different error kind?
        if let Some(span) = item_attributes.option_span() {
            return Err(ParseError::new(
                span,
                ParseErrorKind::UnsupportedItemAttributes,
            ));
        }

        if let Some(span) = item_visibility.option_span() {
            return Err(ParseError::new(
                span,
                ParseErrorKind::UnsupportedItemVisibility,
            ));
        }

        Ok(Self { attributes, items })
    }
}

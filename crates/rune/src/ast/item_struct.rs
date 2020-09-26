use crate::ast;
use crate::{Parse, ParseError, Parser, Spanned, ToTokens};

/// A struct declaration.
#[derive(Debug, Clone, PartialEq, Eq, ToTokens, Spanned)]
pub struct ItemStruct {
    /// The attributes for the struct
    #[rune(iter)]
    pub attributes: Vec<ast::Attribute>,
    /// The visibility of the `struct` item
    #[rune(optional)]
    pub visibility: ast::Visibility,
    /// The `struct` keyword.
    pub struct_: ast::Struct,
    /// The identifier of the struct declaration.
    pub ident: ast::Ident,
    /// The body of the struct.
    pub body: ItemStructBody,
}

impl ItemStruct {
    /// Parse a `struct` item with the given attributes
    pub fn parse_with_meta(
        parser: &mut Parser,
        attributes: Vec<ast::Attribute>,
        visibility: ast::Visibility,
    ) -> Result<Self, ParseError> {
        Ok(Self {
            attributes,
            visibility,
            struct_: parser.parse()?,
            ident: parser.parse()?,
            body: parser.parse()?,
        })
    }
}

/// Parse implementation for a struct.
///
/// # Examples
///
/// ```rust
/// use rune::{testing, ast};
///
/// testing::roundtrip::<ast::ItemStruct>("struct Foo;");
/// testing::roundtrip::<ast::ItemStruct>("struct Foo ( a, b, c );");
/// testing::roundtrip::<ast::ItemStruct>("struct Foo { a, b, c }");
/// testing::roundtrip::<ast::ItemStruct>("struct Foo { #[default_value = 1] a, b, c }");
/// testing::roundtrip::<ast::ItemStruct>("#[alpha] struct Foo ( #[default_value = \"x\" ] a, b, c );");
/// ```
impl Parse for ItemStruct {
    fn parse(parser: &mut Parser<'_>) -> Result<Self, ParseError> {
        let attributes = parser.parse()?;
        let visibility = parser.parse()?;
        Self::parse_with_meta(parser, attributes, visibility)
    }
}

/// AST for a struct body.
#[derive(Debug, Clone, PartialEq, Eq, ToTokens, Spanned)]
pub enum ItemStructBody {
    /// An empty struct declaration.
    EmptyBody(ast::SemiColon),
    /// A tuple struct body.
    TupleBody(ast::Parenthesized<Field, ast::Comma>, ast::SemiColon),
    /// A regular struct body.
    StructBody(ast::Braced<Field, ast::Comma>),
}

impl ItemStructBody {
    /// Iterate over the fields of the body.
    pub fn fields(&self) -> impl Iterator<Item = &'_ (Field, Option<ast::Comma>)> {
        match self {
            ItemStructBody::EmptyBody(..) => IntoIterator::into_iter(&[]),
            ItemStructBody::TupleBody(body, ..) => body.iter(),
            ItemStructBody::StructBody(body) => body.iter(),
        }
    }
}

/// Parse implementation for a struct body.
///
/// # Examples
///
/// ```rust
/// use rune::{testing, ast};
///
/// testing::roundtrip::<ast::ItemStructBody>(";");
///
/// testing::roundtrip::<ast::ItemStructBody>("{ a, b, c }");
/// testing::roundtrip::<ast::ItemStructBody>("{ #[x] a, #[y] b, #[z] #[w] #[f32] c }");
/// testing::roundtrip::<ast::ItemStructBody>("{ a, #[attribute] b, c }");
///
/// testing::roundtrip::<ast::ItemStructBody>("( a, b, c );");
/// testing::roundtrip::<ast::ItemStructBody>("( #[x] a, b, c );");
/// testing::roundtrip::<ast::ItemStructBody>("( #[x] pub a, b, c );");
/// testing::roundtrip::<ast::ItemStructBody>("( a, b, c );");
/// testing::roundtrip::<ast::ItemStructBody>("();");
/// ```
impl Parse for ItemStructBody {
    fn parse(parser: &mut Parser<'_>) -> Result<Self, ParseError> {
        let t = parser.token_peek_eof()?;

        let body = match t.kind {
            ast::Kind::Open(ast::Delimiter::Parenthesis) => {
                Self::TupleBody(parser.parse()?, parser.parse()?)
            }
            ast::Kind::Open(ast::Delimiter::Brace) => Self::StructBody(parser.parse()?),
            _ => Self::EmptyBody(parser.parse()?),
        };

        Ok(body)
    }
}

/// A field as part of a struct or a tuple body.
///
/// # Examples
///
/// ```rust
/// use rune::{testing, ast};
///
/// testing::roundtrip::<ast::Field>("a");
/// testing::roundtrip::<ast::Field>("#[x] a");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, ToTokens, Parse, Spanned)]
pub struct Field {
    /// Attributes associated with field.
    #[rune(iter, attributes)]
    pub attributes: Vec<ast::Attribute>,
    /// The visibility of the field
    #[rune(optional)]
    pub visibility: ast::Visibility,
    /// Name of the field.
    pub name: ast::Ident,
}

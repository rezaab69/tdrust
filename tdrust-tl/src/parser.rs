//! Parser for TDLib's `td_api.tl` schema.
//!
//! The schema is a sequence of single-line declarations of the form
//! `name field:type ... = ResultType;`, each preceded by `//@` documentation
//! comments (continued with `//-` lines). Declarations after the
//! `---functions---` marker are functions; everything before it is a type.

use std::collections::BTreeMap;
use std::fmt;

/// A type reference as written in the schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TlType {
    /// A named type: a builtin (`int32`, `string`, ...), a constructor
    /// (`formattedText`) or an abstract type (`MessageContent`).
    Named(String),
    /// `vector<T>`.
    Vector(Box<TlType>),
}

impl fmt::Display for TlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlType::Named(name) => f.write_str(name),
            TlType::Vector(inner) => write!(f, "vector<{inner}>"),
        }
    }
}

/// A field of a constructor or a parameter of a function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Name as written in the schema (`snake_case`).
    pub name: String,
    /// Declared type.
    pub ty: TlType,
    /// Documentation from the `//@name` tag.
    pub description: String,
}

/// A constructor (`types`) or a function (`functions`) declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// Name as written in the schema (`camelCase`).
    pub name: String,
    /// Fields or parameters, in declaration order.
    pub fields: Vec<Field>,
    /// The type this constructor belongs to, or the type a function returns.
    pub result: String,
    /// Documentation from the `//@description` tag.
    pub description: String,
    /// 1-based line number in the schema source.
    pub line: usize,
}

/// A parsed schema.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schema {
    /// Type constructors, in declaration order.
    pub types: Vec<Definition>,
    /// Functions, in declaration order.
    pub functions: Vec<Definition>,
    /// Abstract types declared with `//@class`, mapped to their description.
    pub classes: BTreeMap<String, String>,
}

/// An error encountered while parsing a schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "td_api.tl:{}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Constructors that describe the TL type system itself rather than the API.
const BUILTIN_CONSTRUCTORS: &[&str] =
    &["double", "string", "int32", "int53", "int64", "bytes", "boolFalse", "boolTrue", "vector"];

/// Parses the contents of a `td_api.tl` file.
pub fn parse(source: &str) -> Result<Schema, ParseError> {
    let mut schema = Schema::default();
    let mut in_functions = false;
    let mut doc = String::new();

    for (index, raw) in source.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.trim();

        if line.is_empty() {
            doc.clear();
            continue;
        }
        if line == "---functions---" {
            in_functions = true;
            doc.clear();
            continue;
        }
        if line == "---types---" {
            in_functions = false;
            doc.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("//@class ") {
            let text = format!("@class {rest}");
            let tags = doc_tags(&text, |name| name == "class" || name == "description");
            let name = tag(&tags, "class").unwrap_or_default();
            if !is_identifier(&name) {
                return Err(error(line_no, format!("invalid class name `{name}`")));
            }
            schema.classes.insert(name, tag(&tags, "description").unwrap_or_default());
            continue;
        }
        if let Some(rest) = line.strip_prefix("//-") {
            doc.push(' ');
            doc.push_str(rest.trim());
            continue;
        }
        if let Some(rest) = line.strip_prefix("//") {
            if rest.starts_with('@') {
                doc.push(' ');
                doc.push_str(rest.trim());
            }
            continue;
        }

        let mut definition = parse_declaration(line, line_no)?;
        if BUILTIN_CONSTRUCTORS.contains(&definition.name.as_str()) {
            doc.clear();
            continue;
        }
        attach_docs(&mut definition, &doc);
        doc.clear();

        if in_functions {
            schema.functions.push(definition);
        } else {
            schema.types.push(definition);
        }
    }

    Ok(schema)
}

fn error(line: usize, message: String) -> ParseError {
    ParseError { line, message }
}

fn parse_declaration(line: &str, line_no: usize) -> Result<Definition, ParseError> {
    let body = line.strip_suffix(';').ok_or_else(|| error(line_no, "declaration must end with `;`".into()))?;
    let (lhs, result) = body.split_once('=').ok_or_else(|| error(line_no, "declaration is missing `=`".into()))?;
    let result = result.trim().to_owned();

    let mut parts = lhs.split_whitespace();
    let name = parts.next().ok_or_else(|| error(line_no, "declaration has no name".into()))?.to_owned();

    // The builtin `vector {t:Type} # [ t ] = Vector t;` uses syntax that the
    // rest of the schema never does, so it is recognized by name only.
    if BUILTIN_CONSTRUCTORS.contains(&name.as_str()) {
        return Ok(Definition { name, fields: Vec::new(), result, description: String::new(), line: line_no });
    }
    if !is_identifier(&name) || !is_identifier(&result) {
        return Err(error(line_no, format!("invalid declaration `{line}`")));
    }

    let mut fields = Vec::new();
    for part in parts {
        let (field_name, ty) = part.split_once(':').ok_or_else(|| error(line_no, format!("invalid field `{part}`")))?;
        if !is_identifier(field_name) {
            return Err(error(line_no, format!("invalid field name `{field_name}`")));
        }
        let ty = parse_type(ty).ok_or_else(|| error(line_no, format!("invalid type `{ty}`")))?;
        fields.push(Field { name: field_name.to_owned(), ty, description: String::new() });
    }

    Ok(Definition { name, fields, result, description: String::new(), line: line_no })
}

fn parse_type(text: &str) -> Option<TlType> {
    if let Some(inner) = text.strip_prefix("vector<").and_then(|rest| rest.strip_suffix('>')) {
        return parse_type(inner).map(|inner| TlType::Vector(Box::new(inner)));
    }
    is_identifier(text).then(|| TlType::Named(text.to_owned()))
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic()) && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Fills in the description of a definition and of its fields from the
/// accumulated `//@` documentation.
fn attach_docs(definition: &mut Definition, doc: &str) {
    let field_names: Vec<&str> = definition.fields.iter().map(|f| f.name.as_str()).collect();
    let tags =
        doc_tags(doc, |name| name == "description" || name == "param_description" || field_names.contains(&name));

    definition.description = tag(&tags, "description").unwrap_or_default();
    for field in &mut definition.fields {
        // A field called `description` is documented as `@param_description`
        // so that it does not clash with the declaration's own description.
        let key = if field.name == "description" { "param_description" } else { field.name.as_str() };
        field.description = tag(&tags, key).unwrap_or_default();
    }
}

fn tag(tags: &[(String, String)], name: &str) -> Option<String> {
    tags.iter().find(|(tag, _)| tag == name).map(|(_, value)| value.clone())
}

/// Splits documentation text into `(tag, value)` pairs.
///
/// A tag is `@name` at the start of the text or after whitespace, where `name`
/// is accepted by `is_tag`. Restricting tags to known names keeps text such as
/// "contact @BotFather" inside the value it belongs to.
fn doc_tags(text: &str, is_tag: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    let bytes = text.as_bytes();
    // (position of `@`, end of tag name)
    let mut markers = Vec::new();
    for (at, &byte) in bytes.iter().enumerate() {
        if byte != b'@' || (at > 0 && !bytes[at - 1].is_ascii_whitespace()) {
            continue;
        }
        let name_end = bytes[at + 1..]
            .iter()
            .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
            .map_or(bytes.len(), |offset| at + 1 + offset);
        let followed_by_space = name_end == bytes.len() || bytes[name_end].is_ascii_whitespace();
        if name_end > at + 1 && followed_by_space && is_tag(&text[at + 1..name_end]) {
            markers.push((at, name_end));
        }
    }

    markers
        .iter()
        .enumerate()
        .map(|(i, &(at, name_end))| {
            let value_end = markers.get(i + 1).map_or(text.len(), |&(next, _)| next);
            let value = text[name_end..value_end].split_whitespace().collect::<Vec<_>>().join(" ");
            (text[at + 1..name_end].to_owned(), value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
double ? = Double;
string ? = String;
vector {t:Type} # [ t ] = Vector t;

//@description A text with some entities @text The text @entities Entities contained in the text.
//-Entities can be nested
formattedText text:string entities:vector<textEntity> = FormattedText;

//@class ChatType @description Describes the type of a chat

//@description An ordinary chat with a user @user_id User identifier
chatTypePrivate user_id:int53 = ChatType;

//@description Describes a game @param_description Game description @title Title; contact @BotFather
game title:string description:string = Game;

---functions---

//@description Returns the current user. Can be called synchronously
getMe = User;

//@description Sends a message @chat_id Target chat @rows Rows
sendRows chat_id:int53 rows:vector<vector<int64>> = Ok;
"#;

    #[test]
    fn parses_types_functions_and_classes() {
        let schema = parse(SAMPLE).unwrap();
        let names: Vec<_> = schema.types.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["formattedText", "chatTypePrivate", "game"]);
        let functions: Vec<_> = schema.functions.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(functions, ["getMe", "sendRows"]);
        assert_eq!(schema.classes["ChatType"], "Describes the type of a chat");
    }

    #[test]
    fn attaches_docs_with_continuations() {
        let schema = parse(SAMPLE).unwrap();
        let text = &schema.types[0];
        assert_eq!(text.description, "A text with some entities");
        assert_eq!(text.fields[0].description, "The text");
        assert_eq!(text.fields[1].description, "Entities contained in the text. Entities can be nested");
    }

    #[test]
    fn param_description_and_inline_at_signs() {
        let schema = parse(SAMPLE).unwrap();
        let game = &schema.types[2];
        assert_eq!(game.description, "Describes a game");
        assert_eq!(game.fields[0].description, "Title; contact @BotFather");
        assert_eq!(game.fields[1].description, "Game description");
    }

    #[test]
    fn parses_nested_vectors() {
        let schema = parse(SAMPLE).unwrap();
        let rows = &schema.functions[1].fields[1];
        assert_eq!(rows.ty.to_string(), "vector<vector<int64>>");
    }

    #[test]
    fn reports_errors_with_line_numbers() {
        let err = parse("\n\nbroken field:int32 = Ok").unwrap_err();
        assert_eq!(err.line, 3);
        assert!(err.to_string().contains("td_api.tl:3"));
    }
}

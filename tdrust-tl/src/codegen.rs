//! Generation of Rust source code from a parsed [`Schema`].
//!
//! The output consists of three pieces of code, meant to be `include!`d by
//! the `tdrust` crate:
//!
//! * `types`: one struct per constructor and one enum per abstract type;
//! * `functions`: one struct per function, implementing `crate::Function`;
//! * `methods`: an `impl crate::Client` block with one async method per
//!   function.
//!
//! Encoding rules (mirroring TDLib's JSON interface):
//!
//! * every struct serializes its own `"@type"` tag;
//! * enums serialize as the struct they hold and deserialize by looking at
//!   `"@type"`, keeping unknown objects as `Unrecognized(serde_json::Value)`;
//! * `int64` values travel as JSON strings and `bytes` as base64 strings;
//! * object fields documented as possibly `null` become `Option<T>`;
//! * fields that would make a type infinitely large are boxed.

use std::collections::{HashMap, HashSet};
use std::fmt::{self, Write as _};

use crate::parser::{Definition, Field, Schema, TlType};

/// Settings that control code generation.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Method names already defined on `Client` by hand. Generating a method
    /// with one of these names is reported as an error rather than producing
    /// code that does not compile.
    pub reserved_method_names: Vec<String>,
}

/// Generated Rust source code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    /// Contents of the `types` module.
    pub types: String,
    /// Contents of the `functions` module.
    pub functions: String,
    /// An `impl crate::Client { ... }` block.
    pub methods: String,
}

/// An error that prevents generating code for a schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerateError(pub String);

impl fmt::Display for GenerateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GenerateError {}

type Result<T> = std::result::Result<T, GenerateError>;

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(GenerateError(message.into()))
}

const STRING: &str = "::std::string::String";
const BYTES: &str = "::std::vec::Vec<u8>";
const UNRECOGNIZED: &str = "Unrecognized";

/// Generates Rust code for `schema`.
pub fn generate(schema: &Schema, options: &Options) -> Result<Generated> {
    let model = Model::build(schema)?;
    Ok(Generated {
        types: model.emit_types()?,
        functions: model.emit_functions()?,
        methods: model.emit_methods(options)?,
    })
}

struct Model<'a> {
    schema: &'a Schema,
    /// Rust struct name of every type constructor.
    constructors: HashMap<&'a str, String>,
    /// Rust type name of every result (abstract) type.
    results: HashMap<&'a str, String>,
    /// Result types rendered as enums, with their constructors.
    enums: Vec<(&'a str, Vec<&'a Definition>)>,
    enum_names: HashSet<String>,
    /// `pub type Alias = Struct;` for single-constructor types whose
    /// constructor name differs from the type name.
    aliases: Vec<(String, String)>,
    /// `(struct, field)` pairs whose type must be boxed to break recursion.
    boxed: HashSet<(String, String)>,
    /// Type structs for which `Default` can be derived.
    defaultable: HashSet<String>,
}

impl<'a> Model<'a> {
    fn build(schema: &'a Schema) -> Result<Self> {
        let mut constructors = HashMap::new();
        let mut order: Vec<&str> = Vec::new();
        let mut by_result: HashMap<&str, Vec<&Definition>> = HashMap::new();

        for def in &schema.types {
            if constructors.insert(def.name.as_str(), pascal(&def.name)).is_some() {
                return fail(format!("line {}: duplicate constructor `{}`", def.line, def.name));
            }
            by_result
                .entry(def.result.as_str())
                .or_insert_with(|| {
                    order.push(def.result.as_str());
                    Vec::new()
                })
                .push(def);
        }
        for class in schema.classes.keys() {
            by_result.entry(class.as_str()).or_insert_with(|| {
                order.push(class.as_str());
                Vec::new()
            });
        }

        let mut results = HashMap::new();
        let mut enums = Vec::new();
        let mut enum_names = HashSet::new();
        let mut aliases = Vec::new();
        for result in order {
            let ctors = by_result.remove(result).unwrap_or_default();
            if schema.classes.contains_key(result) || ctors.len() != 1 {
                results.insert(result, result.to_owned());
                enum_names.insert(result.to_owned());
                enums.push((result, ctors));
            } else {
                let name = pascal(&ctors[0].name);
                if name != result {
                    aliases.push((result.to_owned(), name.clone()));
                }
                results.insert(result, name);
            }
        }

        let mut seen = HashSet::new();
        let all_names = constructors
            .values()
            .cloned()
            .chain(enum_names.iter().cloned())
            .chain(aliases.iter().map(|(alias, _)| alias.clone()));
        for name in all_names {
            if !seen.insert(name.clone()) {
                return fail(format!("two schema types map to the Rust name `{name}`"));
            }
        }

        let mut model = Model {
            schema,
            constructors,
            results,
            enums,
            enum_names,
            aliases,
            boxed: HashSet::new(),
            defaultable: HashSet::new(),
        };
        model.boxed = model.find_boxed_fields()?;
        model.defaultable = model.find_defaultable()?;
        Ok(model)
    }

    /// Resolves a named schema type to the Rust type name it maps to, or
    /// `None` for builtins.
    fn object_name(&self, name: &str) -> Result<Option<&str>> {
        if is_builtin(name) {
            return Ok(None);
        }
        let found = if starts_lowercase(name) { self.constructors.get(name) } else { self.results.get(name) };
        match found {
            Some(rust) => Ok(Some(rust.as_str())),
            None => fail(format!("unknown type `{name}`")),
        }
    }

    fn is_enum(&self, rust_name: &str) -> bool {
        self.enum_names.contains(rust_name)
    }

    /// Finds fields that participate in a cycle of by-value containment and
    /// therefore must be boxed. Every such cycle contains at least one field
    /// edge (enums only point at their variant structs), so boxing all field
    /// edges inside a strongly connected component breaks every cycle.
    fn find_boxed_fields(&self) -> Result<HashSet<(String, String)>> {
        let mut graph = Graph::default();
        let mut field_edges = Vec::new();
        for def in &self.schema.types {
            let from = graph.node(&self.constructors[def.name.as_str()]);
            for field in &def.fields {
                if let TlType::Named(name) = &field.ty {
                    if let Some(target) = self.object_name(name)? {
                        let to = graph.node(target);
                        graph.edge(from, to);
                        field_edges.push((from, to, field.name.clone()));
                    }
                }
            }
        }
        for (result, ctors) in &self.enums {
            let from = graph.node(result);
            for ctor in ctors {
                let to = graph.node(&self.constructors[ctor.name.as_str()]);
                graph.edge(from, to);
            }
        }

        let component = graph.strongly_connected_components();
        Ok(field_edges
            .into_iter()
            .filter(|(from, to, _)| component[*from] == component[*to])
            .map(|(from, _, field)| (graph.names[from].clone(), field))
            .collect())
    }

    fn find_defaultable(&self) -> Result<HashSet<String>> {
        let mut defaultable: HashSet<String> = self.constructors.values().cloned().collect();
        loop {
            let mut changed = false;
            for def in &self.schema.types {
                let name = &self.constructors[def.name.as_str()];
                if !defaultable.contains(name) {
                    continue;
                }
                for field in &def.fields {
                    if !self.field_is_defaultable(name, field, &defaultable)? {
                        defaultable.remove(name);
                        changed = true;
                        break;
                    }
                }
            }
            if !changed {
                return Ok(defaultable);
            }
        }
    }

    fn field_is_defaultable(&self, owner: &str, field: &Field, defaultable: &HashSet<String>) -> Result<bool> {
        let TlType::Named(name) = &field.ty else { return Ok(true) };
        let Some(target) = self.object_name(name)? else { return Ok(true) };
        if is_nullable(field) {
            return Ok(true);
        }
        if self.boxed.contains(&(owner.to_owned(), field.name.clone())) || self.is_enum(target) {
            return Ok(false);
        }
        Ok(defaultable.contains(target))
    }

    /// The Rust type of a schema type, without `Option`/`Box` wrappers.
    fn rust_type(&self, ty: &TlType) -> Result<String> {
        Ok(match ty {
            TlType::Vector(inner) => format!("::std::vec::Vec<{}>", self.rust_type(inner)?),
            TlType::Named(name) => match name.as_str() {
                "int32" => "i32".to_owned(),
                "int53" | "int64" => "i64".to_owned(),
                "double" => "f64".to_owned(),
                "string" => STRING.to_owned(),
                "bytes" => BYTES.to_owned(),
                "Bool" => "bool".to_owned(),
                _ => format!("crate::types::{}", self.object_name(name)?.unwrap_or_default()),
            },
        })
    }

    fn field(&self, owner: &str, field: &Field) -> Result<FieldModel> {
        let base = self.rust_type(&field.ty)?;
        let object = match &field.ty {
            TlType::Named(name) => self.object_name(name)?,
            TlType::Vector(_) => None,
        };
        let nullable = object.is_some() && is_nullable(field);
        let boxed = self.boxed.contains(&(owner.to_owned(), field.name.clone()));

        let mut ty = base.clone();
        if boxed {
            ty = format!("::std::boxed::Box<{ty}>");
        }
        if nullable {
            ty = format!("::std::option::Option<{ty}>");
        }

        let mut attrs = Vec::new();
        let (rust_name, renamed) = rust_identifier(&field.name);
        if renamed {
            attrs.push(format!("rename = \"{}\"", field.name));
        }
        match serde_helper(&field.ty)? {
            Some(helper) => {
                attrs.push("default".to_owned());
                attrs.push(format!("with = \"crate::serde_helpers::{helper}\""));
            }
            None if object.is_none() => attrs.push("default".to_owned()),
            None if nullable => {
                attrs.push("default".to_owned());
                attrs.push("skip_serializing_if = \"::std::option::Option::is_none\"".to_owned());
            }
            None => {}
        }

        Ok(FieldModel {
            rust_name,
            ty,
            base,
            attrs,
            is_string: field.ty == TlType::Named("string".to_owned()),
            is_required_enum: !nullable && !boxed && object.is_some_and(|name| self.is_enum(name)),
            description: field.description.clone(),
        })
    }

    fn emit_struct(&self, out: &mut String, def: &Definition, name: &str, derive_default: bool) -> Result<()> {
        doc(out, "", &def.description);
        let default = if derive_default { ", Default" } else { "" };
        writeln!(out, "#[derive(Debug, Clone, PartialEq, ::serde::Serialize, ::serde::Deserialize{default})]").unwrap();
        writeln!(out, "#[serde(tag = \"@type\", rename = \"{}\")]", def.name).unwrap();
        writeln!(out, "pub struct {name} {{").unwrap();
        for field in &def.fields {
            let field = self.field(name, field)?;
            doc(out, "    ", &field.description);
            if !field.attrs.is_empty() {
                writeln!(out, "    #[serde({})]", field.attrs.join(", ")).unwrap();
            }
            writeln!(out, "    pub {}: {},", field.rust_name, field.ty).unwrap();
        }
        writeln!(out, "}}\n").unwrap();
        Ok(())
    }

    fn emit_types(&self) -> Result<String> {
        let mut out = String::from(HEADER);
        for def in &self.schema.types {
            let name = &self.constructors[def.name.as_str()];
            self.emit_struct(&mut out, def, name, self.defaultable.contains(name))?;
        }
        for (alias, target) in &self.aliases {
            writeln!(out, "/// Alias of [`{target}`], the only constructor of `{alias}`.").unwrap();
            writeln!(out, "pub type {alias} = {target};\n").unwrap();
        }
        for (result, ctors) in &self.enums {
            self.emit_enum(&mut out, result, ctors)?;
        }
        Ok(out)
    }

    fn emit_enum(&self, out: &mut String, name: &str, ctors: &[&Definition]) -> Result<()> {
        let short: Vec<String> = ctors.iter().map(|ctor| variant_name(name, &ctor.name)).collect();
        let names = if all_unique(&short) { short } else { ctors.iter().map(|ctor| pascal(&ctor.name)).collect() };
        if !all_unique(&names) {
            return fail(format!("enum `{name}` has conflicting variant names"));
        }
        let variants: Vec<(String, &str, &Definition)> = names
            .into_iter()
            .zip(ctors)
            .map(|(variant, ctor)| (variant, self.constructors[ctor.name.as_str()].as_str(), *ctor))
            .collect();

        let description = self.schema.classes.get(name).cloned().unwrap_or_default();
        doc(out, "", &description);
        writeln!(out, "#[derive(Debug, Clone, PartialEq)]").unwrap();
        writeln!(out, "pub enum {name} {{").unwrap();
        for (variant, ty, ctor) in &variants {
            doc(out, "    ", &ctor.description);
            writeln!(out, "    {variant}(crate::types::{ty}),").unwrap();
        }
        writeln!(out, "    /// An object whose `@type` is unknown to this version of the bindings,").unwrap();
        writeln!(out, "    /// kept as raw JSON.").unwrap();
        writeln!(out, "    {UNRECOGNIZED}(::serde_json::Value),").unwrap();
        writeln!(out, "}}\n").unwrap();

        writeln!(out, "impl {name} {{").unwrap();
        writeln!(out, "    /// The TDLib `@type` of the contained object.").unwrap();
        writeln!(out, "    pub fn type_name(&self) -> &str {{").unwrap();
        writeln!(out, "        match self {{").unwrap();
        for (variant, _, ctor) in &variants {
            writeln!(out, "            Self::{variant}(_) => \"{}\",", ctor.name).unwrap();
        }
        writeln!(
            out,
            "            Self::{UNRECOGNIZED}(value) => value.get(\"@type\").and_then(::serde_json::Value::as_str).unwrap_or_default(),"
        )
        .unwrap();
        writeln!(out, "        }}\n    }}\n}}\n").unwrap();

        writeln!(out, "impl ::serde::Serialize for {name} {{").unwrap();
        writeln!(
            out,
            "    fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> ::std::result::Result<S::Ok, S::Error> {{"
        )
        .unwrap();
        writeln!(out, "        match self {{").unwrap();
        for (variant, _, _) in &variants {
            writeln!(out, "            Self::{variant}(value) => ::serde::Serialize::serialize(value, serializer),")
                .unwrap();
        }
        writeln!(out, "            Self::{UNRECOGNIZED}(value) => ::serde::Serialize::serialize(value, serializer),")
            .unwrap();
        writeln!(out, "        }}\n    }}\n}}\n").unwrap();

        writeln!(out, "impl<'de> ::serde::Deserialize<'de> for {name} {{").unwrap();
        writeln!(
            out,
            "    fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> ::std::result::Result<Self, D::Error> {{"
        )
        .unwrap();
        writeln!(out, "        let value = <::serde_json::Value as ::serde::Deserialize>::deserialize(deserializer)?;")
            .unwrap();
        writeln!(out, "        match crate::serde_helpers::type_name::<D::Error>(&value)?.as_str() {{").unwrap();
        for (variant, _, ctor) in &variants {
            writeln!(
                out,
                "            \"{}\" => crate::serde_helpers::from_value(value).map(Self::{variant}),",
                ctor.name
            )
            .unwrap();
        }
        writeln!(out, "            _ => ::std::result::Result::Ok(Self::{UNRECOGNIZED}(value)),").unwrap();
        writeln!(out, "        }}\n    }}\n}}\n").unwrap();

        for (variant, ty, _) in &variants {
            writeln!(out, "impl ::std::convert::From<crate::types::{ty}> for {name} {{").unwrap();
            writeln!(out, "    fn from(value: crate::types::{ty}) -> Self {{").unwrap();
            writeln!(out, "        Self::{variant}(value)\n    }}\n}}\n").unwrap();
        }
        Ok(())
    }

    fn emit_functions(&self) -> Result<String> {
        let mut out = String::from(HEADER);
        for def in &self.schema.functions {
            let name = pascal(&def.name);
            let output = self.rust_type(&TlType::Named(def.result.clone()))?;
            let mut derive_default = true;
            for field in &def.fields {
                derive_default &= self.field_is_defaultable(&name, field, &self.defaultable)?;
            }

            let mut described = def.clone();
            described.description = format!("{}\n\nReturns [`{output}`].", def.description);
            self.emit_struct(&mut out, &described, &name, derive_default)?;

            writeln!(out, "impl crate::Function for {name} {{").unwrap();
            writeln!(out, "    type Output = {output};").unwrap();
            writeln!(out, "    const TYPE: &'static str = \"{}\";", def.name).unwrap();
            writeln!(out, "}}\n").unwrap();
            if def.description.contains("Can be called synchronously") {
                writeln!(out, "impl crate::SyncFunction for {name} {{}}\n").unwrap();
            }
        }
        Ok(out)
    }

    fn emit_methods(&self, options: &Options) -> Result<String> {
        let mut out = String::from(HEADER);
        writeln!(out, "#[allow(clippy::too_many_arguments)]").unwrap();
        writeln!(out, "impl crate::Client {{").unwrap();
        for def in &self.schema.functions {
            let struct_name = pascal(&def.name);
            let (method, _) = rust_identifier(&snake(&def.name));
            if options.reserved_method_names.contains(&method) {
                return fail(format!(
                    "line {}: generated method `{method}` clashes with a hand-written `Client` method",
                    def.line
                ));
            }
            let output = self.rust_type(&TlType::Named(def.result.clone()))?;

            let mut params = Vec::new();
            let mut inits = Vec::new();
            let mut docs = Vec::new();
            for field in &def.fields {
                let field = self.field(&struct_name, field)?;
                let name = &field.rust_name;
                if field.is_string {
                    params.push(format!("{name}: impl ::std::convert::Into<{STRING}>"));
                    inits.push(format!("{name}: {name}.into()"));
                } else if field.is_required_enum {
                    params.push(format!("{name}: impl ::std::convert::Into<{}>", field.base));
                    inits.push(format!("{name}: {name}.into()"));
                } else {
                    params.push(format!("{name}: {}", field.ty));
                    inits.push(name.clone());
                }
                docs.push((name.trim_start_matches("r#").to_owned(), field.description));
            }

            doc(&mut out, "    ", &def.description);
            if !docs.is_empty() {
                writeln!(out, "    ///\n    /// # Parameters\n    ///").unwrap();
                for (name, description) in &docs {
                    writeln!(out, "    /// * `{name}` - {}", escape_doc(description)).unwrap();
                }
            }
            writeln!(out, "    ///\n    /// Sends [`crate::functions::{struct_name}`].").unwrap();
            writeln!(
                out,
                "    pub async fn {method}(&self{}{}) -> crate::Result<{output}> {{",
                if params.is_empty() { "" } else { ", " },
                params.join(", ")
            )
            .unwrap();
            if inits.is_empty() {
                writeln!(out, "        self.send(crate::functions::{struct_name} {{}}).await").unwrap();
            } else {
                writeln!(out, "        self.send(crate::functions::{struct_name} {{ {} }}).await", inits.join(", "))
                    .unwrap();
            }
            writeln!(out, "    }}\n").unwrap();
        }
        writeln!(out, "}}").unwrap();
        Ok(out)
    }
}

struct FieldModel {
    rust_name: String,
    /// Full field type, including `Option`/`Box`.
    ty: String,
    /// Field type without wrappers.
    base: String,
    attrs: Vec<String>,
    is_string: bool,
    is_required_enum: bool,
    description: String,
}

/// Whether all names are distinct and none clashes with the catch-all variant.
fn all_unique(names: &[String]) -> bool {
    let mut seen = HashSet::new();
    names.iter().all(|name| name != UNRECOGNIZED && seen.insert(name.as_str()))
}

const HEADER: &str = "// @generated by tdrust-tl from td_api.tl. Do not edit by hand.\n\n";

fn is_builtin(name: &str) -> bool {
    matches!(name, "int32" | "int53" | "int64" | "double" | "string" | "bytes" | "Bool")
}

fn starts_lowercase(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
}

/// Object fields are optional when their documentation says they can be null
/// ("may be null", "pass null to ...").
fn is_nullable(field: &Field) -> bool {
    field.description.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| word.eq_ignore_ascii_case("null"))
}

/// The `crate::serde_helpers` module used to (de)serialize a field, if any.
fn serde_helper(ty: &TlType) -> Result<Option<&'static str>> {
    Ok(match ty {
        TlType::Named(name) if name == "int64" => Some("int64"),
        TlType::Named(name) if name == "bytes" => Some("bytes"),
        TlType::Vector(inner) => match named(inner) {
            Some("int64") => Some("vec_int64"),
            Some("bytes") => Some("vec_bytes"),
            None if contains_special(inner) => return fail(format!("unsupported nested type `{ty}`")),
            _ => None,
        },
        TlType::Named(_) => None,
    })
}

fn named(ty: &TlType) -> Option<&str> {
    match ty {
        TlType::Named(name) => Some(name),
        TlType::Vector(_) => None,
    }
}

fn contains_special(ty: &TlType) -> bool {
    match ty {
        TlType::Named(name) => name == "int64" || name == "bytes",
        TlType::Vector(inner) => contains_special(inner),
    }
}

/// `messageText` -> `MessageText`.
pub fn pascal(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map(|first| first.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default()
}

/// `getChatHistory` -> `get_chat_history`, `getRTMPUrl` -> `get_rtmp_url`.
pub fn snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_is_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase() || prev.is_ascii_digit() || (prev.is_ascii_uppercase() && next_is_lower) {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// The enum variant for a constructor: the constructor name without the
/// longest whole-word prefix it shares with the type name
/// (`authorizationStateReady` in `AuthorizationState` -> `Ready`,
/// `messageText` in `MessageContent` -> `Text`), or the full name when
/// nothing can be stripped.
pub fn variant_name(enum_name: &str, ctor: &str) -> String {
    let full = pascal(ctor);
    let word_ends = enum_name
        .char_indices()
        .skip(1)
        .filter(|(_, c)| c.is_ascii_uppercase())
        .map(|(i, _)| i)
        .chain([enum_name.len()]);
    let mut prefixes: Vec<&str> = word_ends.map(|end| &enum_name[..end]).collect();
    prefixes.reverse();
    for prefix in prefixes {
        if let Some(rest) = full.strip_prefix(prefix) {
            if rest.starts_with(|c: char| c.is_ascii_uppercase()) {
                return rest.to_owned();
            }
        }
    }
    full
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "dyn", "else", "enum", "extern", "false", "fn", "for", "if",
    "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static", "struct", "trait",
    "true", "type", "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "gen", "macro",
    "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
];

/// A Rust identifier for a schema name, and whether serde needs an explicit
/// `rename` (raw identifiers such as `r#type` are handled by serde itself).
fn rust_identifier(name: &str) -> (String, bool) {
    match name {
        "self" | "Self" | "super" | "crate" => (format!("{name}_"), true),
        _ if KEYWORDS.contains(&name) => (format!("r#{name}"), false),
        _ => (name.to_owned(), false),
    }
}

/// Writes `text` as `///` doc comment lines, escaping Markdown that rustdoc
/// would misinterpret.
fn doc(out: &mut String, indent: &str, text: &str) {
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            writeln!(out, "{indent}///").unwrap();
            continue;
        }
        writeln!(out, "{indent}/// {}", escape_doc(line)).unwrap();
    }
}

fn escape_doc(text: &str) -> String {
    // Keep intra-doc links we generate ourselves (`[`crate::...`]`) intact.
    if text.starts_with("Returns [`") || text.starts_with("Sends [`") {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '[' | ']' | '*' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// A directed graph over Rust type names, for cycle detection.
#[derive(Default)]
struct Graph {
    names: Vec<String>,
    index: HashMap<String, usize>,
    edges: Vec<Vec<usize>>,
}

impl Graph {
    fn node(&mut self, name: &str) -> usize {
        if let Some(&id) = self.index.get(name) {
            return id;
        }
        let id = self.names.len();
        self.names.push(name.to_owned());
        self.index.insert(name.to_owned(), id);
        self.edges.push(Vec::new());
        id
    }

    fn edge(&mut self, from: usize, to: usize) {
        self.edges[from].push(to);
    }

    /// Tarjan's algorithm; returns the component id of every node.
    fn strongly_connected_components(&self) -> Vec<usize> {
        struct State<'g> {
            graph: &'g Graph,
            counter: usize,
            index: Vec<Option<usize>>,
            low: Vec<usize>,
            on_stack: Vec<bool>,
            stack: Vec<usize>,
            component: Vec<usize>,
            components: usize,
        }

        fn visit(s: &mut State<'_>, v: usize) {
            s.index[v] = Some(s.counter);
            s.low[v] = s.counter;
            s.counter += 1;
            s.stack.push(v);
            s.on_stack[v] = true;
            for &w in &s.graph.edges[v] {
                match s.index[w] {
                    None => {
                        visit(s, w);
                        s.low[v] = s.low[v].min(s.low[w]);
                    }
                    Some(index) if s.on_stack[w] => s.low[v] = s.low[v].min(index),
                    Some(_) => {}
                }
            }
            if Some(s.low[v]) == s.index[v] {
                while let Some(w) = s.stack.pop() {
                    s.on_stack[w] = false;
                    s.component[w] = s.components;
                    if w == v {
                        break;
                    }
                }
                s.components += 1;
            }
        }

        let n = self.names.len();
        let mut state = State {
            graph: self,
            counter: 0,
            index: vec![None; n],
            low: vec![0; n],
            on_stack: vec![false; n],
            stack: Vec::new(),
            component: vec![0; n],
            components: 0,
        };
        for v in 0..n {
            if state.index[v].is_none() {
                visit(&mut state, v);
            }
        }
        state.component
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    const SAMPLE: &str = r#"
//@class RichText @description Describes a rich text
//@description A plain text @text Text
richTextPlain text:string = RichText;
//@description A bold text @text Text
richTextBold text:RichText = RichText;

//@description A chat @id Chat identifier @photo Chat photo; may be null @rich Rich title @access_hash Hash @data Data @ids Identifiers
chat id:int53 photo:chatPhoto rich:RichText access_hash:int64 data:bytes ids:vector<int64> = Chat;
//@description A photo @small Small size
chatPhoto small:string = ChatPhoto;

//@description A node @next Next node
node next:node = Node;

//@description Ok
ok = Ok;

---functions---

//@description Returns a chat. Can be called synchronously @chat_id Chat identifier @type Title type
getChat chat_id:int53 type:RichText = Chat;

//@description Closes the instance
close = Ok;
"#;

    fn generated() -> Generated {
        generate(&parse(SAMPLE).unwrap(), &Options::default()).unwrap()
    }

    #[test]
    fn naming() {
        assert_eq!(pascal("messageText"), "MessageText");
        assert_eq!(snake("getChatHistory"), "get_chat_history");
        assert_eq!(snake("getRTMPUrl"), "get_rtmp_url");
        assert_eq!(snake("getMe"), "get_me");
        assert_eq!(variant_name("AuthorizationState", "authorizationStateReady"), "Ready");
        assert_eq!(variant_name("InputFile", "inputFileId"), "Id");
        assert_eq!(variant_name("MessageContent", "messageText"), "Text");
        assert_eq!(variant_name("InputMessageContent", "inputMessageText"), "Text");
        assert_eq!(variant_name("Update", "updateNewMessage"), "NewMessage");
        assert_eq!(variant_name("Update", "chatTypePrivate"), "ChatTypePrivate");
        assert_eq!(variant_name("Chat", "chat2"), "Chat2");
    }

    #[test]
    fn boxes_recursive_fields_only() {
        let code = generated().types;
        assert!(code.contains("pub text: ::std::boxed::Box<crate::types::RichText>,"));
        assert!(code.contains("pub next: ::std::boxed::Box<crate::types::Node>,"));
        assert!(code.contains("pub rich: crate::types::RichText,"));
    }

    #[test]
    fn nullable_and_special_encodings() {
        let code = generated().types;
        assert!(code.contains("pub photo: ::std::option::Option<crate::types::ChatPhoto>,"));
        assert!(code.contains("with = \"crate::serde_helpers::int64\""));
        assert!(code.contains("with = \"crate::serde_helpers::bytes\""));
        assert!(code.contains("with = \"crate::serde_helpers::vec_int64\""));
    }

    #[test]
    fn enums_have_catch_all_and_conversions() {
        let code = generated().types;
        assert!(code.contains("pub enum RichText {"));
        assert!(code.contains("    Plain(crate::types::RichTextPlain),"));
        assert!(code.contains("    Unrecognized(::serde_json::Value),"));
        assert!(code.contains("impl ::std::convert::From<crate::types::RichTextBold> for RichText {"));
    }

    #[test]
    fn default_is_derived_only_when_possible() {
        let code = generated().types;
        let chat_photo = code.find("pub struct ChatPhoto").unwrap();
        assert!(
            code[..chat_photo].ends_with("Deserialize, Default)]\n#[serde(tag = \"@type\", rename = \"chatPhoto\")]\n")
        );
        let chat = code.find("pub struct Chat {").unwrap();
        assert!(!code[..chat].ends_with("Default)]\n#[serde(tag = \"@type\", rename = \"chat\")]\n"));
    }

    #[test]
    fn functions_and_methods() {
        let generated = generated();
        assert!(generated.functions.contains("impl crate::Function for GetChat {"));
        assert!(generated.functions.contains("impl crate::SyncFunction for GetChat {}"));
        assert!(!generated.functions.contains("impl crate::SyncFunction for Close {}"));
        assert!(generated.methods.contains(
            "pub async fn get_chat(&self, chat_id: i64, r#type: impl ::std::convert::Into<crate::types::RichText>) -> crate::Result<crate::types::Chat> {"
        ));
        assert!(generated.methods.contains("self.send(crate::functions::Close {}).await"));
    }

    #[test]
    fn reserved_method_names_are_rejected() {
        let options = Options { reserved_method_names: vec!["close".into()] };
        let err = generate(&parse(SAMPLE).unwrap(), &options).unwrap_err();
        assert!(err.0.contains("`close`"));
    }
}

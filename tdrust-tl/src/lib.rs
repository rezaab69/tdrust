//! Parser and Rust code generator for TDLib's `td_api.tl` schema.
//!
//! This crate is the build-time half of [`tdrust`](https://docs.rs/tdrust):
//! its build script feeds `td_api.tl` through [`generate_from_source`] and
//! includes the result. It can also be used on its own to inspect a schema:
//!
//! ```
//! let schema = tdrust_tl::parse("//@description Returns the current user\ngetMe = User;\n").unwrap();
//! assert!(schema.functions.is_empty()); // no `---functions---` marker, so it is a type
//! assert_eq!(schema.types[0].name, "getMe");
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod codegen;
pub mod parser;

pub use codegen::{generate, GenerateError, Generated, Options};
pub use parser::{parse, Definition, Field, ParseError, Schema, TlType};

/// Parses `source` and generates code for it in one step.
pub fn generate_from_source(source: &str, options: &Options) -> Result<Generated, Box<dyn std::error::Error>> {
    let schema = parse(source)?;
    Ok(generate(&schema, options)?)
}

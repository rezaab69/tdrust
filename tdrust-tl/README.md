# tdrust-tl

Parser and Rust code generator for TDLib's `td_api.tl` schema.

This crate is used by the build script of [`tdrust`](../README.md) and usually
doesn't need to be used directly. It turns every declaration in `td_api.tl` into
Rust:

| Schema | Generated Rust |
| --- | --- |
| constructor `formattedText text:string ... = FormattedText;` | `struct FormattedText` with serde support |
| abstract type (`//@class MessageContent`) | `enum MessageContent { Text(MessageText), ..., Unrecognized(Value) }` |
| function `getMe = User;` | `struct GetMe` implementing `Function<Output = User>` and `Client::get_me()` |

Encoding rules follow TDLib's JSON interface: every object carries `@type`,
`int64` values are sent as strings, `bytes` values as base64, and object fields
documented as "may be null" become `Option<T>`.

```rust
let source = std::fs::read_to_string("td_api.tl")?;
let generated = tdrust_tl::generate_from_source(&source, &tdrust_tl::Options::default())?;
std::fs::write("types.rs", generated.types)?;
```

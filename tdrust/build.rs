//! Generates the TDLib API bindings from `td_api.tl`.
//!
//! The bundled schema matches `BUNDLED_TDLIB_VERSION` in `src/lib.rs`. To
//! build against a different TDLib version, point `TDRUST_TL_SCHEMA` at the
//! `td/generate/scheme/td_api.tl` file of that version.

use std::env;
use std::fs;
use std::path::PathBuf;

/// Methods defined by hand on `Client`; the generator refuses to emit a
/// TDLib method with the same name.
const RESERVED_METHODS: &[&str] = &[
    "authorization_state",
    "authorize",
    "builder",
    "id",
    "is_closed",
    "reply_text",
    "runtime",
    "send",
    "send_json",
    "send_text",
    "shutdown",
    "wait_closed",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=TDRUST_TL_SCHEMA");

    let schema = env::var_os("TDRUST_TL_SCHEMA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("tl/td_api.tl"));
    println!("cargo:rerun-if-changed={}", schema.display());

    let source = fs::read_to_string(&schema)
        .unwrap_or_else(|err| panic!("cannot read TDLib schema {}: {err}", schema.display()));
    let options =
        tdrust_tl::Options { reserved_method_names: RESERVED_METHODS.iter().map(|name| name.to_string()).collect() };
    let generated = tdrust_tl::generate_from_source(&source, &options)
        .unwrap_or_else(|err| panic!("cannot generate bindings from {}: {err}", schema.display()));

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    fs::write(out.join("types.rs"), generated.types).expect("write types.rs");
    fs::write(out.join("functions.rs"), generated.functions).expect("write functions.rs");
    fs::write(out.join("methods.rs"), generated.methods).expect("write methods.rs");

    if env::var_os("CARGO_FEATURE_LINK").is_some() {
        println!("cargo:rerun-if-env-changed=TDJSON_LIB_DIR");
        if let Some(dir) = env::var_os("TDJSON_LIB_DIR") {
            println!("cargo:rustc-link-search=native={}", PathBuf::from(dir).display());
        }
        println!("cargo:rustc-link-lib=dylib=tdjson");
    }
}

//! Checks that TDLib can be found and demonstrates calls that need no login:
//! synchronous execution, text parsing, and the raw JSON escape hatch.
//!
//! ```sh
//! TDJSON_PATH=/path/to/libtdjson.so cargo run --example offline
//! ```

use serde_json::json;
use tdrust::functions::GetTextEntities;
use tdrust::types::FormattedText;

fn main() -> tdrust::Result<()> {
    println!("TDLib {} (bindings generated for {})", tdrust::tdlib_version()?, tdrust::BUNDLED_TDLIB_VERSION);

    let parsed = FormattedText::markdown("*bold* _italic_ `code` [link](https://telegram.org)")?;
    println!("\nMarkdown parsed into {:?}", parsed.text);
    for entity in &parsed.entities {
        println!("  {:>2}..{:<2} {}", entity.offset, entity.offset + entity.length, entity.r#type.type_name());
    }

    let found = tdrust::execute(GetTextEntities { text: "Mail @durov or visit t.me/telegram #news".into() })?;
    println!("\nEntities found in plain text:");
    for entity in found.entities {
        println!("  {:>2}..{:<2} {}", entity.offset, entity.offset + entity.length, entity.r#type.type_name());
    }

    // Raw JSON works for anything, including functions newer than the bindings.
    let runtime = tdrust::Runtime::global()?;
    let raw = runtime.execute_json(json!({"@type": "getOption", "name": "version"}))?;
    println!("\nRaw JSON response: {raw}");
    Ok(())
}

//! Prints which system font family fontique picks for Han characters, per
//! language, and whether a few well-known CJK families are visible to it.
//!
//! Run it on each test machine: `cargo run -p g1-ime --example fallback_probe`.
//! The order of the queries matters, which is itself the finding: "no language"
//! and "zh-Hans" share one cached answer, so the first one asked decides both.

use parley::FontContext;
use parley::fontique::{FallbackKey, Script};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let untagged_first = args.iter().any(|arg| arg == "--untagged-first");
    let mut order = vec![Some("zh-Hans"), None, Some("zh-Hant"), Some("ja")];
    if untagged_first {
        order.swap(0, 1);
    }

    let mut cx = FontContext::default();
    let han = Script::from_bytes(*b"Hani");
    println!("Han fallback, in query order:");
    for language in order {
        let key: FallbackKey = match language {
            Some(language) => (han, language).into(),
            None => han.into(),
        };
        let ids: Vec<_> = cx.collection.fallback_families(key).collect();
        let names: Vec<String> = ids
            .into_iter()
            .map(|id| cx.collection.family_name(id).unwrap_or("?").to_owned())
            .collect();
        println!(
            "  {:<10} resolved as {:<8} -> {names:?}",
            language.unwrap_or("(none)"),
            key.locale_str().unwrap_or("-"),
        );
    }

    println!("Families visible to fontique:");
    for name in [
        "PingFang SC",
        "Hiragino Sans GB",
        "Heiti SC",
        "Songti SC",
        "Hiragino Sans",
        "Microsoft YaHei",
        "SimSun",
        "Yu Gothic",
        "Noto Sans CJK SC",
        "Noto Sans CJK JP",
    ] {
        let found = cx.collection.family_id(name).is_some();
        println!("  {name:<18} {}", if found { "yes" } else { "no" });
    }
}

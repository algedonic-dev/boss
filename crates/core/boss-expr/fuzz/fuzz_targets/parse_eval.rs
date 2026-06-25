#![no_main]

use libfuzzer_sys::fuzz_target;

// Fuzz the boss-expr DSL end to end. The language is total by design —
// boolean expressions over field access, no arithmetic, no loops, no
// external state — so the contract is that parse → references → eval
// never panics: every failure must surface as a ParseError/EvalError,
// not a crash. This target asserts that contract over arbitrary input.
//
// Run with: cargo +nightly fuzz run parse_eval
fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(expr) = boss_expr::parse(src) {
        // Structure walk used by boss-jobs to derive the DAG — must be
        // total over any well-formed AST.
        let _ = boss_expr::references(&expr);
        // Evaluate against an empty payload with no helpers registered;
        // identifier/helper misses must come back as Err, never a panic.
        let payload = serde_json::Value::Object(serde_json::Map::new());
        let ctx = boss_expr::Context {
            payload: &payload,
            helpers: &boss_expr::NoHelpers,
        };
        let _ = boss_expr::eval(&expr, &ctx);
    }
});

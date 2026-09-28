//! Rule 7 of the builder rules must tell a builder whose car waits for
//! an adversarial review to gate it with `--hold` BESIDE its `--park-*`
//! flags, now that the gate carries the pair.
//!
//! MEASURED (backlog 486dde37, 2026-09-26). Car 92a3346c
//! (`fix/a-gate-carries-park-intent-and-a-hold-together`) taught
//! `boss gate` to accept `--hold` with park intent: `jobs.auto-park`
//! files the car on green and writes the hold onto its review step, so
//! it stands at the dock HELD and leaves only through `boss release`.
//! But rule 7 — the gate command every dispatched builder types — named
//! the `--park-*` flags and never `--hold`, so a trust-boundary builder
//! still gated unheld and the car boarded on depth within minutes of
//! its green, before the operator's `boss hold` (the race the item was
//! filed for: three trust-boundary cars hit it on 2026-09-26). The door
//! existed; the rule that hands builders their command did not show it.
//!
//! CLAUDE.md §9a: whether the gate carries the pair is not retyped here.
//! The `hold` field's own help on the `Gate` variant (what `boss gate
//! --help` prints) is read with `syn`: while it says the hold rides WITH
//! `--park-*`, rule 7 must name `--hold` and `boss release`. If the gate
//! ever refuses the pair again, that help changes and this pin fails by
//! name rather than leaving rule 7 recommending a refused command.

use boss_testing::repo_root;
use syn::visit::Visit;

const MAIN: &str = "crates/orchestrators/boss-cli/src/main.rs";
const RULES: &str = "infra/platform/documents/builder-rules.md";

/// The `///` doc of the `hold` field of the `Gate` variant — the text
/// clap renders as `boss gate --help` for `--hold`.
fn gate_hold_help() -> String {
    struct Find(Option<String>);
    impl<'ast> Visit<'ast> for Find {
        fn visit_variant(&mut self, v: &'ast syn::Variant) {
            if v.ident != "Gate" {
                return;
            }
            let Some(field) = v
                .fields
                .iter()
                .find(|f| f.ident.as_ref().is_some_and(|i| i == "hold"))
            else {
                return;
            };
            let doc = field
                .attrs
                .iter()
                .filter(|a| a.path().is_ident("doc"))
                .filter_map(|a| match &a.meta {
                    syn::Meta::NameValue(syn::MetaNameValue {
                        value:
                            syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(s),
                                ..
                            }),
                        ..
                    }) => Some(s.value()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.0 = Some(doc);
        }
    }
    let src = std::fs::read_to_string(repo_root().join(MAIN)).expect("main.rs is readable");
    let file = syn::parse_file(&src).expect("main.rs parses");
    let mut find = Find(None);
    find.visit_file(&file);
    find.0
        .unwrap_or_else(|| panic!("no `hold` field on the `Gate` variant in {MAIN}"))
}

/// Rule 7's own text: from its number to the next rule's.
fn rule_seven() -> String {
    let text = std::fs::read_to_string(repo_root().join(RULES)).expect("the builder rules");
    let start = text.find("\n7. ").expect("the builder rules have a rule 7");
    let rest = &text[start + 1..];
    let end = rest.find("\n8. ").expect("the builder rules have a rule 8");
    rest[..end].to_string()
}

#[test]
fn rule_seven_gates_a_trust_boundary_car_held_beside_its_park_intent() {
    // Collapsed to single spaces: the doc wraps, and "`boss release`"
    // is split across two `///` lines there today.
    let help = gate_hold_help()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        help.contains("--park-") && help.contains("boss release"),
        "the help for `boss gate --hold` in {MAIN} no longer says the hold rides \
         WITH `--park-*` and is left through `boss release` — if the gate refuses \
         the pair again, rule 7 of {RULES} must stop recommending it; if the \
         sentence only moved, this pin must follow it. Help read: {help:?}"
    );
    let rule = rule_seven();
    for needle in ["--hold", "boss release", "trust-boundary"] {
        assert!(
            rule.contains(needle),
            "rule 7 of {RULES} must name `{needle}`: a builder whose car waits for \
             an adversarial review gates it with `--hold '<reason>'` beside its \
             `--park-*` flags, or the car boards on depth before the operator can \
             hold it (backlog 486dde37)"
        );
    }
}

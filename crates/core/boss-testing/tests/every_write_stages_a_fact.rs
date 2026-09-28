//! Every database write stages a fact in the same transaction, or is
//! named on a justified allowlist (backlog 8c271e8f, design 3036296f
//! mechanism A, David approved 2026-09-27).
//!
//! WHY. The log is the system of record (CLAUDE.md §Events: "every
//! state-changing operation publishes an event; nothing else"), and
//! editing or retiring a Class was a bare `UPDATE classes` on the pool
//! until 2026-09-27 (10dabe13) — found by an adversarial review, after it
//! shipped. Nothing in the gate could see the shape: a Postgres write
//! whose function stages nothing on the outbox. (The design also named
//! 42da8bd2, a tenant publish through the machine door that left no
//! stamp. Measured against the file it came from, that verb's database
//! path already staged `tenant.published` in the stamp's transaction;
//! the door route skipped the write altogether, and a write that never
//! happens has no site for this pin to read. It is not this pin's class.)
//!
//! WHAT IS A WRITE. A string literal in non-test code under
//! `crates/*/*/src/` — a plain literal, or one inside a macro such as
//! `sqlx::query!` or `format!` — carrying `INSERT INTO <t>`,
//! `UPDATE <t> SET`, `DELETE FROM <t>` or `TRUNCATE <t>`. A literal in a
//! `const` counts against every function in its file that names the
//! const; one no function names is a write nobody can attribute, and is
//! reported as its own site. Test code (`tests/`, `#[cfg(test)]`,
//! `#[test]`) is not a write path.
//!
//! WHAT IS STAGING A FACT. A function stages when its body calls
//! `boss_events::outbox::record_event_in_tx`, carries the outbox's own
//! `INSERT INTO event_outbox`, or calls — by name, within its crate — a
//! staging HELPER: a function that stages and takes its caller's
//! transaction (a `Transaction`, a `*Connection`, an `Executor`). A function
//! that stages in a transaction of its own covers its own writes and no
//! caller's. A function that writes and stages nothing is still covered
//! when it writes on its caller's behalf — takes the caller's
//! transaction, or returns the SQL for the caller to run — and it has at
//! least one caller (by bare name in its crate, or crate-qualified from
//! another: `boss_subject_kinds::subjects::record_subject_in_tx`) and
//! EVERY caller is covered or excused. One fact-less caller uncovers it,
//! and the failure names that caller.
//!
//! WHAT THIS CANNOT SEE, said so nobody reads more into a green than it
//! holds: it reads names, not types — "the same transaction" is judged
//! by the write and the stage sharing a function (or a caller chain),
//! not by proving they bind the same `Transaction`; and a write assembled
//! from pieces no single literal carries (a table name in one literal,
//! `SET` in another) is invisible. Triggers inside the database
//! (infra/postgres/schema) are out of its reach entirely.
//!
//! THE ALLOWLIST is ONE file, `crates/core/boss-events/writes-without-a-fact.txt`,
//! and every entry carries a reason. It is for writes that are not
//! facts (`not-a-fact`: projections rebuilt from the log, telemetry,
//! sessions, outbox bookkeeping) and for the gaps measured when this pin
//! landed (`gap`), each a debt with an item to retire it, not a decision
//! that the write needs no fact. An entry that excuses
//! nothing fails the pin by name: a stale allowance is a hole shaped
//! like a write that no longer exists (infra/lint/lib/allowlist.sh
//! makes the same argument for the lints).
//!
//! tree-wide pin — it scans every crate's `src/`, so no changed-file map
//! can attribute it to this crate, and every scoped gate runs it
//! whatever its scope (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use proc_macro2::{TokenStream, TokenTree};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use syn::visit::Visit;

/// The one allowlist, repo-relative.
const ALLOWLIST: &str = "crates/core/boss-events/writes-without-a-fact.txt";

/// The call that stages a fact — the one door every staging path goes
/// through (crates/core/boss-events/src/outbox.rs).
const STAGE_CALL: &str = "record_event_in_tx";

/// The parameter types that hand a function its caller's transaction,
/// matched as a type name's SUFFIX (sqlx's `Transaction`, its Postgres
/// and `Any` connections, an `Executor` or `Acquire` bound). A function
/// taking none of these opens its own (or writes on the pool), so
/// whatever its caller stages is NOT in the same transaction.
const TX_TYPES: [&str; 4] = ["Transaction", "Connection", "Executor", "Acquire"];

/// One write statement found in a function.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Write {
    verb: &'static str,
    table: String,
    line: usize,
}

/// One function (or an unattributed `const`) that holds SQL.
#[derive(Debug, Default, Clone)]
struct Func {
    /// Repo-relative file.
    file: String,
    /// `crates/<tier>/<name>` — the resolution scope for calls.
    krate: String,
    /// The bare name calls are resolved by.
    name: String,
    /// `Type::name` inside an impl, `name` otherwise — the allowlist key.
    qual: String,
    writes: Vec<Write>,
    /// Every identifier the body names (paths, consts) — how a const
    /// held write is attributed to the functions that use it.
    idents: BTreeSet<String>,
    /// Every name the body CALLS (`f(..)`, `x.f(..)`, `f!(..)`'s inner
    /// calls) — how calls within a crate are resolved.
    calls: BTreeSet<String>,
    /// Every crate-qualified call, as `<first segment>::<name>`
    /// (`boss_subject_kinds::record_subject_in_tx`) — how a call into
    /// ANOTHER crate is resolved, since a bare name cannot say whose.
    qualified_calls: BTreeSet<String>,
    /// Writes on its caller's behalf: takes the caller's transaction or
    /// connection, or returns the SQL text for the caller to run. Only
    /// such a function writes or stages INSIDE its caller's transaction.
    for_caller: bool,
    /// Calls `record_event_in_tx` or inserts into `event_outbox` itself.
    stages_directly: bool,
}

fn write_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // UPDATE takes an optional alias before SET; requiring SET is
        // what keeps `FOR UPDATE SKIP LOCKED` and `DO UPDATE SET` (an
        // upsert's conflict arm, already counted as its INSERT) out.
        Regex::new(
            r#"(?x)
            \b(?:
                INSERT \s+ INTO \s+ (?:ONLY\s+)? (?P<ins>[\w."{}]+)
              | DELETE \s+ FROM \s+ (?:ONLY\s+)? (?P<del>[\w."{}]+)
              | UPDATE \s+ (?:ONLY\s+)? (?P<upd>[\w."{}]+) (?:\s+(?:AS\s+)?\w+)? \s+ SET \b
              | TRUNCATE \s+ (?:TABLE\s+)? (?:ONLY\s+)? (?P<tru>[\w."{}]+)
            )"#,
        )
        .expect("the write pattern compiles")
    })
}

/// Normalise a table token: drop quoting and a `public.` schema.
fn table_name(raw: &str) -> String {
    let t = raw.replace('"', "");
    let t = t.strip_prefix("public.").unwrap_or(&t);
    t.trim_end_matches([',', ';']).to_string()
}

/// The writes one literal carries, and whether it is the outbox INSERT.
fn writes_in(text: &str, line: usize) -> (Vec<Write>, bool) {
    let mut writes = Vec::new();
    let mut stages = false;
    for caps in write_regex().captures_iter(text) {
        let (verb, raw) = if let Some(m) = caps.name("ins") {
            ("INSERT", m.as_str())
        } else if let Some(m) = caps.name("del") {
            ("DELETE", m.as_str())
        } else if let Some(m) = caps.name("upd") {
            ("UPDATE", m.as_str())
        } else if let Some(m) = caps.name("tru") {
            ("TRUNCATE", m.as_str())
        } else {
            continue;
        };
        let table = table_name(raw);
        if table.eq_ignore_ascii_case("SET") {
            continue;
        }
        if verb == "INSERT" && table == "event_outbox" {
            // Staging the fact is not a write that owes one.
            stages = true;
            continue;
        }
        writes.push(Write { verb, table, line });
    }
    (writes, stages)
}

/// `#[cfg(test)]` in any of its shapes, and the test attributes.
fn is_test_code(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        let last = path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        if last == "test" {
            return true;
        }
        if last != "cfg" && last != "cfg_attr" {
            return false;
        }
        attr.meta
            .require_list()
            .map(|list| {
                list.tokens
                    .to_string()
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|t| t == "test")
            })
            .unwrap_or(false)
    })
}

struct Scan {
    file: String,
    krate: String,
    /// Self type of the impl currently open.
    impl_ty: Vec<String>,
    /// Functions currently open, innermost last.
    open: Vec<Func>,
    /// The `const` / `static` currently open outside any function.
    open_const: Option<String>,
    /// Write-carrying consts by name.
    consts: BTreeMap<String, (Vec<Write>, bool)>,
    done: Vec<Func>,
}

impl Scan {
    fn literal(&mut self, value: &str, line: usize) {
        let (writes, stages) = writes_in(value, line);
        if writes.is_empty() && !stages {
            return;
        }
        if let Some(f) = self.open.last_mut() {
            f.writes.extend(writes);
            f.stages_directly |= stages;
        } else if let Some(name) = &self.open_const {
            let entry = self.consts.entry(name.clone()).or_default();
            entry.0.extend(writes);
            entry.1 |= stages;
        }
    }

    fn ident(&mut self, ident: &str) {
        if let Some(f) = self.open.last_mut() {
            f.idents.insert(ident.to_string());
        }
    }

    fn call(&mut self, name: &str) {
        if let Some(f) = self.open.last_mut() {
            if name == STAGE_CALL {
                f.stages_directly = true;
            }
            f.calls.insert(name.to_string());
        }
    }

    fn tokens(&mut self, tokens: &TokenStream) {
        let trees: Vec<TokenTree> = tokens.clone().into_iter().collect();
        for (i, tree) in trees.iter().enumerate() {
            match tree {
                TokenTree::Group(g) => self.tokens(&g.stream()),
                TokenTree::Ident(id) => {
                    let name = id.to_string();
                    self.ident(&name);
                    // Inside a macro's tokens a call is an ident followed
                    // by a parenthesised group.
                    if let Some(TokenTree::Group(g)) = trees.get(i + 1)
                        && g.delimiter() == proc_macro2::Delimiter::Parenthesis
                    {
                        self.call(&name);
                    }
                }
                TokenTree::Literal(lit) => {
                    if let syn::Lit::Str(s) = syn::Lit::new(lit.clone()) {
                        self.literal(&s.value(), lit.span().start().line);
                    }
                }
                TokenTree::Punct(_) => {}
            }
        }
    }

    fn open_fn(&mut self, sig: &syn::Signature) {
        let name = sig.ident.to_string();
        let qual = match self.impl_ty.last() {
            Some(ty) if !ty.is_empty() => format!("{ty}::{name}"),
            _ => name.clone(),
        };
        // The names the parameter and generic types spell.
        struct Names(BTreeSet<String>);
        impl<'ast> Visit<'ast> for Names {
            fn visit_ident(&mut self, i: &'ast proc_macro2::Ident) {
                self.0.insert(i.to_string());
            }
        }
        let mut names = Names(BTreeSet::new());
        for input in &sig.inputs {
            names.visit_fn_arg(input);
        }
        names.visit_generics(&sig.generics);
        let takes_tx = names
            .0
            .iter()
            .any(|n| TX_TYPES.iter().any(|t| n.ends_with(t)));
        // A function that RETURNS text builds SQL for its caller to run
        // (`insert_run_sql() -> String`): like a const, its write is
        // the caller's.
        let mut output = Names(BTreeSet::new());
        output.visit_return_type(&sig.output);
        let builds_sql = output.0.contains("String") || output.0.contains("str");
        self.open.push(Func {
            file: self.file.clone(),
            krate: self.krate.clone(),
            name,
            qual,
            for_caller: takes_tx || builds_sql,
            ..Func::default()
        });
    }

    fn close_fn(&mut self) {
        if let Some(f) = self.open.pop() {
            self.done.push(f);
        }
    }
}

impl<'ast> Visit<'ast> for Scan {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let attrs: &[syn::Attribute] = match item {
            syn::Item::Mod(i) => &i.attrs,
            syn::Item::Fn(i) => &i.attrs,
            syn::Item::Impl(i) => &i.attrs,
            syn::Item::Const(i) => &i.attrs,
            syn::Item::Static(i) => &i.attrs,
            syn::Item::Trait(i) => &i.attrs,
            syn::Item::Macro(i) => &i.attrs,
            _ => &[],
        };
        if is_test_code(attrs) {
            return;
        }
        syn::visit::visit_item(self, item);
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let attrs: &[syn::Attribute] = match item {
            syn::ImplItem::Fn(i) => &i.attrs,
            syn::ImplItem::Const(i) => &i.attrs,
            _ => &[],
        };
        if is_test_code(attrs) {
            return;
        }
        syn::visit::visit_impl_item(self, item);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let ty = match &*node.self_ty {
            syn::Type::Path(p) => p
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.impl_ty.push(ty);
        syn::visit::visit_item_impl(self, node);
        self.impl_ty.pop();
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.impl_ty.push(node.ident.to_string());
        syn::visit::visit_item_trait(self, node);
        self.impl_ty.pop();
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        // A free fn nested in an impl method is not that impl's method.
        self.impl_ty.push(String::new());
        self.open_fn(&node.sig);
        syn::visit::visit_item_fn(self, node);
        self.close_fn();
        self.impl_ty.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.open_fn(&node.sig);
        syn::visit::visit_impl_item_fn(self, node);
        self.close_fn();
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        if is_test_code(&node.attrs) {
            return;
        }
        self.open_fn(&node.sig);
        syn::visit::visit_trait_item_fn(self, node);
        self.close_fn();
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        let outer = self.open_const.replace(node.ident.to_string());
        syn::visit::visit_item_const(self, node);
        self.open_const = outer;
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        let outer = self.open_const.replace(node.ident.to_string());
        syn::visit::visit_item_static(self, node);
        self.open_const = outer;
    }

    fn visit_impl_item_const(&mut self, node: &'ast syn::ImplItemConst) {
        let outer = self.open_const.replace(node.ident.to_string());
        syn::visit::visit_impl_item_const(self, node);
        self.open_const = outer;
    }

    fn visit_attribute(&mut self, _: &'ast syn::Attribute) {
        // A doc comment is prose: "a bare `UPDATE classes SET`" in one
        // is a sentence about a write, not a write.
    }

    fn visit_lit_str(&mut self, node: &'ast syn::LitStr) {
        self.literal(&node.value(), node.span().start().line);
    }

    fn visit_ident(&mut self, node: &'ast proc_macro2::Ident) {
        self.ident(&node.to_string());
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*node.func
            && let Some(last) = p.path.segments.last()
        {
            self.call(&last.ident.to_string());
            if let (Some(first), true) = (p.path.segments.first(), p.path.segments.len() > 1)
                && let Some(f) = self.open.last_mut()
            {
                f.qualified_calls
                    .insert(format!("{}::{}", first.ident, last.ident));
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        self.call(&node.method.to_string());
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        syn::visit::visit_macro(self, node);
        self.tokens(&node.tokens);
    }
}

/// Every function in one source that holds SQL or names anything, with
/// const-held writes folded into the functions that name the const.
fn scan_source(file: &str, krate: &str, source: &str) -> Result<Vec<Func>, syn::Error> {
    let parsed = syn::parse_file(source)?;
    let mut scan = Scan {
        file: file.to_string(),
        krate: krate.to_string(),
        impl_ty: Vec::new(),
        open: Vec::new(),
        open_const: None,
        consts: BTreeMap::new(),
        done: Vec::new(),
    };
    scan.visit_file(&parsed);
    let mut funcs = scan.done;
    let mut named: BTreeSet<String> = BTreeSet::new();
    for f in &mut funcs {
        for (name, (writes, stages)) in &scan.consts {
            if f.idents.contains(name) {
                named.insert(name.clone());
                f.writes.extend(writes.iter().cloned());
                f.stages_directly |= *stages;
            }
        }
    }
    // A const-held write no function in its file names: reported as its
    // own site, so it can neither hide nor be excused by accident.
    for (name, (writes, stages)) in scan.consts {
        if !named.contains(&name) && !writes.is_empty() {
            funcs.push(Func {
                file: file.to_string(),
                krate: krate.to_string(),
                name: name.clone(),
                qual: format!("const {name}"),
                writes,
                stages_directly: stages,
                ..Func::default()
            });
        }
    }
    Ok(funcs)
}

/// One write the pin judges.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Site {
    file: String,
    qual: String,
    table: String,
    verb: &'static str,
    line: usize,
    /// Why it is uncovered, in the reader's terms.
    why: String,
}

/// Whether an allowlist entry says the write is not a fact, or is a
/// write that owes one and does not stage it yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Not a fact: a projection rebuilt from the log, a cache, a
    /// session, telemetry, outbox bookkeeping.
    NotAFact,
    /// A real gap, measured when this pin landed: the write changes
    /// company state and stages nothing. Each is a debt with an item to
    /// retire it — never a decision that the write needs no fact.
    Gap,
}

/// What an allowlist entry names.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    /// `fn <file>::<qual>`: one function, and every write it holds.
    Fn(String),
    /// `file <file>`: every function in one file (a rebuilder module).
    File(String),
    /// `table <name>`: every write to one table, wherever it is.
    Table(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Allow {
    line: usize,
    status: Status,
    scope: Scope,
}

/// Parse the allowlist; every malformed or reasonless line is an error
/// naming its line number.
fn parse_allowlist(text: &str) -> Result<Vec<Allow>, Vec<String>> {
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let n = n + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((head, reason)) = line.split_once(" -- ") else {
            errors.push(format!(
                "line {n}: no ` -- <reason>` — every entry carries its reason"
            ));
            continue;
        };
        if reason.trim().len() < 20 {
            errors.push(format!(
                "line {n}: the reason `{}` is too short to be one",
                reason.trim()
            ));
            continue;
        }
        let fields: Vec<&str> = head.split_whitespace().collect();
        let status = match fields.first() {
            Some(&"not-a-fact") => Status::NotAFact,
            Some(&"gap") => Status::Gap,
            _ => {
                errors.push(format!(
                    "line {n}: an entry starts `not-a-fact` or `gap`, got `{head}`"
                ));
                continue;
            }
        };
        let scope = match &fields[1..] {
            ["fn", site] if site.contains(".rs::") => Scope::Fn((*site).to_string()),
            ["file", path] if path.ends_with(".rs") => Scope::File((*path).to_string()),
            ["table", table] => Scope::Table((*table).to_string()),
            _ => {
                errors.push(format!(
                    "line {n}: expected `fn <file>::<fn>`, `file <file>` or `table <name>` \
                     after the status, got `{head}`"
                ));
                continue;
            }
        };
        entries.push(Allow {
            line: n,
            status,
            scope,
        });
    }
    if errors.is_empty() {
        Ok(entries)
    } else {
        Err(errors)
    }
}

/// The pin's answer over a set of functions and an allowlist.
#[derive(Debug, Default)]
struct Judgement {
    /// Writes that stage no fact and that no entry excuses.
    unexcused: Vec<Site>,
    /// Every write found, covered or not — the pin's control count.
    writes: usize,
    /// Allowlist lines that excused something.
    used: BTreeSet<usize>,
    /// Functions read as staging, by `file::qual` — a control.
    staging: BTreeSet<String>,
}

fn judge(funcs: &[Func], allow: &[Allow]) -> Judgement {
    // Staging helpers, to a fixpoint: a function that stages AND works
    // in its caller's transaction stages in that transaction, so calling
    // it (by name, within the crate) is staging. One that opens its own
    // transaction stages in THAT one, which covers its own writes and
    // nobody else's — resolving every name that ever staged anything
    // made `new`, `get` and `router` stagers and covered everything.
    let mut helpers: BTreeSet<(String, String)> = BTreeSet::new();
    let stages_with = |f: &Func, helpers: &BTreeSet<(String, String)>| {
        f.stages_directly
            || f.calls
                .iter()
                .any(|c| c != &f.name && helpers.contains(&(f.krate.clone(), c.clone())))
    };
    loop {
        let before = helpers.len();
        for f in funcs {
            if f.for_caller && stages_with(f, &helpers) {
                helpers.insert((f.krate.clone(), f.name.clone()));
            }
        }
        if helpers.len() == before {
            break;
        }
    }
    let stages: Vec<bool> = funcs.iter().map(|f| stages_with(f, &helpers)).collect();

    // The entry (if any) that excuses a whole function.
    let excused_by = |f: &Func| -> Option<usize> {
        let key = format!("{}::{}", f.file, f.qual);
        allow
            .iter()
            .find(|a| match &a.scope {
                Scope::Fn(k) => *k == key,
                Scope::File(p) => *p == f.file,
                Scope::Table(_) => false,
            })
            .map(|a| a.line)
    };

    // Who calls whom: within a crate by bare name, across crates by
    // `<crate>::<name>` (the crate's directory with `-` read as `_`).
    let crate_ident = |krate: &str| krate.rsplit('/').next().unwrap_or(krate).replace('-', "_");
    let callers_of = |idx: usize| -> Vec<usize> {
        let f = &funcs[idx];
        let qualified = format!("{}::{}", crate_ident(&f.krate), f.name);
        funcs
            .iter()
            .enumerate()
            .filter(|(j, g)| {
                *j != idx
                    && ((g.krate == f.krate && g.calls.contains(&f.name))
                        || g.qualified_calls.contains(&qualified))
            })
            .map(|(j, _)| j)
            .collect()
    };

    // Covered: stages, or is excused whole, or writes on its caller's
    // behalf and EVERY caller is covered (and there is at least one).
    // A cycle counts as not covered.
    fn covered(
        idx: usize,
        funcs: &[Func],
        stages: &[bool],
        excused: &dyn Fn(&Func) -> Option<usize>,
        callers_of: &dyn Fn(usize) -> Vec<usize>,
        seen: &mut Vec<usize>,
        memo: &mut BTreeMap<usize, bool>,
    ) -> bool {
        if let Some(v) = memo.get(&idx) {
            return *v;
        }
        let f = &funcs[idx];
        if stages[idx] || excused(f).is_some() {
            memo.insert(idx, true);
            return true;
        }
        if !f.for_caller || seen.contains(&idx) {
            return false;
        }
        seen.push(idx);
        let callers = callers_of(idx);
        let ok = !callers.is_empty()
            && callers
                .iter()
                .all(|&j| covered(j, funcs, stages, excused, callers_of, seen, memo));
        seen.pop();
        memo.insert(idx, ok);
        ok
    }

    let mut out = Judgement::default();
    let mut memo = BTreeMap::new();
    // Functions that call a writing helper: an entry excusing one of
    // them excuses the writes it drives, so it is not stale.
    let mut drives_writes: BTreeSet<usize> = BTreeSet::new();
    for (idx, f) in funcs.iter().enumerate() {
        if stages[idx] {
            out.staging.insert(format!("{}::{}", f.file, f.qual));
        }
        if f.writes.is_empty() {
            continue;
        }
        if f.for_caller {
            drives_writes.extend(callers_of(idx));
        }
        out.writes += f.writes.len();
        if covered(
            idx,
            funcs,
            &stages,
            &excused_by,
            &callers_of,
            &mut Vec::new(),
            &mut memo,
        ) {
            continue;
        }
        // Say WHY, for a helper: which of its callers left it uncovered.
        // A verdict the reader must re-derive is not a verdict.
        let why = if f.for_caller {
            let bare: Vec<String> = callers_of(idx)
                .into_iter()
                .filter(|&j| {
                    !covered(
                        j,
                        funcs,
                        &stages,
                        &excused_by,
                        &callers_of,
                        &mut Vec::new(),
                        &mut memo,
                    )
                })
                .map(|j| format!("{}::{}", funcs[j].file, funcs[j].qual))
                .collect();
            if bare.is_empty() {
                "writes for a caller, and no caller was found".to_string()
            } else {
                format!(
                    "writes in its caller's transaction, and these callers stage nothing: {}",
                    bare.join(", ")
                )
            }
        } else {
            "stages no fact".to_string()
        };
        for w in &f.writes {
            let by_table: Vec<usize> = allow
                .iter()
                .filter(|a| a.scope == Scope::Table(w.table.clone()))
                .map(|a| a.line)
                .collect();
            if by_table.is_empty() {
                out.unexcused.push(Site {
                    file: f.file.clone(),
                    qual: f.qual.clone(),
                    table: w.table.clone(),
                    verb: w.verb,
                    line: w.line,
                    why: why.clone(),
                });
            }
            out.used.extend(by_table);
        }
    }
    // An entry excusing a function is used when that function is still
    // there, still stages nothing, and writes or drives a write.
    for (idx, f) in funcs.iter().enumerate() {
        if stages[idx] || (f.writes.is_empty() && !drives_writes.contains(&idx)) {
            continue;
        }
        if let Some(line) = excused_by(f) {
            out.used.insert(line);
        }
    }
    out
}

/// `.rs` files under `dir`, test directories excluded by path.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" && name != "benches" && name != "target" {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// Every function under `crates/*/*/src/`, and the count of files read.
fn scan_tree(root: &Path) -> (Vec<Func>, usize) {
    let mut funcs = Vec::new();
    let mut files = 0;
    let mut unparsable = Vec::new();
    let mut tiers: Vec<PathBuf> = std::fs::read_dir(root.join("crates"))
        .expect("crates/ is readable")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    tiers.sort();
    for tier in tiers {
        let mut krates: Vec<PathBuf> = std::fs::read_dir(&tier)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        krates.sort();
        for krate in krates {
            let krate_rel = krate
                .strip_prefix(root)
                .unwrap_or(&krate)
                .display()
                .to_string();
            let mut paths = Vec::new();
            rust_files(&krate.join("src"), &mut paths);
            paths.sort();
            for path in paths {
                files += 1;
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                let source = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{rel} is readable: {e}"));
                match scan_source(&rel, &krate_rel, &source) {
                    Ok(found) => funcs.extend(found),
                    Err(e) => unparsable.push(format!("{rel}: {e}")),
                }
            }
        }
    }
    assert!(
        unparsable.is_empty(),
        "a file this pin cannot parse is a file whose writes it cannot see:\n  {}",
        unparsable.join("\n  ")
    );
    (funcs, files)
}

const FIXTURE_FILE: &str = "crates/x/boss-y/src/lib.rs";

fn judge_source(source: &str, allow: &str) -> Judgement {
    let funcs = scan_source(FIXTURE_FILE, "crates/x/boss-y", source).expect("the fixture parses");
    judge(
        &funcs,
        &parse_allowlist(allow).expect("the fixture allowlist parses"),
    )
}

fn tables(j: &Judgement) -> Vec<(String, String)> {
    let mut t: Vec<(String, String)> = j
        .unexcused
        .iter()
        .map(|s| (s.qual.clone(), s.table.clone()))
        .collect();
    t.sort();
    t
}

#[test]
fn a_bare_write_on_the_pool_is_a_write_without_a_fact() {
    // The shape of 10dabe13 as it stood before the fix: the classes
    // adapter's `update` was one UPDATE on the pool and nothing else.
    // (The fixtures spell no `Pg`-prefixed type: the lint
    // a-pg-test-declares-its-feature reads one as reaching the Postgres
    // adapter, and this test reaches none.)
    let j = judge_source(
        r#"
        impl ClassRepository for SqlClasses {
            async fn update(&self, class: &Class) -> Result<bool, ClassError> {
                let result = sqlx::query(
                    "UPDATE classes SET \
                     display_name = $3, parent_code = $4 \
                     WHERE subject_kind = $1 AND code = $2",
                )
                .execute(&self.pool)
                .await?;
                Ok(result.rows_affected() > 0)
            }
        }
        "#,
        "",
    );
    assert_eq!(j.writes, 1);
    assert_eq!(j.unexcused.len(), 1, "{:?}", j.unexcused);
    let site = &j.unexcused[0];
    assert_eq!(
        (site.qual.as_str(), site.table.as_str(), site.verb),
        ("SqlClasses::update", "classes", "UPDATE")
    );
    assert_eq!(
        site.line, 5,
        "the line of the literal, for the reader to go and look"
    );
}

#[test]
fn a_write_that_stages_its_fact_is_covered() {
    // The same function after the fix: the UPDATE and the fact share a
    // transaction.
    let j = judge_source(
        r#"
        impl ClassRepository for SqlClasses {
            async fn update(&self, class: &Class, stamp: &EventStamp) -> Result<bool, ClassError> {
                let mut tx = self.pool.begin().await?;
                sqlx::query("UPDATE classes SET display_name = $3 WHERE code = $2")
                    .execute(&mut *tx)
                    .await?;
                boss_events::outbox::record_event_in_tx(&mut tx, &event).await?;
                tx.commit().await?;
                Ok(true)
            }
        }
        "#,
        "",
    );
    assert_eq!(j.writes, 1);
    assert!(j.unexcused.is_empty(), "{:?}", j.unexcused);
}

const HELPER_AND_STAGING_CALLER: &str = r#"
    async fn insert_row(tx: &mut Transaction<'_, Postgres>, r: &Row) -> Result<(), E> {
        sqlx::query("INSERT INTO widgets (id) VALUES ($1)").execute(&mut **tx).await?;
        Ok(())
    }
    async fn stage(tx: &mut Transaction<'_, Postgres>, e: &Event) -> Result<(), E> {
        boss_events::outbox::record_event_in_tx(tx, e).await
    }
    pub async fn create(pool: &Pool, r: &Row) -> Result<(), E> {
        let mut tx = pool.begin().await?;
        insert_row(&mut tx, r).await?;
        stage(&mut tx, &event_for(r)).await?;
        tx.commit().await
    }
"#;

#[test]
fn a_helper_is_covered_only_while_every_caller_stages() {
    let j = judge_source(HELPER_AND_STAGING_CALLER, "");
    assert!(
        j.unexcused.is_empty(),
        "a helper whose one caller stages (through a wrapper that takes the \
         transaction) is covered: {:?}",
        j.unexcused
    );

    // A second caller that stages nothing uncovers the helper.
    let with_backfill = format!(
        "{HELPER_AND_STAGING_CALLER}
        pub async fn backfill(pool: &Pool, r: &Row) -> Result<(), E> {{
            let mut tx = pool.begin().await?;
            insert_row(&mut tx, r).await?;
            tx.commit().await
        }}"
    );
    let j = judge_source(&with_backfill, "");
    assert_eq!(
        tables(&j),
        vec![("insert_row".to_string(), "widgets".to_string())]
    );

    // ...unless that caller is itself excused — a rebuilder replaying
    // the log owes no fact, and neither do the writes it drives.
    let j = judge_source(
        &with_backfill,
        "not-a-fact fn crates/x/boss-y/src/lib.rs::backfill -- a projection rebuilt from the log",
    );
    assert!(j.unexcused.is_empty(), "{:?}", j.unexcused);
    assert_eq!(j.used.len(), 1);

    // A helper nobody calls is covered by nobody.
    let j = judge_source(
        r#"async fn orphan(conn: &mut AnyConnection) { sqlx::query("DELETE FROM widgets").execute(conn).await; }"#,
        "",
    );
    assert_eq!(
        tables(&j),
        vec![("orphan".to_string(), "widgets".to_string())]
    );
}

#[test]
fn staging_in_a_transaction_of_its_own_covers_no_caller() {
    // `save` opens and commits its own transaction and stages there; a
    // helper handed a DIFFERENT transaction by `other` is not covered by
    // `save` staging, and neither is `save`'s name made a stager: before
    // this rule, `new`, `get` and `router` all resolved as stagers.
    let j = judge_source(
        r#"
        impl SqlWidgets {
            async fn save(&self, w: &W) -> Result<(), E> {
                let mut tx = self.pool.begin().await?;
                sqlx::query("UPDATE widgets SET a = 1").execute(&mut *tx).await?;
                boss_events::outbox::record_event_in_tx(&mut tx, &ev).await?;
                tx.commit().await
            }
            async fn other(&self, w: &W) -> Result<(), E> {
                let mut tx = self.pool.begin().await?;
                write_gadget(&mut tx).await?;
                self.save(w).await?;
                tx.commit().await
            }
        }
        async fn write_gadget(tx: &mut Transaction<'_, Postgres>) -> Result<(), E> {
            sqlx::query("INSERT INTO gadgets (a) VALUES (1)").execute(&mut **tx).await?;
            Ok(())
        }
        "#,
        "",
    );
    assert_eq!(
        tables(&j),
        vec![("write_gadget".to_string(), "gadgets".to_string())]
    );
}

#[test]
fn a_helper_in_another_crate_is_resolved_by_its_crate_path() {
    // `record_subject_in_tx` lives in boss-subject-kinds and is called,
    // crate-qualified, from the domain crates' staging writes.
    let helper = scan_source(
        "crates/core/boss-subject-kinds/src/subjects.rs",
        "crates/core/boss-subject-kinds",
        r#"pub async fn record_subject_in_tx(tx: &mut Transaction<'_, Postgres>, id: &str) {
               sqlx::query("INSERT INTO subjects (id) VALUES ($1)").execute(&mut **tx).await;
           }"#,
    )
    .expect("parses");
    let staging_caller = scan_source(
        "crates/modules/boss-a/src/postgres.rs",
        "crates/modules/boss-a",
        r#"async fn create(pool: &Pool) {
               let mut tx = pool.begin().await?;
               boss_subject_kinds::subjects::record_subject_in_tx(&mut tx, "x").await?;
               boss_events::outbox::record_event_in_tx(&mut tx, &ev).await?;
           }"#,
    )
    .expect("parses");
    let mut funcs = [helper.clone(), staging_caller.clone()].concat();
    assert!(judge(&funcs, &[]).unexcused.is_empty());
    // A same-named function in another crate is not a call to it.
    let bare_elsewhere = scan_source(
        "crates/modules/boss-b/src/postgres.rs",
        "crates/modules/boss-b",
        r#"async fn mint(pool: &Pool) {
               let mut tx = pool.begin().await?;
               record_subject_in_tx(&mut tx, "x").await?;
           }"#,
    )
    .expect("parses");
    funcs.extend(bare_elsewhere);
    assert!(judge(&funcs, &[]).unexcused.is_empty());
    // A crate-qualified caller that stages nothing uncovers it.
    let fact_less = scan_source(
        "crates/modules/boss-c/src/postgres.rs",
        "crates/modules/boss-c",
        r#"async fn mint(pool: &Pool) {
               let mut tx = pool.begin().await?;
               boss_subject_kinds::subjects::record_subject_in_tx(&mut tx, "x").await?;
           }"#,
    )
    .expect("parses");
    funcs.extend(fact_less);
    assert_eq!(judge(&funcs, &[]).unexcused.len(), 1);
}

#[test]
fn the_write_shapes_are_found_in_every_literal_form() {
    let j = judge_source(
        r##"
        const INSERT_SQL: &str = "INSERT INTO public.\"gadgets\" (id) VALUES ($1)";
        const UNUSED: &str = "DELETE FROM sprockets WHERE id = $1";
        /// A doc comment saying `UPDATE prose SET x` is not a write.
        fn a(pool: &Pool) {
            sqlx::query!("UPDATE jobs j SET status = 'x' WHERE j.id = $1", id);
            sqlx::query(&format!("DELETE FROM {table} WHERE id = $1"));
            sqlx::query(r#"TRUNCATE TABLE caches"#);
            sqlx::query(INSERT_SQL);
            // Not writes: a row lock, and an upsert's conflict arm (its
            // INSERT is the write).
            sqlx::query("SELECT 1 FROM jobs FOR UPDATE SKIP LOCKED");
            sqlx::query("INSERT INTO t (a) VALUES (1) ON CONFLICT (a) DO UPDATE SET a = 2");
        }
        fn insert_sql() -> String {
            "INSERT INTO built (a) VALUES ($1)".to_string()
        }
        fn b(pool: &Pool) {
            sqlx::query(&insert_sql());
        }
        #[cfg(test)]
        mod tests {
            fn t() { sqlx::query("DELETE FROM hidden"); }
        }
        #[test]
        fn t2() { sqlx::query("DELETE FROM hidden2"); }
        "##,
        "",
    );
    let mut found: Vec<(String, &str, &str)> = j
        .unexcused
        .iter()
        .map(|s| (s.table.clone(), s.verb, s.qual.as_str()))
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            // A SQL builder's write is reported where it is written,
            // judged by its callers.
            ("built".to_string(), "INSERT", "insert_sql"),
            ("caches".to_string(), "TRUNCATE", "a"),
            ("gadgets".to_string(), "INSERT", "a"),
            ("jobs".to_string(), "UPDATE", "a"),
            ("sprockets".to_string(), "DELETE", "const UNUSED"),
            ("t".to_string(), "INSERT", "a"),
            ("{table}".to_string(), "DELETE", "a"),
        ]
    );
}

#[test]
fn the_allowlist_refuses_a_reasonless_or_malformed_entry_and_names_a_stale_one() {
    for bad in [
        "not-a-fact table sessions\n",
        "not-a-fact table sessions -- cache\n",
        "table sessions -- login sessions are not company state\n",
        "maybe table sessions -- login sessions are not company state\n",
        "gap fn crates/a.rs -- a function without its name is no key\n",
        "gap file crates/a -- a file entry names one .rs file\n",
    ] {
        assert!(parse_allowlist(bad).is_err(), "accepted: {bad}");
    }
    let j = judge_source(
        r#"
        fn f(pool: &Pool) { sqlx::query("INSERT INTO widgets (a) VALUES (1)"); }
        fn g(pool: &Pool) { sqlx::query("DELETE FROM sessions"); }
        fn h(pool: &Pool) { sqlx::query("INSERT INTO gizmos (a) VALUES (1)"); }
        fn k(pool: &Pool) {
            sqlx::query("INSERT INTO kept (a) VALUES (1)");
            boss_events::outbox::record_event_in_tx(&mut tx, &ev);
        }
        "#,
        "# comment\n\
         not-a-fact table sessions -- login sessions are not company state\n\
         gap fn crates/x/boss-y/src/lib.rs::f -- widget edits stage no widget event yet\n\
         not-a-fact fn crates/x/boss-y/src/lib.rs::gone -- a write that no longer exists\n\
         gap fn crates/x/boss-y/src/lib.rs::k -- a write that has since learned to stage\n",
    );
    assert_eq!(tables(&j), vec![("h".to_string(), "gizmos".to_string())]);
    assert_eq!(
        j.used,
        BTreeSet::from([2, 3]),
        "the entry naming `gone` excused nothing, and neither did the one for `k`, which \
         stages now"
    );
}

#[test]
fn every_database_write_stages_a_fact_or_is_named_on_the_allowlist() {
    let root = repo_root();
    let (funcs, files) = scan_tree(&root);
    let text = std::fs::read_to_string(root.join(ALLOWLIST))
        .unwrap_or_else(|e| panic!("{ALLOWLIST} is readable: {e}"));
    let allow = parse_allowlist(&text)
        .unwrap_or_else(|errs| panic!("{ALLOWLIST} is malformed:\n  {}", errs.join("\n  ")));
    let j = judge(&funcs, &allow);

    // Controls: a walk that reads nothing, or a pattern that matches
    // nothing, would pass every write it never saw.
    assert!(
        files > 500,
        "read only {files} .rs files under crates/*/*/src — the walk broke"
    );
    assert!(
        j.writes > 300,
        "found only {} writes — the write pattern has gone blind",
        j.writes
    );
    // And one that stopped seeing staging would report every write in
    // the tree: about a hundred functions stage when this landed. (A
    // function that stops staging is not this control's business — it
    // fails below, by name, as the write it now is.)
    assert!(
        j.staging.len() > 60,
        "only {} function(s) read as staging a fact — the staging reader has gone blind",
        j.staging.len()
    );

    let mut failures = Vec::new();
    if !j.unexcused.is_empty() {
        let lines: Vec<String> = j
            .unexcused
            .iter()
            .map(|s| {
                format!(
                    "{}:{} {} {} {} — {}",
                    s.file, s.line, s.qual, s.verb, s.table, s.why
                )
            })
            .collect();
        failures.push(format!(
            "{} database write(s) stage no fact on the outbox and are not on {ALLOWLIST} \
             (backlog 8c271e8f). Stage the event in the same transaction as the write \
             (boss_events::outbox::record_event_in_tx) — the log is the system of record. \
             Only a write that is NOT a fact (a projection rebuilt from the log, a cache, a \
             session, telemetry, outbox bookkeeping) goes on the allowlist, as \
             `not-a-fact fn <file>::<fn> -- <reason>`:\n  {}",
            j.unexcused.len(),
            lines.join("\n  ")
        ));
    }
    let stale: Vec<String> = allow
        .iter()
        .filter(|a| !j.used.contains(&a.line))
        .filter_map(|a| {
            text.lines()
                .nth(a.line - 1)
                .map(|l| format!("line {}: {l}", a.line))
        })
        .collect();
    if !stale.is_empty() {
        failures.push(format!(
            "{} entr(y/ies) in {ALLOWLIST} excuse no write — the write is gone or now \
             stages its fact, so delete the entry in the same change (a stale allowance is \
             a hole a future write walks through; a retired `gap` is a debt paid, so close \
             its item too):\n  {}",
            stale.len(),
            stale.join("\n  ")
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

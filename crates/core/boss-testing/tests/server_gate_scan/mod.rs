//! Conservative source inventory, not runtime listener ownership evidence.
use std::collections::{BTreeMap, BTreeSet};
use syn::visit::Visit;

#[derive(Debug)]
pub struct Server {
    pub function: String,
    pub kind: &'static str,
    pub gated: bool,
}

#[derive(Clone)]
struct Alias {
    path: String,
    unknown: bool,
}

type Aliases = BTreeMap<String, Alias>;

fn path(path: &syn::Path, aliases: &Aliases) -> String {
    let mut names = path.segments.iter().map(|s| s.ident.to_string());
    let first = names.next().unwrap_or_default();
    std::iter::once(aliases.get(&first).map(|a| a.path.clone()).unwrap_or(first))
        .chain(names)
        .collect::<Vec<_>>()
        .join("::")
}

fn imports(tree: &syn::UseTree, prefix: &str, aliases: &mut BTreeMap<String, String>) {
    match tree {
        syn::UseTree::Path(p) => imports(&p.tree, &format!("{prefix}{}::", p.ident), aliases),
        syn::UseTree::Name(n) => {
            if n.ident == "self" {
                let target = prefix.trim_end_matches("::");
                if let Some(name) = target.rsplit("::").next() {
                    aliases.insert(name.to_string(), target.to_string());
                }
            } else {
                aliases.insert(n.ident.to_string(), format!("{prefix}{}", n.ident));
            }
        }
        syn::UseTree::Rename(n) => {
            aliases.insert(
                n.rename.to_string(),
                if n.ident == "self" {
                    prefix.trim_end_matches("::").to_string()
                } else {
                    format!("{prefix}{}", n.ident)
                },
            );
        }
        syn::UseTree::Group(g) => {
            for item in &g.items {
                imports(item, prefix, aliases);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

#[derive(Clone, Default)]
struct Scope {
    aliases: Aliases,
    types: BTreeSet<String>,
    values: BTreeSet<String>,
    imported: BTreeSet<String>,
    declared_values: BTreeSet<String>,
    opaque_import: bool,
}

fn has_glob(tree: &syn::UseTree) -> bool {
    match tree {
        syn::UseTree::Glob(_) => true,
        syn::UseTree::Path(path) => has_glob(&path.tree),
        syn::UseTree::Group(group) => group.items.iter().any(has_glob),
        _ => false,
    }
}

fn declarations(item: &syn::Item, types: &mut BTreeSet<String>, values: &mut BTreeSet<String>) {
    let type_name = match item {
        syn::Item::Mod(i) => Some(&i.ident),
        syn::Item::Struct(i) => Some(&i.ident),
        syn::Item::Enum(i) => Some(&i.ident),
        syn::Item::Union(i) => Some(&i.ident),
        syn::Item::Type(i) => Some(&i.ident),
        syn::Item::Trait(i) => Some(&i.ident),
        syn::Item::TraitAlias(i) => Some(&i.ident),
        syn::Item::ExternCrate(i) => Some(i.rename.as_ref().map_or(&i.ident, |(_, name)| name)),
        _ => None,
    };
    if let Some(name) = type_name {
        types.insert(name.to_string());
    }
    let value_name = match item {
        syn::Item::Fn(i) => Some(&i.sig.ident),
        syn::Item::Const(i) => Some(&i.ident),
        syn::Item::Static(i) => Some(&i.ident),
        syn::Item::Struct(i) if !matches!(i.fields, syn::Fields::Named(_)) => Some(&i.ident),
        _ => None,
    };
    if let Some(name) = value_name {
        values.insert(name.to_string());
    }
}

fn resolve_import(
    raw: &str,
    local: &BTreeMap<String, String>,
    parent: &Aliases,
    types: &BTreeSet<String>,
    seen: &mut BTreeSet<String>,
) -> Alias {
    let mut parts = raw.split("::");
    let first = parts.next().unwrap_or_default();
    let rest: Vec<_> = parts.collect();
    if types.contains(first) {
        return Alias {
            path: raw.into(),
            unknown: true,
        };
    }
    let base = if let Some(target) = local.get(first) {
        if !seen.insert(first.into()) {
            return Alias {
                path: raw.into(),
                unknown: true,
            };
        }
        resolve_import(target, local, parent, types, seen)
    } else {
        parent.get(first).cloned().unwrap_or(Alias {
            path: first.into(),
            unknown: false,
        })
    };
    Alias {
        path: std::iter::once(base.path)
            .chain(rest.into_iter().map(str::to_string))
            .collect::<Vec<_>>()
            .join("::"),
        unknown: base.unknown,
    }
}

fn scoped(items: &[&syn::Item], parent: &Scope) -> Scope {
    let mut types = BTreeSet::new();
    let mut values = BTreeSet::new();
    let mut raw = BTreeMap::new();
    let mut opaque_import = false;
    for item in items {
        declarations(item, &mut types, &mut values);
        if let syn::Item::Use(item) = item {
            imports(&item.tree, "", &mut raw);
            opaque_import |= has_glob(&item.tree);
        }
    }
    let mut scope = parent.clone();
    scope.opaque_import |= opaque_import;
    scope.imported = raw.keys().cloned().collect();
    scope.declared_values = values.clone();
    // Imports in a child scope replace inherited declarations. Same-scope
    // declarations remain unknown; compilation judges conflicting imports.
    for name in raw.keys() {
        if !types.contains(name) {
            scope.types.remove(name);
        }
        if !values.contains(name) {
            scope.values.remove(name);
        }
    }
    scope.types.extend(types);
    scope.values.extend(values);
    for (name, target) in &raw {
        let alias = resolve_import(
            target,
            &raw,
            &parent.aliases,
            &scope.types,
            &mut BTreeSet::new(),
        );
        scope.aliases.insert(name.clone(), alias);
    }
    scope
}

fn generics(scope: &mut Scope, generics: &syn::Generics) {
    for parameter in &generics.params {
        match parameter {
            syn::GenericParam::Type(i) => {
                scope.types.insert(i.ident.to_string());
            }
            syn::GenericParam::Const(i) => {
                scope.values.insert(i.ident.to_string());
            }
            syn::GenericParam::Lifetime(_) => {}
        }
    }
}

fn inner(expr: &syn::Expr) -> &syn::Expr {
    match expr {
        syn::Expr::Await(e) => inner(&e.base),
        syn::Expr::Try(e) => inner(&e.expr),
        syn::Expr::Paren(e) => inner(&e.expr),
        syn::Expr::Reference(e) => inner(&e.expr),
        _ => expr,
    }
}

fn variable(expr: &syn::Expr) -> Option<String> {
    match inner(expr) {
        syn::Expr::Path(p)
            if p.qself.is_none()
                && p.path.leading_colon.is_none()
                && p.path.segments.len() == 1
                && matches!(p.path.segments[0].arguments, syn::PathArguments::None) =>
        {
            Some(p.path.segments[0].ident.to_string())
        }
        _ => None,
    }
}

fn value_preserving_method(method: &syn::Ident) -> bool {
    [
        "clone",
        "into_make_service",
        "into_make_service_with_connect_info",
    ]
    .iter()
    .any(|name| method == *name)
}

fn listener(expr: &syn::Expr, aliases: &Aliases) -> bool {
    match inner(expr) {
        syn::Expr::Call(c) => {
            matches!(&*c.func, syn::Expr::Path(p) if path(&p.path, aliases).ends_with("TcpListener::bind"))
        }
        syn::Expr::MethodCall(c)
            if ["unwrap", "expect", "context", "with_context"]
                .iter()
                .any(|n| c.method == *n) =>
        {
            listener(&c.receiver, aliases)
        }
        syn::Expr::Match(m) => listener(&m.expr, aliases),
        _ => false,
    }
}

fn macro_calls(tokens: proc_macro2::TokenStream, aliases: &Aliases) -> usize {
    use proc_macro2::{Delimiter, TokenTree};
    let tokens: Vec<_> = tokens.into_iter().collect();
    let mut calls = 0;
    for (i, token) in tokens.iter().enumerate() {
        if let TokenTree::Group(group) = token {
            calls += macro_calls(group.stream(), aliases);
        }
        let TokenTree::Ident(first) = token else {
            continue;
        };
        if matches!(i.checked_sub(1).and_then(|n| tokens.get(n)), Some(TokenTree::Punct(p)) if p.as_char() == ':')
        {
            continue;
        }
        let first = first.to_string();
        let mut name = aliases.get(&first).map(|a| a.path.clone()).unwrap_or(first);
        let mut j = i + 1;
        while let Some(
            [
                TokenTree::Punct(a),
                TokenTree::Punct(b),
                TokenTree::Ident(next),
            ],
        ) = tokens.get(j..j + 3)
        {
            if a.as_char() != ':' || b.as_char() != ':' {
                break;
            }
            name.push_str("::");
            name.push_str(&next.to_string());
            j += 3;
        }
        if matches!(tokens.get(j), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis)
            && (name == "axum::serve"
                || name.ends_with("TcpListener::bind")
                || name.ends_with("role_service::serve_with_refresh")
                || name == "axum_server::bind"
                || name == "hyper::Server::bind")
        {
            calls += 1;
        }
    }
    calls
}

fn macro_references(tokens: proc_macro2::TokenStream) -> BTreeSet<String> {
    tokens
        .into_iter()
        .flat_map(|token| match token {
            proc_macro2::TokenTree::Group(group) => macro_references(group.stream()),
            proc_macro2::TokenTree::Ident(name) => BTreeSet::from([name.to_string()]),
            _ => BTreeSet::new(),
        })
        .collect()
}

#[derive(Clone)]
struct Binding {
    id: usize,
    gated: bool,
    listener_origin: Option<usize>,
}

struct Function<'a> {
    name: String,
    scope: Scope,
    bindings: BTreeMap<String, Binding>,
    next_id: usize,
    tainted: BTreeSet<usize>,
    listeners: BTreeSet<usize>,
    consumed: BTreeSet<usize>,
    binds: usize,
    servers: &'a mut Vec<Server>,
}

impl Function<'_> {
    fn finish(&mut self) {
        let raw = self
            .binds
            .saturating_sub(self.listeners.intersection(&self.consumed).count());
        for _ in 0..raw {
            self.serve("unaccounted TCP listener", None, None);
        }
    }
    fn bind(&mut self, name: String, gated: bool) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.bindings.insert(
            name,
            Binding {
                id,
                gated,
                listener_origin: None,
            },
        );
        id
    }
    fn invalidate(&mut self, name: &str) {
        if let Some(binding) = self.bindings.get(name) {
            self.tainted.insert(binding.id);
        }
    }
    fn unknown_pattern(&mut self, pattern: &syn::Pat) {
        struct Names(Vec<String>);
        impl<'ast> Visit<'ast> for Names {
            fn visit_pat_ident(&mut self, id: &'ast syn::PatIdent) {
                self.0.push(id.ident.to_string());
                syn::visit::visit_pat_ident(self, id);
            }
        }
        let mut names = Names(Vec::new());
        names.visit_pat(pattern);
        for name in names.0 {
            self.bind(name, false);
        }
    }
    fn gated(&self, expr: &syn::Expr) -> bool {
        match inner(expr) {
            syn::Expr::Call(c) => {
                let syn::Expr::Path(p) = &*c.func else {
                    return false;
                };
                let Some(resolved) = self.authentic_path(p) else {
                    return false;
                };
                matches!(
                    resolved.as_str(),
                    "machine_gate::mount" | "boss_core::machine_gate::mount"
                )
            }
            syn::Expr::Path(_) => variable(expr).is_some_and(|v| {
                self.bindings
                    .get(&v)
                    .is_some_and(|b| b.gated && !self.tainted.contains(&b.id))
            }),
            syn::Expr::MethodCall(c) if value_preserving_method(&c.method) => {
                self.gated(&c.receiver)
            }
            _ => false,
        }
    }

    // A mounted value proves nothing about a transport which resolves to a
    // different local item. Mounts and transports share this lexical judgment.
    fn authentic_path(&self, path_expr: &syn::ExprPath) -> Option<String> {
        let first = path_expr.path.segments.first()?.ident.to_string();
        if path_expr.qself.is_some()
            || (path_expr.path.segments.len() == 1 && self.bindings.contains_key(&first))
            || (path_expr.path.segments.len() > 1 && self.scope.types.contains(&first))
            || (self.scope.opaque_import && !self.scope.imported.contains(&first))
            || self
                .scope
                .aliases
                .get(&first)
                .is_some_and(|alias| alias.unknown)
        {
            return None;
        }
        Some(path(&path_expr.path, &self.scope.aliases))
    }

    fn known_builder(&self, expr: &syn::Expr) -> bool {
        let syn::Expr::Call(call) = inner(expr) else {
            return false;
        };
        let syn::Expr::Path(path) = &*call.func else {
            return false;
        };
        self.authentic_path(path).is_some_and(|name| {
            matches!(name.as_str(), "axum_server::bind" | "hyper::Server::bind")
        })
    }

    fn serve(&mut self, kind: &'static str, app: Option<&syn::Expr>, input: Option<&syn::Expr>) {
        if let Some(origin) = input.and_then(|input| self.listener_origin(input)) {
            self.consumed.insert(origin);
        }
        self.servers.push(Server {
            function: self.name.clone(),
            kind,
            gated: app.is_some_and(|app| self.gated(app)),
        });
    }

    fn listener_origin(&self, expr: &syn::Expr) -> Option<usize> {
        let name = variable(expr)?;
        let binding = self.bindings.get(&name)?;
        (!self.tainted.contains(&binding.id))
            .then_some(binding.listener_origin)
            .flatten()
    }
}

impl<'ast> Visit<'ast> for Function<'_> {
    fn visit_expr_reference(&mut self, expr: &'ast syn::ExprReference) {
        if expr.mutability.is_some()
            && let Some(name) = variable(&expr.expr)
        {
            self.invalidate(&name);
        }
        syn::visit::visit_expr_reference(self, expr);
    }
    fn visit_expr_closure(&mut self, expr: &'ast syn::ExprClosure) {
        let bindings = self.bindings.clone();
        for pattern in &expr.inputs {
            self.unknown_pattern(pattern);
        }
        self.visit_expr(&expr.body);
        self.bindings = bindings;
    }

    fn visit_expr_for_loop(&mut self, expr: &'ast syn::ExprForLoop) {
        let bindings = self.bindings.clone();
        self.visit_expr(&expr.expr);
        self.unknown_pattern(&expr.pat);
        self.visit_block(&expr.body);
        self.bindings = bindings;
    }

    fn visit_expr_while(&mut self, expr: &'ast syn::ExprWhile) {
        let bindings = self.bindings.clone();
        syn::visit::visit_expr_while(self, expr);
        self.bindings = bindings;
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        let bindings = self.bindings.clone();
        self.unknown_pattern(&arm.pat);
        syn::visit::visit_arm(self, arm);
        self.bindings = bindings;
    }

    fn visit_expr_if(&mut self, expr: &'ast syn::ExprIf) {
        let bindings = self.bindings.clone();
        syn::visit::visit_expr_if(self, expr);
        self.bindings = bindings;
    }

    fn visit_expr_let(&mut self, expr: &'ast syn::ExprLet) {
        self.visit_expr(&expr.expr);
        self.unknown_pattern(&expr.pat);
    }
    fn visit_block(&mut self, block: &'ast syn::Block) {
        let bindings = self.bindings.clone();
        let scope = self.scope.clone();
        // Rust use-items apply to their whole lexical scope, even when
        // written after the call. Do not collect a nested scope's imports.
        let items: Vec<_> = block
            .stmts
            .iter()
            .filter_map(|statement| match statement {
                syn::Stmt::Item(item) => Some(item),
                _ => None,
            })
            .collect();
        self.scope = scoped(&items, &self.scope);
        for name in &self.scope.imported {
            if !self.scope.declared_values.contains(name) {
                self.bindings.remove(name);
            }
        }
        for name in self.scope.declared_values.clone() {
            self.bind(name, false);
        }
        syn::visit::visit_block(self, block);
        self.bindings = bindings;
        self.scope = scope;
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        let scope = self.scope.clone();
        if let Some((_, content)) = &module.content {
            self.scope = scoped(&content.iter().collect::<Vec<_>>(), &self.scope);
        }
        syn::visit::visit_item_mod(self, module);
        self.scope = scope;
    }
    fn visit_item_use(&mut self, _item: &'ast syn::ItemUse) {}

    fn visit_local(&mut self, local: &'ast syn::Local) {
        // Initializer borrows can invalidate the old binding. Judge its
        // value afterwards so a new alias cannot copy pre-borrow proof.
        // The new pattern is not in scope in its initializer or let-else.
        syn::visit::visit_local(self, local);
        let proof = local.init.as_ref().map(|init| {
            (
                self.gated(&init.expr),
                listener(&init.expr, &self.scope.aliases),
                self.listener_origin(&init.expr),
            )
        });
        // Destructuring never inherits an outer mount.
        let pat = match &local.pat {
            syn::Pat::Type(t) => &*t.pat,
            p => p,
        };
        if let (Some((gated, listener, prior_origin)), syn::Pat::Ident(id)) = (proof, pat)
            && id.subpat.is_none()
            && !(id.by_ref.is_some() && id.mutability.is_some())
        {
            let name = id.ident.to_string();
            let binding_id = self.bind(name.clone(), gated);
            if listener {
                self.listeners.insert(binding_id);
            }
            if let Some(binding) = self.bindings.get_mut(&name) {
                binding.listener_origin = if listener {
                    Some(binding_id)
                } else {
                    prior_origin
                };
            }
        } else {
            self.unknown_pattern(&local.pat);
        }
    }

    fn visit_expr_assign(&mut self, expr: &'ast syn::ExprAssign) {
        if let Some(name) = variable(&expr.left) {
            self.invalidate(&name);
        }
        syn::visit::visit_expr_assign(self, expr);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        // Arguments can mutate a mounted binding before the transport
        // receives it; an outer verdict cannot precede those visits.
        syn::visit::visit_expr_call(self, call);
        if let syn::Expr::Path(p) = &*call.func {
            let name = path(&p.path, &self.scope.aliases);
            if name.ends_with("TcpListener::bind") {
                self.binds += 1;
            }
            if name == "axum::serve" || name.ends_with("role_service::serve_with_refresh") {
                if !self.authentic_path(p).is_some_and(|name| {
                    matches!(
                        name.as_str(),
                        "axum::serve" | "boss_policy_client::role_service::serve_with_refresh"
                    )
                }) {
                    self.serve("shadowed serve adapter", None, None);
                } else {
                    self.serve("HTTP serve", call.args.iter().nth(1), call.args.first());
                }
            } else if name == "serve" && call.args.len() == 2 {
                self.serve("unresolved serve", None, None);
            }
        }
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if !value_preserving_method(&call.method)
            && let Some(name) = variable(&call.receiver)
        {
            self.invalidate(&name);
        }
        syn::visit::visit_expr_method_call(self, call);
        if call.method == "serve" {
            // Unknown .serve receivers stay visible and cannot inherit a
            // neighbouring mount. This is intentionally conservative.
            let app = self
                .known_builder(&call.receiver)
                .then(|| call.args.first())
                .flatten();
            self.serve("HTTP builder serve", app, None);
        } else if call.method == "serve_connection" {
            self.serve("unproved HTTP connection adapter", None, call.args.first());
        }
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // Opaque tokens can mutate values they name, even without a
        // serving call. Literal text and unrelated names borrow no proof.
        for name in macro_references(mac.tokens.clone()) {
            self.invalidate(&name);
        }
        // Macro expansion cannot be proven by syn. Name recognized opaque
        // serving syntax as unknown instead of silently counting it gated.
        let calls = macro_calls(mac.tokens.clone(), &self.scope.aliases);
        for _ in 0..calls {
            self.serve("opaque macro serve", None, None);
        }
    }

    fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
        self.nested_function(&function.sig, &function.block);
    }
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let scope = self.scope.clone();
        generics(&mut self.scope, &item.generics);
        syn::visit::visit_item_impl(self, item);
        self.scope = scope;
    }
    fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
        self.nested_function(&function.sig, &function.block);
    }
    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        let scope = self.scope.clone();
        generics(&mut self.scope, &item.generics);
        self.scope.types.insert("Self".into());
        syn::visit::visit_item_trait(self, item);
        self.scope = scope;
    }
    fn visit_trait_item_fn(&mut self, function: &'ast syn::TraitItemFn) {
        if let Some(block) = &function.default {
            self.nested_function(&function.sig, block);
        }
    }
}

impl Function<'_> {
    fn nested_function(&mut self, signature: &syn::Signature, block: &syn::Block) {
        let mut nested = Function {
            name: format!("{}::{}", self.name, signature.ident),
            scope: self.scope.clone(),
            bindings: BTreeMap::new(),
            next_id: 0,
            tainted: BTreeSet::new(),
            listeners: BTreeSet::new(),
            consumed: BTreeSet::new(),
            binds: 0,
            servers: self.servers,
        };
        // Nested functions can resolve enclosing items, but cannot capture
        // an outer app's mounted value or other local bindings.
        for name in &self.scope.values {
            nested.bind(name.clone(), false);
        }
        generics(&mut nested.scope, &signature.generics);
        nested.bind(signature.ident.to_string(), false);
        for argument in &signature.inputs {
            if let syn::FnArg::Typed(argument) = argument {
                nested.unknown_pattern(&argument.pat);
            }
        }
        nested.visit_block(block);
        nested.finish();
    }
}

pub fn scan(source: &str) -> Result<Vec<Server>, syn::Error> {
    struct Functions {
        scope: Scope,
        servers: Vec<Server>,
        prefix: String,
    }
    impl Functions {
        fn function(&mut self, name: String, signature: &syn::Signature, block: &syn::Block) {
            let mut checker = Function {
                name: format!("{}{name}", self.prefix),
                scope: self.scope.clone(),
                bindings: BTreeMap::new(),
                next_id: 0,
                tainted: BTreeSet::new(),
                listeners: BTreeSet::new(),
                consumed: BTreeSet::new(),
                binds: 0,
                servers: &mut self.servers,
            };
            for name in &self.scope.values {
                checker.bind(name.clone(), false);
            }
            generics(&mut checker.scope, &signature.generics);
            for argument in &signature.inputs {
                if let syn::FnArg::Typed(argument) = argument {
                    checker.unknown_pattern(&argument.pat);
                }
            }
            checker.visit_block(block);
            checker.finish();
        }
    }
    impl<'ast> Visit<'ast> for Functions {
        fn visit_file(&mut self, file: &'ast syn::File) {
            self.scope = scoped(&file.items.iter().collect::<Vec<_>>(), &self.scope);
            syn::visit::visit_file(self, file);
        }
        fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
            let prefix = self.prefix.clone();
            let scope = self.scope.clone();
            self.prefix = format!("{}{}::", self.prefix, module.ident);
            if let Some((_, items)) = &module.content {
                self.scope = scoped(&items.iter().collect::<Vec<_>>(), &self.scope);
            }
            syn::visit::visit_item_mod(self, module);
            self.prefix = prefix;
            self.scope = scope;
        }
        fn visit_item_use(&mut self, _item: &'ast syn::ItemUse) {}
        fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
            let scope = self.scope.clone();
            generics(&mut self.scope, &item.generics);
            syn::visit::visit_item_impl(self, item);
            self.scope = scope;
        }
        fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
            let scope = self.scope.clone();
            generics(&mut self.scope, &item.generics);
            self.scope.types.insert("Self".into());
            syn::visit::visit_item_trait(self, item);
            self.scope = scope;
        }
        fn visit_trait_item_fn(&mut self, function: &'ast syn::TraitItemFn) {
            if let Some(block) = &function.default {
                self.function(
                    format!("<trait>::{}", function.sig.ident),
                    &function.sig,
                    block,
                );
            }
        }
        fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
            self.function(
                function.sig.ident.to_string(),
                &function.sig,
                &function.block,
            );
        }
        fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
            self.function(
                format!("<impl>::{}", function.sig.ident),
                &function.sig,
                &function.block,
            );
        }
    }
    let parsed = syn::parse_file(source)?;
    let mut checker = Functions {
        scope: Scope::default(),
        servers: Vec::new(),
        prefix: String::new(),
    };
    checker.visit_file(&parsed);
    Ok(checker.servers)
}

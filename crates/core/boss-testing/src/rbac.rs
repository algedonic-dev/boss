//! ONE reader for the Kubernetes manifests the credential pins judge
//! (backlog cbb56130, 2026-09-29).
//!
//! WHY ONE. Two pins guard the credential boundary from two sides —
//! `no_default_service_account_reads_a_secret.rs` (no pod that declares no
//! identity holds a grant; only the boss pod runs as the broker's account)
//! and `the_dev_session_writes_no_workload_in_boss.rs` (the dev session
//! reaches no credential in an instance namespace). Each shipped its own
//! line-matching YAML reader, and each passed its builder and failed its
//! adversarial review the same way: shapes `kubectl apply` accepts were read
//! as "nothing here". Review 28367b57: an inline `subjects: [{...}]`, an
//! inline `rules: [{...}]` and a `kind: List` were each green (M2b, M6, M9).
//! Review 978b: 11 of 15 probes bypassed, among them an inline `metadata:`,
//! a single-quoted namespace, an aggregated ClusterRole and a `User` subject
//! spelling the ServiceAccount's user name. Two readers with one weakness
//! are one fact held twice (CLAUDE.md §9a), so both pins now read through
//! this module and cannot drift apart again.
//!
//! WHAT IT READS. A strict subset of YAML: block mappings and sequences,
//! one-line flow collections (`{a: b}`, `[a, "b"]`, nested), plain,
//! single-quoted and double-quoted scalars (unquoted), and block scalars
//! (`|`, `>`), whose text it skips. EVERYTHING ELSE IS REFUSED, naming the
//! file and line, and the refusal ends in "refused, not passed": an anchor,
//! an alias, a tag, a merge key, a complex key, a double-quoted escape, a
//! multi-line plain or quoted scalar, a flow collection that spans lines, a
//! duplicate key, tab indentation, a directive, a document that is not a
//! mapping. And these, which it COULD parse but which would move a grant
//! out of the place the pins look for it:
//!
//!   * `kind: List` (any `*List`) and a top-level `items:` — `kubectl
//!     apply` unwraps a List, so a binding inside one is live;
//!   * `aggregationRule` — the ClusterRole's rules are whatever other
//!     roles' labels say at run time, not what the file says;
//!   * any value written INLINE on a top-level `metadata:`, `roleRef:`,
//!     `subjects:` or `rules:` line. The flow form parses, but no manifest
//!     in the tree writes it (measured 2026-09-29), and the two readers
//!     this replaces both read it as empty; refusing it keeps the one shape
//!     a reviewer reads in the diff the only shape there is.
//!
//! An unreadable grant is not a grant of nothing (CLAUDE.md: no evidence is
//! not a pass).
//!
//! WHAT IS READ AS THE ESTATE. `render-instance.sh --all` — what the
//! converge applies, in every instance namespace it applies it (an
//! `instance` manifest once per instance, the `pipeline` set into the
//! source's) — plus every other `*.yaml` / `*.yml` under `infra/` that the
//! render does not already carry, so a grant cannot hide by moving to a
//! file outside `infra/cluster/manifests/`.
//
// not a tree-wide pin: a reader, not a test — the two pins that call it
// (named above) each declare themselves tree-wide.

use crate::{repo_root, scratch_dir};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The renderer the converge applies through.
pub const RENDER: &str = "infra/cluster/render-instance.sh";

/// The directory the render reads; its files are judged as rendered.
pub const MANIFESTS: &str = "infra/cluster/manifests";

// ---- the YAML subset --------------------------------------------------------

/// A node, the line it starts on, and whether it was written on its key's
/// own line (`subjects: [...]`) rather than below it.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub line: usize,
    pub inline: bool,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Map(Vec<(String, Node)>),
    Seq(Vec<Node>),
    Str(String),
    /// A block scalar (`|`, `>`): text this reader skips and never judges.
    Text,
    Null,
}

impl Node {
    fn at(line: usize, value: Value) -> Node {
        Node {
            line,
            inline: false,
            value,
        }
    }

    /// A mapping's value for `key`.
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.value {
            Value::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, n)| n),
            _ => None,
        }
    }

    /// The value at a path of mapping keys.
    pub fn path(&self, keys: &[&str]) -> Option<&Node> {
        keys.iter().try_fold(self, |node, key| node.get(key))
    }

    pub fn str(&self) -> Option<&str> {
        match &self.value {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Every `(key, value)` anywhere under this node, depth first.
    pub fn walk(&self) -> Vec<(&str, &Node)> {
        match &self.value {
            Value::Map(entries) => entries
                .iter()
                .flat_map(|(k, n)| {
                    std::iter::once((k.as_str(), n))
                        .chain(n.walk())
                        .collect::<Vec<_>>()
                })
                .collect(),
            Value::Seq(items) => items.iter().flat_map(Node::walk).collect(),
            _ => Vec::new(),
        }
    }
}

fn refused(source: &str, line: usize, what: &str) -> String {
    format!("{source}:{line}: {what} — refused, not passed")
}

#[derive(Debug, Clone)]
struct Line {
    number: usize,
    indent: usize,
    text: String,
}

/// `- a: b` is a sequence entry holding the mapping `a: b` at the column
/// `a` sits in; split it into a `-` line and that line, recursively, so
/// the parser sees only two line shapes.
fn expand(number: usize, indent: usize, text: &str, out: &mut Vec<Line>) {
    let text = text.trim_end();
    let rest = if text == "-" {
        Some("")
    } else {
        text.strip_prefix("- ")
    };
    let Some(rest) = rest else {
        out.push(Line {
            number,
            indent,
            text: text.to_string(),
        });
        return;
    };
    out.push(Line {
        number,
        indent,
        text: "-".to_string(),
    });
    let body = rest.trim_start_matches(' ');
    if !body.is_empty() && !body.starts_with('#') {
        expand(number, indent + 2 + (rest.len() - body.len()), body, out);
    }
}

/// A quoted scalar at the head of `text`: its value and what follows it.
fn quoted(text: &str) -> Result<(String, &str), String> {
    let mut chars = text.char_indices();
    let quote = chars.next().map(|(_, c)| c).unwrap_or('"');
    let mut out = String::new();
    while let Some((at, c)) = chars.next() {
        match (quote, c) {
            ('"', '\\') => {
                return Err("an escape in a double-quoted scalar".to_string());
            }
            ('\'', '\'') => {
                if text[at + 1..].starts_with('\'') {
                    out.push('\'');
                    chars.next();
                } else {
                    return Ok((out, &text[at + 1..]));
                }
            }
            ('"', '"') => return Ok((out, &text[at + 1..])),
            _ => out.push(c),
        }
    }
    Err("a quoted scalar that does not close on its line".to_string())
}

/// Nothing may follow a value on its line but a comment.
fn only_comment(after: &str) -> Result<(), String> {
    let t = after.trim_start();
    if t.is_empty() || (t.starts_with('#') && after.starts_with([' ', '\t'])) {
        Ok(())
    } else {
        Err(format!("text after a value: {after:?}"))
    }
}

/// The characters that start something other than a plain scalar:
/// anchors, aliases, tags, directives, reserved, block and flow openers,
/// complex keys.
const NOT_PLAIN: &[char] = &[
    '&', '*', '!', '%', '@', '`', '|', '>', '{', '}', '[', ']', '?',
];

/// `text` up to a comment: a `#` at the start or after a space or a tab
/// (`name: default\t# x` is `default`, and `a#b` is one scalar).
fn uncommented(text: &str) -> &str {
    let bytes = text.as_bytes();
    let at = (0..bytes.len())
        .find(|&i| bytes[i] == b'#' && (i == 0 || matches!(bytes[i - 1], b' ' | b'\t')));
    &text[..at.unwrap_or(text.len())]
}

/// A plain scalar's value: the YAML null spellings are null, not the
/// string — a subject's `namespace: ~` is no namespace to the API server.
fn plain(value: &str) -> Value {
    match value {
        "" | "~" | "null" | "Null" | "NULL" => Value::Null,
        _ => Value::Str(value.to_string()),
    }
}

/// One plain or quoted scalar that fills the rest of its line.
fn scalar(text: &str) -> Result<Value, String> {
    if text.starts_with(['"', '\'']) {
        let (value, after) = quoted(text)?;
        only_comment(after)?;
        return Ok(Value::Str(value));
    }
    if text.starts_with(NOT_PLAIN) {
        return Err(format!(
            "a value starting with {:?} (an anchor, alias, tag or other shape this reader does not read)",
            &text[..1]
        ));
    }
    let value = uncommented(text).trim_end();
    if value.contains(": ") || value.contains(":\t") {
        return Err(format!("a plain scalar holding `: `: {value:?}"));
    }
    Ok(plain(value))
}

/// A YAML 1.1 boolean, which is what the API server's YAML decoder reads
/// (`yes`, `on` and `True` are true to it, so a pin comparing to the
/// string "true" would pass a pod that asks for its token with `yes`).
pub fn yaml_bool(node: &Node) -> Option<bool> {
    match node.str()? {
        "true" | "True" | "TRUE" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" | "y" | "Y" => {
            Some(true)
        }
        "false" | "False" | "FALSE" | "no" | "No" | "NO" | "off" | "Off" | "OFF" | "n" | "N" => {
            Some(false)
        }
        _ => None,
    }
}

/// A `key: rest` line: the unquoted key and the rest, or None when the
/// line is not a mapping entry.
fn split_key(text: &str) -> Result<Option<(String, String)>, String> {
    if text.starts_with(['"', '\'']) {
        let (key, after) = quoted(text)?;
        let after = after.trim_start();
        return Ok(after.strip_prefix(':').and_then(|rest| {
            (rest.is_empty() || rest.starts_with(' ')).then(|| (key, rest.trim().to_string()))
        }));
    }
    if text.starts_with(NOT_PLAIN) || text.starts_with('#') {
        return Ok(None);
    }
    let head = uncommented(text);
    let colon = head
        .find(": ")
        .or_else(|| head.ends_with(':').then(|| head.len() - 1));
    Ok(colon.map(|at| {
        let rest = text[at + 1..].trim();
        let rest = if rest.starts_with('#') { "" } else { rest };
        (head[..at].trim_end().to_string(), rest.to_string())
    }))
}

/// A one-line flow collection or scalar, read from a char cursor.
struct Flow {
    chars: Vec<char>,
    at: usize,
    line: usize,
}

impl Flow {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }
    fn skip_ws(&mut self) {
        while self.peek() == Some(' ') {
            self.at += 1;
        }
    }
    fn rest(&self) -> String {
        self.chars[self.at..].iter().collect()
    }

    fn value(&mut self, in_map: bool) -> Result<Node, String> {
        self.skip_ws();
        let line = self.line;
        let node = move |value| Node {
            line,
            inline: true,
            value,
        };
        match self.peek() {
            Some('[') => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.peek() == Some(']') {
                        self.at += 1;
                        break;
                    }
                    items.push(self.value(false)?);
                    self.skip_ws();
                    match self.peek() {
                        Some(',') => self.at += 1,
                        Some(']') => {
                            self.at += 1;
                            break;
                        }
                        _ => {
                            return Err(format!(
                                "a flow sequence that does not close: {}",
                                self.rest()
                            ));
                        }
                    }
                }
                Ok(node(Value::Seq(items)))
            }
            Some('{') => {
                self.at += 1;
                let mut entries: Vec<(String, Node)> = Vec::new();
                loop {
                    self.skip_ws();
                    if self.peek() == Some('}') {
                        self.at += 1;
                        break;
                    }
                    let key = match self.value(true)?.value {
                        Value::Str(k) => k,
                        _ => return Err("a flow mapping key that is not a scalar".to_string()),
                    };
                    self.skip_ws();
                    if self.peek() != Some(':') {
                        return Err(format!("a flow mapping entry with no `:`: {key:?}"));
                    }
                    self.at += 1;
                    self.skip_ws();
                    let value = if matches!(self.peek(), Some(',' | '}')) {
                        node(Value::Null)
                    } else {
                        self.value(false)?
                    };
                    if entries.iter().any(|(k, _)| *k == key) {
                        return Err(format!("a duplicate key {key:?}"));
                    }
                    entries.push((key, value));
                    self.skip_ws();
                    match self.peek() {
                        Some(',') => self.at += 1,
                        Some('}') => {
                            self.at += 1;
                            break;
                        }
                        _ => {
                            return Err(format!(
                                "a flow mapping that does not close: {}",
                                self.rest()
                            ));
                        }
                    }
                }
                Ok(node(Value::Map(entries)))
            }
            Some('"' | '\'') => {
                let text = self.rest();
                let (value, after) = quoted(&text)?;
                self.at = self.chars.len() - after.chars().count();
                Ok(node(Value::Str(value)))
            }
            Some(c) if NOT_PLAIN.contains(&c) || c == ',' => Err(format!(
                "a flow value starting with {c:?} (an anchor, alias, tag or other shape this reader does not read)"
            )),
            None => Err("a flow collection that does not close on its line".to_string()),
            Some(_) => {
                let start = self.at;
                while let Some(c) = self.peek() {
                    let ends_key = in_map
                        && c == ':'
                        && matches!(self.chars.get(self.at + 1), None | Some(' ' | ',' | '}'));
                    if matches!(c, ',' | ']' | '}') || ends_key {
                        break;
                    }
                    if matches!(c, '[' | '{' | '#') {
                        return Err(format!("{c:?} inside a plain flow value"));
                    }
                    self.at += 1;
                }
                let value: String = self.chars[start..self.at].iter().collect();
                let value = value.trim().to_string();
                if value.contains(": ") || value.contains(":\t") {
                    return Err(format!("a plain flow value holding `: `: {value:?}"));
                }
                Ok(node(plain(&value)))
            }
        }
    }
}

struct Parser<'a> {
    source: &'a str,
    lines: Vec<Line>,
    pos: usize,
}

impl Parser<'_> {
    fn err(&self, line: usize, what: &str) -> String {
        refused(self.source, line, what)
    }

    /// The node whose first line is at `pos`, under a parent at `parent`
    /// indent (-1 for the document root).
    fn node(&mut self, parent: isize) -> Result<Node, String> {
        let line = self.lines[self.pos].clone();
        if line.text == "-" {
            return self.seq(line.indent);
        }
        let is_key = split_key(&line.text)
            .map_err(|e| self.err(line.number, &e))?
            .is_some();
        if is_key {
            return self.map(line.indent);
        }
        self.pos += 1;
        self.inline(parent, &line, &line.text)
    }

    fn map(&mut self, indent: usize) -> Result<Node, String> {
        let first = self.lines[self.pos].number;
        let mut entries: Vec<(String, Node)> = Vec::new();
        while let Some(line) = self.lines.get(self.pos).cloned() {
            if line.indent < indent {
                break;
            }
            if line.indent > indent {
                return Err(self.err(line.number, "a line indented under nothing"));
            }
            let Some((key, rest)) = split_key(&line.text).map_err(|e| self.err(line.number, &e))?
            else {
                return Err(self.err(
                    line.number,
                    &format!(
                        "a line that is not `key: value` inside a mapping: {:?}",
                        line.text
                    ),
                ));
            };
            if key == "<<" {
                return Err(self.err(line.number, "a merge key `<<`"));
            }
            if entries.iter().any(|(k, _)| *k == key) {
                return Err(self.err(line.number, &format!("a duplicate key `{key}`")));
            }
            self.pos += 1;
            let value = if rest.is_empty() {
                match self.lines.get(self.pos) {
                    Some(next) if next.indent > indent => self.node(indent as isize)?,
                    Some(next) if next.indent == indent && next.text == "-" => self.seq(indent)?,
                    _ => Node::at(line.number, Value::Null),
                }
            } else {
                let mut node = self.inline(indent as isize, &line, &rest)?;
                node.inline = true;
                node
            };
            entries.push((key, value));
        }
        Ok(Node::at(first, Value::Map(entries)))
    }

    fn seq(&mut self, indent: usize) -> Result<Node, String> {
        let first = self.lines[self.pos].number;
        let mut items = Vec::new();
        while let Some(line) = self.lines.get(self.pos).cloned() {
            if line.indent > indent {
                return Err(self.err(line.number, "a line indented under nothing"));
            }
            if line.indent < indent || line.text != "-" {
                break;
            }
            self.pos += 1;
            let item = match self.lines.get(self.pos) {
                Some(next) if next.indent > indent => self.node(indent as isize)?,
                _ => Node::at(line.number, Value::Null),
            };
            items.push(item);
        }
        Ok(Node::at(first, Value::Seq(items)))
    }

    /// A value written on the line itself: a block scalar header, a flow
    /// collection or a scalar. Nothing more indented than `parent` may
    /// follow it — that would be a multi-line scalar this reader does not
    /// read — except a block scalar's own text, which is skipped.
    fn inline(&mut self, parent: isize, line: &Line, text: &str) -> Result<Node, String> {
        let deeper = |l: &Line| l.indent as isize > parent;
        if text.starts_with(['|', '>']) {
            let header = uncommented(text).trim_end();
            if !header[1..]
                .chars()
                .all(|c| matches!(c, '+' | '-' | '1'..='9'))
            {
                return Err(self.err(line.number, &format!("a block scalar header {header:?}")));
            }
            while self.lines.get(self.pos).is_some_and(deeper) {
                self.pos += 1;
            }
            return Ok(Node::at(line.number, Value::Text));
        }
        let node = if text.starts_with(['[', '{']) {
            let mut flow = Flow {
                chars: text.chars().collect(),
                at: 0,
                line: line.number,
            };
            let node = flow.value(false).map_err(|e| self.err(line.number, &e))?;
            only_comment(&flow.rest()).map_err(|e| self.err(line.number, &e))?;
            node
        } else {
            let value = scalar(text).map_err(|e| self.err(line.number, &e))?;
            Node::at(line.number, value)
        };
        if let Some(next) = self.lines.get(self.pos).filter(|l| deeper(l)) {
            return Err(self.err(
                next.number,
                "a line continuing an inline value (a multi-line scalar or a flow collection \
                 over several lines)",
            ));
        }
        Ok(node)
    }
}

/// The documents of one YAML stream, each parsed whole, with the line its
/// first text sits on.
pub fn parse_stream(source: &str, text: &str) -> Result<Vec<Node>, String> {
    let mut docs: Vec<Vec<Line>> = vec![Vec::new()];
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        if let Some(after) = raw.strip_prefix("---")
            && (after.is_empty() || after.starts_with([' ', '\t']))
        {
            only_comment(after).map_err(|_| {
                refused(source, number, "content on a `---` document separator line")
            })?;
            docs.push(Vec::new());
            continue;
        }
        if raw.starts_with("...") || raw.starts_with('%') {
            return Err(refused(
                source,
                number,
                "a document end marker or directive",
            ));
        }
        if raw.contains('\u{feff}') {
            // A byte-order mark hides the key it precedes from a reader
            // matching `kind:`, and the API server's decoder skips it.
            return Err(refused(source, number, "a byte-order mark"));
        }
        let body = raw.trim_start_matches(' ');
        if body.trim().is_empty() || body.starts_with('#') {
            continue;
        }
        if body.starts_with('\t') {
            return Err(refused(source, number, "tab indentation"));
        }
        let lines = docs.last_mut().ok_or("no document")?;
        expand(number, raw.len() - body.len(), body, lines);
    }
    docs.into_iter()
        .filter(|lines| !lines.is_empty())
        .map(|lines| {
            let mut parser = Parser {
                source,
                lines,
                pos: 0,
            };
            let root = parser.node(-1)?;
            match parser.lines.get(parser.pos) {
                Some(extra) => Err(parser.err(extra.number, "a second root in one document")),
                None => Ok(root),
            }
        })
        .collect()
}

// ---- Kubernetes objects -----------------------------------------------------

/// One document of the estate, read as a Kubernetes object.
#[derive(Debug, Clone)]
pub struct Object {
    pub source: String,
    pub line: usize,
    /// Empty for a document that is not a Kubernetes object (a Talos patch,
    /// a compose file).
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
    pub root: Node,
}

/// Top-level keys whose value must be written as a block below the key.
const BLOCK_ONLY: &[&str] = &["metadata", "roleRef", "subjects", "rules"];

/// The objects of one YAML stream, or the refusal naming the first shape
/// this reader will not judge.
pub fn read_stream(source: &str, text: &str) -> Result<Vec<Object>, String> {
    parse_stream(source, text)?
        .into_iter()
        .map(|root| object(source, root))
        .collect()
}

fn object(source: &str, root: Node) -> Result<Object, String> {
    let line = root.line;
    let Value::Map(entries) = &root.value else {
        return Err(refused(source, line, "a document that is not a mapping"));
    };
    if root.inline {
        return Err(refused(
            source,
            line,
            "a document written as one flow mapping — write it as a block",
        ));
    }
    for (key, node) in entries {
        if key == "items" {
            return Err(refused(
                source,
                node.line,
                "a top-level `items:` — `kubectl apply` unwraps a List, so what it holds is live",
            ));
        }
        if key == "aggregationRule" {
            return Err(refused(
                source,
                node.line,
                "an `aggregationRule` — the ClusterRole's rules are whatever other roles' labels \
                 say at run time, not what this file says",
            ));
        }
        if BLOCK_ONLY.contains(&key.as_str()) && node.inline {
            return Err(refused(
                source,
                node.line,
                &format!("`{key}:` written inline — write it as a block below the key"),
            ));
        }
    }
    let text_of = |key: &str, node: Option<&Node>| -> Result<Option<String>, String> {
        match node {
            None => Ok(None),
            Some(Node {
                value: Value::Str(s),
                ..
            }) => Ok(Some(s.clone())),
            Some(n) => Err(refused(
                source,
                n.line,
                &format!("a `{key}` that is not a scalar"),
            )),
        }
    };
    let kind = text_of("kind", root.get("kind"))?.unwrap_or_default();
    if kind.ends_with("List") {
        return Err(refused(
            source,
            line,
            &format!("`kind: {kind}` — `kubectl apply` unwraps a List, so what it holds is live"),
        ));
    }
    let metadata = root.get("metadata");
    if let Some(m) = metadata
        && !matches!(m.value, Value::Map(_) | Value::Null)
    {
        return Err(refused(
            source,
            m.line,
            "a `metadata` that is not a mapping",
        ));
    }
    let name = text_of("metadata.name", metadata.and_then(|m| m.get("name")))?.unwrap_or_default();
    let namespace = text_of(
        "metadata.namespace",
        metadata.and_then(|m| m.get("namespace")),
    )?;
    Ok(Object {
        source: source.to_string(),
        line,
        kind,
        name,
        namespace,
        root,
    })
}

/// One RBAC rule, every list read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rule {
    pub line: usize,
    pub api_groups: Vec<String>,
    pub resources: Vec<String>,
    pub verbs: Vec<String>,
    pub resource_names: Vec<String>,
    pub non_resource_urls: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Subject {
    pub line: usize,
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoleRef {
    pub kind: String,
    pub name: String,
}

/// The kinds that run a pod, and the path to the pod spec in each.
const POD_SPEC_AT: &[(&str, &[&str])] = &[
    ("Pod", &["spec"]),
    ("Deployment", &["spec", "template", "spec"]),
    ("StatefulSet", &["spec", "template", "spec"]),
    ("DaemonSet", &["spec", "template", "spec"]),
    ("ReplicaSet", &["spec", "template", "spec"]),
    ("ReplicationController", &["spec", "template", "spec"]),
    ("Job", &["spec", "template", "spec"]),
    ("PodTemplate", &["template", "spec"]),
    (
        "CronJob",
        &["spec", "jobTemplate", "spec", "template", "spec"],
    ),
];

impl Object {
    /// `Kind ns/name (source:line)`.
    pub fn label(&self) -> String {
        format!(
            "{} {}/{} ({}:{})",
            self.kind,
            self.namespace.as_deref().unwrap_or("-"),
            self.name,
            self.source,
            self.line
        )
    }

    fn refuse(&self, line: usize, what: &str) -> String {
        refused(&self.source, line, &format!("{}: {what}", self.label()))
    }

    /// The `(key, value)` entries of a mapping, refusing any key not in
    /// `known` — an unknown key is a shape this reader has not judged.
    fn entries<'a>(
        &self,
        node: &'a Node,
        known: &[&str],
    ) -> Result<Vec<(&'a str, &'a Node)>, String> {
        let Value::Map(entries) = &node.value else {
            return Err(self.refuse(node.line, "an entry that is not a mapping"));
        };
        entries
            .iter()
            .map(|(k, v)| {
                if known.contains(&k.as_str()) {
                    Ok((k.as_str(), v))
                } else {
                    Err(self.refuse(v.line, &format!("an unknown key `{k}`")))
                }
            })
            .collect()
    }

    fn scalar_of(&self, node: &Node) -> Result<String, String> {
        node.str()
            .map(str::to_string)
            .ok_or_else(|| self.refuse(node.line, "a value that is not a scalar"))
    }

    fn list_of(&self, node: &Node) -> Result<Vec<String>, String> {
        match &node.value {
            Value::Seq(items) => items.iter().map(|n| self.scalar_of(n)).collect(),
            Value::Null => Ok(Vec::new()),
            _ => Err(self.refuse(node.line, "a value that is not a list")),
        }
    }

    fn items(&self, key: &str) -> Result<Vec<&Node>, String> {
        match self.root.get(key) {
            None => Ok(Vec::new()),
            Some(Node {
                value: Value::Seq(items),
                ..
            }) => Ok(items.iter().collect()),
            Some(Node {
                value: Value::Null, ..
            }) => Ok(Vec::new()),
            Some(n) => Err(self.refuse(n.line, &format!("a `{key}:` that is not a list"))),
        }
    }

    /// A Role's or ClusterRole's rules.
    pub fn rules(&self) -> Result<Vec<Rule>, String> {
        self.items("rules")?
            .into_iter()
            .map(|item| {
                let mut rule = Rule {
                    line: item.line,
                    ..Rule::default()
                };
                for (key, value) in self.entries(
                    item,
                    &[
                        "apiGroups",
                        "resources",
                        "verbs",
                        "resourceNames",
                        "nonResourceURLs",
                    ],
                )? {
                    let list = self.list_of(value)?;
                    match key {
                        "apiGroups" => rule.api_groups = list,
                        "resources" => rule.resources = list,
                        "verbs" => rule.verbs = list,
                        "resourceNames" => rule.resource_names = list,
                        _ => rule.non_resource_urls = list,
                    }
                }
                Ok(rule)
            })
            .collect()
    }

    /// A binding's subjects.
    pub fn subjects(&self) -> Result<Vec<Subject>, String> {
        self.items("subjects")?
            .into_iter()
            .map(|item| {
                let mut subject = Subject {
                    line: item.line,
                    ..Subject::default()
                };
                for (key, value) in
                    self.entries(item, &["kind", "name", "namespace", "apiGroup"])?
                {
                    let text = self.scalar_of(value)?;
                    match key {
                        "kind" => subject.kind = text,
                        "name" => subject.name = text,
                        "namespace" => subject.namespace = Some(text),
                        _ => {}
                    }
                }
                Ok(subject)
            })
            .collect()
    }

    /// A binding's roleRef.
    pub fn role_ref(&self) -> Result<RoleRef, String> {
        let node = self
            .root
            .get("roleRef")
            .ok_or_else(|| self.refuse(self.line, "a binding with no roleRef"))?;
        let mut role = RoleRef::default();
        for (key, value) in self.entries(node, &["apiGroup", "kind", "name"])? {
            let text = self.scalar_of(value)?;
            match key {
                "kind" => role.kind = text,
                "name" => role.name = text,
                _ => {}
            }
        }
        Ok(role)
    }

    /// The pod spec of a kind that runs a pod.
    pub fn pod_spec(&self) -> Option<&Node> {
        POD_SPEC_AT
            .iter()
            .find(|(kind, _)| *kind == self.kind)
            .and_then(|(_, path)| self.root.path(path))
    }

    /// The ServiceAccount a pod-running object runs as: `serviceAccountName`,
    /// else the deprecated `serviceAccount` the API server still honours,
    /// else `default`. None for an object that runs no pod.
    pub fn service_account(&self) -> Option<String> {
        let spec = self.pod_spec()?;
        let named = |key| spec.get(key).and_then(Node::str).map(str::to_string);
        Some(
            named("serviceAccountName")
                .or_else(|| named("serviceAccount"))
                .unwrap_or_else(|| "default".to_string()),
        )
    }

    /// Every ServiceAccount this object names as the identity of a pod,
    /// wherever in its tree the key sits — so a kind this reader does not
    /// know the pod-spec path of is still counted.
    pub fn names_service_accounts(&self) -> Vec<String> {
        self.root
            .walk()
            .into_iter()
            .filter(|(k, _)| matches!(*k, "serviceAccountName" | "serviceAccount"))
            .filter_map(|(_, n)| n.str().map(str::to_string))
            .collect()
    }
}

// ---- who a subject reaches ---------------------------------------------------

/// What an RBAC subject grants to, as ServiceAccounts see it.
#[derive(Debug, Clone, PartialEq)]
pub enum Reach {
    /// One ServiceAccount: a `ServiceAccount` subject, or a `User` spelling
    /// its user name `system:serviceaccount:<ns>:<name>`.
    Account { namespace: String, name: String },
    /// `Group system:serviceaccounts:<ns>`.
    EveryAccountIn(String),
    /// `Group system:serviceaccounts`.
    EveryAccount,
    /// `Group system:authenticated` — every ServiceAccount is in it.
    Authenticated,
    /// A person, or a group no ServiceAccount is in.
    Other,
}

impl Reach {
    /// Does this reach ServiceAccount `name` in `namespace` (None: in any
    /// namespace)?
    pub fn includes(&self, namespace: Option<&str>, name: &str) -> bool {
        match self {
            Reach::Account {
                namespace: ns,
                name: n,
            } => n == name && namespace.is_none_or(|want| want == ns),
            Reach::EveryAccountIn(ns) => namespace.is_none_or(|want| want == ns),
            Reach::EveryAccount | Reach::Authenticated => true,
            Reach::Other => false,
        }
    }
}

/// Who `subject` of a binding in `binding_namespace` reaches. A
/// ServiceAccount subject with no namespace means the binding's own — the
/// authorizer's reading.
pub fn reach(subject: &Subject, binding_namespace: Option<&str>) -> Reach {
    match subject.kind.as_str() {
        "ServiceAccount" => Reach::Account {
            namespace: subject
                .namespace
                .clone()
                .or_else(|| binding_namespace.map(str::to_string))
                .unwrap_or_default(),
            name: subject.name.clone(),
        },
        "User" => subject
            .name
            .strip_prefix("system:serviceaccount:")
            .and_then(|rest| rest.split_once(':'))
            .map_or(Reach::Other, |(ns, name)| Reach::Account {
                namespace: ns.to_string(),
                name: name.to_string(),
            }),
        "Group" => match subject.name.as_str() {
            "system:serviceaccounts" => Reach::EveryAccount,
            "system:authenticated" => Reach::Authenticated,
            other => other
                .strip_prefix("system:serviceaccounts:")
                .map_or(Reach::Other, |ns| Reach::EveryAccountIn(ns.to_string())),
        },
        _ => Reach::Other,
    }
}

fn describe(subject: &Subject, reach: &Reach) -> String {
    match reach {
        Reach::Account { namespace, name } if subject.kind == "User" => {
            format!("User {} (ServiceAccount {namespace}/{name})", subject.name)
        }
        Reach::Account { namespace, name } => format!("ServiceAccount {namespace}/{name}"),
        _ => format!(
            "{} {} (every ServiceAccount in it)",
            subject.kind, subject.name
        ),
    }
}

// ---- what a rule reaches -------------------------------------------------------

/// The verbs that write an object.
pub const WRITE_VERBS: &[&str] = &["create", "update", "patch", "*"];

/// `(apiGroup, resource)`: a pod, or an object that owns a pod template a
/// controller turns into one. RBAC scopes a write to an object, never a
/// field, so a write here is a pod started with any Secret the namespace
/// holds and any ServiceAccount it has — a credential read (978bbd7d).
pub const POD_WRITERS: &[(&str, &str)] = &[
    ("", "pods"),
    ("", "replicationcontrollers"),
    ("apps", "deployments"),
    ("apps", "statefulsets"),
    ("apps", "daemonsets"),
    ("apps", "replicasets"),
    ("batch", "jobs"),
    ("batch", "cronjobs"),
];

const RBAC_GROUP: &str = "rbac.authorization.k8s.io";

const RBAC_OBJECTS: &[&str] = &[
    "roles",
    "rolebindings",
    "clusterroles",
    "clusterrolebindings",
];

/// The resource a rule's entry governs: `pods/exec` is a pod.
fn base(resource: &str) -> &str {
    resource.split('/').next().unwrap_or(resource)
}

impl Rule {
    fn verb(&self, verb: &str) -> bool {
        self.verbs.iter().any(|v| v == verb || v == "*")
    }
    fn any_verb(&self, verbs: &[&str]) -> bool {
        verbs.iter().any(|v| self.verb(v))
    }
    /// No apiGroups is not a narrower rule; read it as matching.
    fn group(&self, group: &str) -> bool {
        self.api_groups.is_empty() || self.api_groups.iter().any(|g| g == "*" || g == group)
    }
    /// The rule's resource entries that cover `resource` (a subresource
    /// covers its resource: `pods/exec` is a pod).
    fn covering(&self, resource: &str) -> Vec<&str> {
        self.resources
            .iter()
            .filter(|r| {
                let b = base(r);
                b == "*" || b == resource || r.as_str() == resource
            })
            .map(String::as_str)
            .collect()
    }
    /// The subresource `parent/sub` exactly: permission on the parent
    /// object does not reach a subresource, so only these spellings do.
    fn subresource(&self, parent: &str, sub: &str) -> bool {
        self.resources.iter().any(|r| {
            r == "*"
                || r == "*/*"
                || *r == format!("{parent}/{sub}")
                || *r == format!("{parent}/*")
                || *r == format!("*/{sub}")
        })
    }
    fn token(&self) -> bool {
        self.subresource("serviceaccounts", "token")
    }
}

const CERTIFICATES_GROUP: &str = "certificates.k8s.io";

const ADMISSION_GROUP: &str = "admissionregistration.k8s.io";

const WEBHOOK_CONFIGURATIONS: &[&str] = &[
    "mutatingwebhookconfigurations",
    "validatingwebhookconfigurations",
];

/// The CSR subresources kube-controller-manager's csrapproving controller
/// checks by SubjectAccessReview before it AUTO-approves a node-client CSR
/// (backlog cbdf1e9c).
const NODE_CLIENT_CSR: &[&str] = &["nodeclient", "selfnodeclient"];

/// CEL mutating admission — the policy and the binding that applies it
/// (backlog cbdf1e9c).
const MUTATING_ADMISSION_POLICIES: &[&str] = &[
    "mutatingadmissionpolicies",
    "mutatingadmissionpolicybindings",
];

/// What a rule lets its holder reach across the credential boundary, each
/// entry naming the resource and why it counts. Empty for a rule that
/// reaches none. ONE deny set for both pins, so they cannot disagree about
/// what a credential grant is:
///
///   * any verb on `secrets` — a write overwrites a broker Secret (swap the
///     tunnel secret), a list or watch returns every value;
///   * a write on a pod or a pod-template owner ([`POD_WRITERS`]) — the pod
///     starts as any ServiceAccount in the namespace, `boss` included, and
///     mounts any Secret;
///   * `create` on `serviceaccounts/token` — mints any account's token;
///   * `impersonate` on users, groups, serviceaccounts or uids;
///   * `bind` or `escalate` on roles, and any write on an RBAC object —
///     the holder grants itself the rest;
///
/// and, from review cbb5 (run 42ee3f9b, 2026-09-29):
///
///   * ANY verb on `nodes/proxy` — the kubelet authorizes a WebSocket exec,
///     attach or port-forward, which opens as a GET, as `get nodes/proxy`
///     (disclosed 2026; upstream called it working as intended), so the
///     read is an exec into any pod on the node, boss included;
///   * `update`/`patch` on `certificatesigningrequests/approval` or
///     `approve` on `signers` — with the `create` on CSRs that is easy to
///     come by, it mints a client certificate for any name, a
///     ServiceAccount's included. The approval half alone is flagged: it
///     is the authority, and it approves CSRs others filed;
///   * a write on a mutating or validating webhook configuration — the
///     API server then sends the holder every object it admits (a Secret
///     on its way in) and, if mutating, lets it rewrite a pod's identity;
///
/// and, from the release re-review of the same car (backlog cbdf1e9c):
///
///   * `create` on `certificatesigningrequests/nodeclient` or
///     `/selfnodeclient` (or a certificates-group wildcard covering them) —
///     kube-controller-manager auto-approves that CSR on exactly this
///     permission, so it mints a `system:node:<any>` client certificate,
///     which reads the Secrets of pods on that node, with no approval
///     grant. `create` on the CSR object alone stays clean: it files a
///     request someone else must approve;
///   * a write on a mutating admission policy or its binding — a CEL
///     mutation rewrites a pod's identity or command the way a mutating
///     webhook does.
pub fn credential_reach(rule: &Rule) -> Vec<String> {
    if rule.verbs.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if rule.group("") && !rule.covering("secrets").is_empty() {
        out.push(format!(
            "secrets (verbs {:?}: any verb on a Secret)",
            rule.verbs
        ));
    }
    if rule.any_verb(WRITE_VERBS) {
        for (group, writer) in POD_WRITERS {
            if rule.group(group) {
                out.extend(rule.covering(writer).into_iter().map(|r| {
                    let named = if group.is_empty() {
                        r.to_string()
                    } else {
                        format!("{group}/{r}")
                    };
                    format!("{named} (a pod write: it starts as any account and mounts any Secret)")
                }));
            }
        }
    }
    if rule.group("") && rule.verb("create") && rule.token() {
        out.push("serviceaccounts/token (create mints any account's token)".to_string());
    }
    if rule.verb("impersonate") {
        for target in ["users", "groups", "serviceaccounts", "uids", "userextras"] {
            if !rule.covering(target).is_empty() {
                out.push(format!("impersonate {target}"));
            }
        }
    }
    if rule.group(RBAC_GROUP) {
        for target in RBAC_OBJECTS {
            if rule.covering(target).is_empty() {
                continue;
            }
            if rule.any_verb(WRITE_VERBS) {
                out.push(format!("{RBAC_GROUP}/{target} (an RBAC write)"));
            }
            for verb in ["bind", "escalate"] {
                if rule.verb(verb) && target.ends_with("roles") {
                    out.push(format!("{verb} {target}"));
                }
            }
        }
    }
    if rule.group("") && rule.subresource("nodes", "proxy") {
        out.push(format!(
            "nodes/proxy (verbs {:?}: the kubelet reads a WebSocket exec as get nodes/proxy)",
            rule.verbs
        ));
    }
    if rule.group(CERTIFICATES_GROUP) {
        if rule.any_verb(&["update", "patch"])
            && rule.subresource("certificatesigningrequests", "approval")
        {
            out.push(
                "certificatesigningrequests/approval (approving a CSR mints a client certificate)"
                    .to_string(),
            );
        }
        if rule.verb("approve") && !rule.covering("signers").is_empty() {
            out.push("approve signers (approving a CSR mints a client certificate)".to_string());
        }
        if rule.verb("create") {
            for sub in NODE_CLIENT_CSR {
                if rule.subresource("certificatesigningrequests", sub) {
                    out.push(format!(
                        "certificatesigningrequests/{sub} (create is auto-approved: it mints a system:node client certificate)"
                    ));
                }
            }
        }
    }
    if rule.group(ADMISSION_GROUP) && rule.any_verb(WRITE_VERBS) {
        for target in WEBHOOK_CONFIGURATIONS {
            if !rule.covering(target).is_empty() {
                out.push(format!(
                    "{ADMISSION_GROUP}/{target} (a webhook is sent every object admitted)"
                ));
            }
        }
        for target in MUTATING_ADMISSION_POLICIES {
            if !rule.covering(target).is_empty() {
                out.push(format!(
                    "{ADMISSION_GROUP}/{target} (a CEL mutation rewrites a pod's identity or command)"
                ));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// What [`credential_grants`] found.
#[derive(Debug, Default, PartialEq)]
pub struct Judgement {
    /// Bindings in scope that reach the account at all, dangerous or not —
    /// a control: zero means the reader saw nothing, not that it is clean.
    pub bindings: usize,
    /// One line per rule that reaches a credential, and per binding whose
    /// role no object declares (its rules cannot be read).
    pub findings: Vec<String>,
}

/// Every binding whose subjects `reaches` accepts, in a namespace `in_scope`
/// accepts (None: a ClusterRoleBinding, which is every namespace), resolved
/// to the roles it grants, and every rule of those roles that
/// [`credential_reach`] names. `why` ends each finding.
pub fn credential_grants(
    objects: &[Object],
    reaches: impl Fn(&Reach) -> bool,
    in_scope: impl Fn(Option<&str>) -> bool,
    why: &str,
) -> Result<Judgement, String> {
    let mut judgement = Judgement::default();
    for binding in objects
        .iter()
        .filter(|o| matches!(o.kind.as_str(), "RoleBinding" | "ClusterRoleBinding"))
    {
        let namespace = binding.namespace.as_deref();
        if binding.kind == "RoleBinding" && namespace.is_none() {
            return Err(binding.refuse(
                binding.line,
                "a RoleBinding with no namespace lands wherever the applier's context \
                 points, so where it grants cannot be read",
            ));
        }
        let scope = if binding.kind == "RoleBinding" {
            namespace
        } else {
            None
        };
        if !in_scope(scope) {
            continue;
        }
        let who: Vec<String> = binding
            .subjects()?
            .iter()
            .filter_map(|s| {
                let r = reach(s, namespace);
                reaches(&r).then(|| describe(s, &r))
            })
            .collect();
        if who.is_empty() {
            continue;
        }
        judgement.bindings += 1;
        let role_ref = binding.role_ref()?;
        let roles: Vec<&Object> = objects
            .iter()
            .filter(|o| {
                o.kind == role_ref.kind
                    && o.name == role_ref.name
                    && (o.kind == "ClusterRole" || o.namespace.as_deref() == namespace)
            })
            .collect();
        let who = who.join(", ");
        if roles.is_empty() {
            judgement.findings.push(format!(
                "{}: gives {who} {} {}, which no manifest declares — its rules cannot be \
                 read, so it is refused",
                binding.label(),
                role_ref.kind,
                role_ref.name
            ));
        }
        for role in roles {
            for rule in role.rules()? {
                let reached = credential_reach(&rule);
                if reached.is_empty() {
                    continue;
                }
                judgement.findings.push(format!(
                    "{}:{}: {} {} (bound to {who} by {}) grants verbs {:?} on {} — {why}",
                    role.source,
                    rule.line,
                    role.kind,
                    role.name,
                    binding.label(),
                    rule.verbs,
                    reached.join(", "),
                ));
            }
        }
    }
    Ok(judgement)
}

// ---- the estate ------------------------------------------------------------------

/// One manifest file as the converge sees it.
#[derive(Debug, Clone)]
pub struct ManifestFile {
    /// `<namespace>/<file>` for a rendered file, the repo path otherwise.
    pub label: String,
    /// The instance namespace whose render holds it; None for a file outside
    /// the render.
    pub instance: Option<String>,
    pub text: String,
}

/// Every manifest the credential pins judge.
#[derive(Debug, Clone)]
pub struct Estate {
    /// The instance whose render also carries the `pipeline` set.
    pub source: String,
    /// Every instance namespace, in instances.toml order.
    pub instances: Vec<String>,
    pub files: Vec<ManifestFile>,
}

fn run_render(args: &[&str]) -> String {
    let run = Command::new("bash")
        .arg(repo_root().join(RENDER))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("bash runs {RENDER}: {e}"));
    assert!(
        run.status.success(),
        "{RENDER} {args:?}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout).into_owned()
}

fn yaml_files(dir: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            yaml_files(&path, into);
        } else if matches!(
            path.extension().and_then(|x| x.to_str()),
            Some("yaml" | "yml")
        ) {
            into.push(path);
        }
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The estate of the checkout this binary was built from, rendered once
/// per test binary.
pub fn estate() -> &'static Estate {
    static ESTATE: OnceLock<Estate> = OnceLock::new();
    ESTATE.get_or_init(|| {
        let root = repo_root();
        let instances: Vec<String> = run_render(&["--instances"])
            .lines()
            .filter_map(|l| l.split('\t').nth(1).map(str::to_string))
            .collect();
        assert!(
            instances.len() >= 2,
            "render-instance.sh --instances names prod and the playground: {instances:?}"
        );
        let source = run_render(&["--source"]).trim().to_string();
        let out = scratch_dir("rbac-estate").join("out");
        run_render(&["--all", out.to_str().unwrap_or_default()]);
        let mut files = Vec::new();
        for ns in &instances {
            let mut paths = Vec::new();
            yaml_files(&out.join(ns), &mut paths);
            assert!(!paths.is_empty(), "the render wrote nothing for {ns}");
            files.extend(paths.iter().map(|p| ManifestFile {
                label: format!(
                    "{ns}/{}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ),
                instance: Some(ns.clone()),
                text: read(p),
            }));
        }
        let mut rest = Vec::new();
        yaml_files(&root.join("infra"), &mut rest);
        let manifests = root.join(MANIFESTS);
        files.extend(
            rest.iter()
                .filter(|p| !p.starts_with(&manifests))
                .map(|p| ManifestFile {
                    label: p.strip_prefix(&root).unwrap_or(p).display().to_string(),
                    instance: None,
                    text: read(p),
                }),
        );
        Estate {
            source,
            instances,
            files,
        }
    })
}

impl Estate {
    /// Every object in the estate, or the first refusal.
    pub fn objects(&self) -> Result<Vec<Object>, String> {
        self.objects_where(|_| true)
    }

    /// The objects of one instance namespace's render.
    pub fn objects_in(&self, instance: &str) -> Result<Vec<Object>, String> {
        self.objects_where(|f| f.instance.as_deref() == Some(instance))
    }

    fn objects_where(&self, keep: impl Fn(&ManifestFile) -> bool) -> Result<Vec<Object>, String> {
        let mut out = Vec::new();
        for f in self.files.iter().filter(|f| keep(f)) {
            out.extend(read_stream(&f.label, &f.text)?);
        }
        Ok(out)
    }

    /// This estate with one file's text rewritten. Panics when `label`
    /// names no file or the edit changes nothing — a mutation that edits
    /// nothing proves nothing.
    pub fn edited(&self, label: &str, from: &str, to: &str) -> Estate {
        let mut next = self.clone();
        let file = next
            .files
            .iter_mut()
            .find(|f| f.label == label)
            .unwrap_or_else(|| panic!("no manifest {label} in the estate"));
        assert!(
            file.text.contains(from),
            "{label} no longer holds the text this mutation edits: {from:?}"
        );
        file.text = file.text.replacen(from, to, 1);
        next
    }

    /// This estate with one more file in `instance`'s render.
    pub fn with_file(&self, instance: &str, name: &str, text: &str) -> Estate {
        let mut next = self.clone();
        next.files.push(ManifestFile {
            label: format!("{instance}/{name}"),
            instance: Some(instance.to_string()),
            text: text.to_string(),
        });
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> Node {
        let mut docs = parse_stream("t.yaml", text).expect("parses");
        assert_eq!(docs.len(), 1);
        docs.remove(0)
    }

    #[test]
    fn block_flow_and_compact_forms_read_as_one_tree() {
        let block = one("a:\n  - x: '1'\n    y: [p, \"q\", '']\n  - z\nb: {c: {d: e}, f: [g]}\n");
        let a = block.get("a").expect("a");
        let Value::Seq(items) = &a.value else {
            panic!("{a:?}")
        };
        assert_eq!(items[0].get("x").and_then(Node::str), Some("1"));
        let y = items[0].get("y").expect("y");
        assert_eq!(
            y.value,
            Value::Seq(vec![
                Node {
                    line: 3,
                    inline: true,
                    value: Value::Str("p".into())
                },
                Node {
                    line: 3,
                    inline: true,
                    value: Value::Str("q".into())
                },
                Node {
                    line: 3,
                    inline: true,
                    value: Value::Str(String::new())
                },
            ])
        );
        assert_eq!(items[1].str(), Some("z"));
        assert_eq!(block.path(&["b", "c", "d"]).and_then(Node::str), Some("e"));
        // A sequence at its key's own indent, and a quoted key.
        let same = one("subjects:\n- kind: User\n  name: 'it''s'\n\"kind\": X\n");
        assert_eq!(same.path(&["kind"]).and_then(Node::str), Some("X"));
        let Value::Seq(s) = &same.get("subjects").expect("subjects").value else {
            panic!()
        };
        assert_eq!(s[0].get("name").and_then(Node::str), Some("it's"));
    }

    #[test]
    fn a_block_scalar_is_skipped_whole() {
        let doc = one("a: |\n  kind: Role\n  - x: y\nb: >-\n    t\nc: d # note\n");
        assert_eq!(doc.get("a").map(|n| &n.value), Some(&Value::Text));
        assert_eq!(doc.get("c").and_then(Node::str), Some("d"));
        let seq = one("args:\n  - |\n    set -e\n    x: y\n  - z\n");
        let Value::Seq(items) = &seq.get("args").expect("args").value else {
            panic!()
        };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn every_shape_it_cannot_read_is_refused_by_line() {
        for (text, line) in [
            ("a: &x 1\n", 1),
            ("a: *x\n", 1),
            ("a: !!str 1\n", 1),
            ("<<: {a: b}\n", 1),
            ("a: \"x\\n\"\n", 1),
            ("a: 'open\n", 1),
            ("a: [x,\n  y]\n", 1),
            ("a: b\n  c\n", 2),
            ("a: 1\na: 2\n", 2),
            ("? a\n: b\n", 1),
            ("a:\n\t- b\n", 2),
            ("%YAML 1.2\n", 1),
            ("--- !tag\na: b\n", 1),
            ("- a\n", 1),
        ] {
            let got = read_stream("t.yaml", text);
            let err = got.expect_err(text);
            assert!(
                err.starts_with(&format!("t.yaml:{line}:")) && err.ends_with("refused, not passed"),
                "{text:?}: {err}"
            );
        }
    }

    #[test]
    fn a_list_an_aggregated_role_and_an_inline_rbac_key_are_refused() {
        for text in [
            "kind: List\napiVersion: v1\n",
            "kind: RoleBindingList\n",
            "apiVersion: v1\nitems:\n  - kind: RoleBinding\n",
            "kind: ClusterRole\naggregationRule:\n  clusterRoleSelectors: []\n",
            "kind: RoleBinding\nsubjects: [{kind: ServiceAccount, name: default}]\n",
            "kind: Role\nrules: [{apiGroups: [\"\"], resources: [secrets], verbs: [get]}]\n",
            "kind: RoleBinding\nmetadata: {name: b, namespace: boss}\n",
            "kind: RoleBinding\nroleRef: {kind: Role, name: r}\n",
            "{kind: RoleBinding}\n",
        ] {
            let err = read_stream("t.yaml", text).expect_err(text);
            assert!(err.ends_with("refused, not passed"), "{text:?}: {err}");
        }
    }

    #[test]
    fn a_comment_after_a_tab_a_null_and_a_yaml_1_1_boolean_read_as_the_api_server_reads_them() {
        let doc =
            one("a: default\t# note\nb: x#y\nc: ~\nd: null\ne: yes\nf: 'yes'\ng: [null, p]\n");
        assert_eq!(doc.get("a").and_then(Node::str), Some("default"));
        assert_eq!(doc.get("b").and_then(Node::str), Some("x#y"));
        assert_eq!(doc.get("c").map(|n| &n.value), Some(&Value::Null));
        assert_eq!(doc.get("d").map(|n| &n.value), Some(&Value::Null));
        assert_eq!(doc.get("e").and_then(yaml_bool), Some(true));
        assert_eq!(
            doc.get("g").map(|n| &n.value).and_then(|v| match v {
                Value::Seq(items) => items.first().map(|n| n.value.clone()),
                _ => None,
            }),
            Some(Value::Null)
        );
        // A null where a subject needs a name is refused, not read as "~".
        let objects = read_stream(
            "t.yaml",
            "kind: RoleBinding\nmetadata:\n  name: b\n  namespace: ns\nsubjects:\n  - kind: ServiceAccount\n    name: ~\n",
        )
        .expect("reads");
        assert!(
            objects[0]
                .subjects()
                .expect_err("refused")
                .ends_with("refused, not passed")
        );
        let bom = read_stream("t.yaml", "\u{feff}kind: RoleBinding\n").expect_err("a BOM");
        assert!(bom.starts_with("t.yaml:1:"), "{bom}");
    }

    #[test]
    fn a_user_spelling_a_service_account_reaches_it() {
        let s = |kind: &str, name: &str| Subject {
            kind: kind.into(),
            name: name.into(),
            ..Subject::default()
        };
        assert!(
            reach(&s("User", "system:serviceaccount:boss:boss"), None)
                .includes(Some("boss"), "boss")
        );
        assert!(
            !reach(&s("User", "system:serviceaccount:boss:boss"), None)
                .includes(Some("boss"), "default")
        );
        assert!(reach(&s("ServiceAccount", "default"), Some("ns")).includes(Some("ns"), "default"));
        assert!(reach(&s("Group", "system:serviceaccounts:ns"), None).includes(Some("ns"), "x"));
        assert!(
            !reach(&s("Group", "system:serviceaccounts:ns"), None).includes(Some("other"), "x")
        );
        assert!(reach(&s("Group", "system:authenticated"), None).includes(None, "default"));
        assert_eq!(reach(&s("User", "david"), None), Reach::Other);
    }

    #[test]
    fn the_deny_set_names_every_credential_class_and_passes_a_read() {
        let rule = |groups: &[&str], resources: &[&str], verbs: &[&str]| Rule {
            api_groups: groups.iter().map(|s| s.to_string()).collect(),
            resources: resources.iter().map(|s| s.to_string()).collect(),
            verbs: verbs.iter().map(|s| s.to_string()).collect(),
            ..Rule::default()
        };
        let hits = |r: Rule, want: &str| {
            let got = credential_reach(&r);
            assert!(got.iter().any(|g| g.contains(want)), "{want}: {got:?}");
        };
        hits(rule(&[""], &["secrets"], &["patch"]), "secrets");
        hits(rule(&[""], &["secrets"], &["list"]), "secrets");
        hits(rule(&["*"], &["*"], &["get"]), "secrets");
        hits(rule(&[""], &["pods"], &["create"]), "pods");
        hits(
            rule(&[""], &["replicationcontrollers"], &["patch"]),
            "replicationcontrollers",
        );
        hits(
            rule(&["apps"], &["deployments"], &["patch"]),
            "apps/deployments",
        );
        hits(rule(&[""], &["pods/exec"], &["create"]), "pods/exec");
        hits(
            rule(&[""], &["serviceaccounts/token"], &["create"]),
            "serviceaccounts/token",
        );
        hits(
            rule(&[""], &["serviceaccounts"], &["impersonate"]),
            "impersonate serviceaccounts",
        );
        hits(
            rule(&[RBAC_GROUP], &["clusterroles"], &["bind"]),
            "bind clusterroles",
        );
        hits(
            rule(&[RBAC_GROUP], &["roles"], &["escalate"]),
            "escalate roles",
        );
        hits(
            rule(&[RBAC_GROUP], &["rolebindings"], &["create"]),
            "rolebindings",
        );
        hits(rule(&[""], &["nodes/proxy"], &["get"]), "nodes/proxy");
        hits(rule(&[""], &["nodes/*"], &["list"]), "nodes/proxy");
        hits(
            rule(
                &[CERTIFICATES_GROUP],
                &["certificatesigningrequests/approval"],
                &["update"],
            ),
            "certificatesigningrequests/approval",
        );
        hits(
            rule(&[CERTIFICATES_GROUP], &["signers"], &["approve"]),
            "approve signers",
        );
        // Review cbb5 (backlog cbdf1e9c): create on a node-client CSR
        // subresource is auto-approved by kube-controller-manager, so it
        // mints a system:node client certificate with no approval grant.
        hits(
            rule(
                &[CERTIFICATES_GROUP],
                &["certificatesigningrequests/nodeclient"],
                &["create"],
            ),
            "certificatesigningrequests/nodeclient",
        );
        hits(
            rule(
                &[CERTIFICATES_GROUP],
                &["certificatesigningrequests/selfnodeclient"],
                &["create"],
            ),
            "certificatesigningrequests/selfnodeclient",
        );
        hits(
            rule(&[CERTIFICATES_GROUP], &["*/*"], &["create"]),
            "certificatesigningrequests/nodeclient",
        );
        hits(
            rule(
                &[CERTIFICATES_GROUP],
                &["certificatesigningrequests/*"],
                &["*"],
            ),
            "certificatesigningrequests/selfnodeclient",
        );
        hits(
            rule(
                &[ADMISSION_GROUP],
                &["mutatingwebhookconfigurations"],
                &["create"],
            ),
            "mutatingwebhookconfigurations",
        );
        hits(
            rule(
                &[ADMISSION_GROUP],
                &["validatingwebhookconfigurations"],
                &["patch"],
            ),
            "validatingwebhookconfigurations",
        );
        // Review cbb5 (backlog cbdf1e9c): a CEL mutating admission policy
        // rewrites a pod's identity or command the way a mutating webhook
        // does, and its binding is what makes it apply.
        hits(
            rule(
                &[ADMISSION_GROUP],
                &["mutatingadmissionpolicies"],
                &["create"],
            ),
            "mutatingadmissionpolicies",
        );
        hits(
            rule(
                &[ADMISSION_GROUP],
                &["mutatingadmissionpolicybindings"],
                &["patch"],
            ),
            "mutatingadmissionpolicybindings",
        );
        for clean in [
            rule(&[""], &["pods", "pods/log"], &["get", "list", "watch"]),
            rule(&["apps"], &["deployments"], &["get"]),
            rule(&[""], &["serviceaccounts"], &["get"]),
            rule(&[""], &["configmaps"], &["patch"]),
            rule(&[""], &["nodes", "nodes/status"], &["get", "list"]),
            rule(
                &[CERTIFICATES_GROUP],
                &["certificatesigningrequests"],
                &["create", "get"],
            ),
            rule(
                &[ADMISSION_GROUP],
                &["mutatingwebhookconfigurations"],
                &["get", "list"],
            ),
            // A read of the node-client subresource files nothing, and a
            // read of a mutating policy or binding changes no admission.
            rule(
                &[CERTIFICATES_GROUP],
                &["certificatesigningrequests/nodeclient"],
                &["get"],
            ),
            rule(
                &[ADMISSION_GROUP],
                &[
                    "mutatingadmissionpolicies",
                    "mutatingadmissionpolicybindings",
                ],
                &["get", "list", "watch"],
            ),
        ] {
            assert_eq!(credential_reach(&clean), Vec::<String>::new(), "{clean:?}");
        }
    }
}

#!/usr/bin/env bash
# infra/cluster/talos/check-declared.sh <node> [live.yaml] — compare a
# node's LIVE Talos machine config against the entries the tree declares
# for it in patches/<node>.yaml (backlog 08430090).
#
#   talosctl -n <ip> get machineconfig -o yaml | infra/cluster/talos/check-declared.sh w-1
#   infra/cluster/talos/check-declared.sh cp-2 cp-2.live.yaml
#
# WHY. The gate depends on machine-config entries (the gate-seed
# directory + kubelet bind on w-1, the image-GC thresholds, the registry
# mirror every node pulls through) that were applied by hand and lived
# on a laptop. A rebuilt w-1 loses the seed mount and the first symptom
# is every gate hanging (d42d4967, one day). Declaring them in the tree
# is half; this is the other half — a comparator, so "the node carries
# what the tree says" is a read and not a belief (CLAUDE.md §Mostly sure
# vs. absolutely sure). The check takes the live config as INPUT because
# it needs no talosconfig: David can pipe it from his Mac, and the
# forge's `node-converge` verb feeds it before and after it applies a
# declaration (design 1bc4b4ed; backlog 9d56c616), never printing the
# config itself.
#
# VOCABULARY (design 16115a17), one line per finding, then a summary:
#   MATCH      declared entry present live with the declared value
#   DRIFT      present live with a different value — BOTH printed
#   ABSENT     declared, not live
#   DOUBLED    a declared files/extraMounts entry appears more than once
#              live (w-1 as found 2026-09-15: the seed file entry twice,
#              from two identical hand patches on 2026-09-12)
#   UNDECLARED live, in a class this check reads, named by no declaration
#              — the third state: reported, not a failure
#   UNVERIFIED the live config names no hostname, so the check cannot
#              tell whose config it read; printed first and repeated in
#              the summary, the exit status unchanged (review a79746c6)
# Classes read for UNDECLARED: machine.files under /var/local, every
# machine.kubelet.extraMounts entry, machine.kubelet.extraConfig imageGC*
# keys, every machine.registries.mirrors host, every UserVolumeConfig.
#
# USER VOLUMES (backlog 52ea56ac, 2026-09-30). A declaration is ONE
# machine fragment plus any number of `kind: UserVolumeConfig`
# documents after `---` (w-1's gate disk), compared by `name` in the
# same vocabulary. Any other document kind in a declaration is REFUSED
# (exit 2, naming it) rather than skipped. Live, a user volume is a
# second document of the SAME config: `talosctl get machineconfig`
# prints a multi-document config as a block-string spec, and `talosctl
# read /system/state/config.yaml` prints the stored file itself, every
# document at column 0 — both are read, and the latter is the one to
# feed the check, being what the node holds.
#
# EXIT: 0 every declared entry MATCHes (UNDECLARED may be listed);
#       1 any DRIFT, ABSENT or DOUBLED;
#       2 usage — no node, no declaration for it, unreadable input, no
#         machine config in the input, or a live config whose hostname is
#         NOT the node asked for (a wrong target answers instead of
#         erroring; the document names its host, so it is checked);
#         — and, in the declaration or the live input, any character
#         Talos's YAML reads as a line break or control and this reader
#         does not (NEL, LS, PS, CR, TAB, a BOM past byte 0, any other
#         C0/C1 control), refused before any parse, naming the line and
#         the codepoint (review 5ed37182, finding 1)
#         — and a declaration NOT IN CANONICAL FORM (the one block form
#         this reader re-emits from what it parsed; see "a declaration is
#         in CANONICAL FORM" below), naming the first line that differs,
#         and, in either input, content on a `---`/`...` line, a
#         non-empty flow sequence or mapping, or lines left after a
#         document's root node (review a7fa61ec)
#         — and ANY error the reader did not expect (a YAML escape
#         json.loads cannot decode, say): one `UNREADABLE` line, never a
#         traceback, and never exit 1, which callers read as findings
#         (review be5ba8f7, finding 2);
#       3 (--only-read-classes only) the declaration names an entry in no
#         class this check reads or outside EXTRA_CONFIG_KEYS, each
#         printed as UNREAD, or a user volume whose diskSelector is not in
#         the allowlist grammar (SELECTOR_TERM), printed as REFUSED;
#      78 python3 is not on this box.
#
# PARSING. The gate image and this pod carry python3 and no PyYAML
# (infra/forge/boss-ci/required-tools.txt lists python3 alone), so the
# YAML reader below is a deliberately SMALL stdlib parser for the subset
# talosctl and these patch files use: block mappings and sequences,
# plain and quoted scalars, `|` block scalars, comments, `---` documents,
# empty `[]`/`{}` and flat flow lists. Every scalar is compared as its
# text (`40` and `"40"` are equal; so are `0o644` and `0o644`). Anchors,
# aliases, tags or nested flow collections stop the check with exit 2
# naming the line rather than reading past it — a parser that guesses
# would report a clean node from a document it did not understand.
#
# The live document is printed by talosctl as MORE THAN ONE resource
# (two copies on every node in David's 2026-09-15 read); the check reads
# the `v1alpha1` one, or the first carrying `machine:`, and says which.
# Counting across copies would read every node as DOUBLED.
set -u

usage() {
    echo "usage: $0 <node> [live-machineconfig.yaml]   (stdin when no file)" >&2
    echo "       $0 --only-read-classes <node>          (the declaration alone; no live input)" >&2
    echo "  declarations: \${BOSS_TALOS_PATCHES:-<beside this script>/patches}/<node>.yaml" >&2
    exit 2
}

# --only-read-classes (backlog 9d56c616): judge the DECLARATION alone —
# exit 0 when every entry it names sits in a class this check reads, 3
# naming each one it does not (and each extraConfig key outside
# EXTRA_CONFIG_KEYS, the kubelet keys a declaration may carry, and each
# UserVolumeConfig whose diskSelector is not in the allowlist grammar
# that proves it excludes the system disk, printed as REFUSED). node-converge applies a declaration and
# then reads its effect back through this check, so an entry the check
# cannot read is an effect nobody could prove; the verb refuses it
# before anything is applied. The classes are READ_CLASSES below, the
# same four the comparison walks — one definition, not two lists.
mode=compare
if [ "${1:-}" = "--only-read-classes" ]; then
    mode=classes
    shift
    [ $# -eq 1 ] || usage
fi

node="${1:-}"
[ -n "$node" ] || usage
case "$node" in -h|--help) usage ;; esac
live="${2:--}"

here="${BASH_SOURCE[0]%/*}"
[ "$here" != "${BASH_SOURCE[0]}" ] || here=.
patches="${BOSS_TALOS_PATCHES:-$here/patches}"
declared="$patches/$node.yaml"
if [ ! -f "$declared" ]; then
    echo "check-declared: no declaration for node '$node' at $declared" >&2
    exit 2
fi
if [ "$live" != "-" ] && [ ! -r "$live" ]; then
    echo "check-declared: cannot read live config file: $live" >&2
    exit 2
fi
if ! command -v python3 >/dev/null 2>&1; then
    echo "check-declared: python3 is not on this box — cannot read the config" >&2
    exit 78
fi

# The program rides on fd 3, not stdin: stdin is the live config when no
# file is named, and a heredoc on stdin would BE the program's stdin —
# the check would read an empty document and refuse a healthy node.
python3 /dev/fd/3 "$node" "$declared" "$live" "$mode" 3<<'PY'
import json
import os
import re
import sys


# ANY error this program did not expect is exit 2, UNREADABLE — never a
# traceback and exit 1 (review be5ba8f7, finding 2). Exit 1 means
# "findings", and a caller reading a crash as findings once signed a plan
# saying every entry was ABSENT over a config nobody read (a YAML-only
# escape such as \x41, which json.loads cannot decode). Only the error's
# type and this program's line are printed: its message may quote the
# live config, whose bytes carry the cluster's keys.
def _unreadable(etype, value, tb):
    line = "?"
    while tb is not None:
        line = tb.tb_lineno
        tb = tb.tb_next
    sys.stdout.flush()
    print(f"check-declared: UNREADABLE — the input could not be read ({etype.__name__} at line {line} of the reader); no verdict", file=sys.stderr)
    sys.stderr.flush()
    os._exit(2)


sys.excepthook = _unreadable

node, declared_path, live_path, mode = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]


class Unsupported(Exception):
    pass


# ---- the subset YAML reader ------------------------------------------

def _strip_comment(s):
    """Drop a ` #` comment from a plain scalar line; quotes respected."""
    q = None
    for i, ch in enumerate(s):
        if q:
            if ch == q:
                q = None
        elif ch in ('"', "'"):
            q = ch
        elif ch == "#" and (i == 0 or s[i - 1] in " \t"):
            return s[:i].rstrip()
    return s.rstrip()


def _scalar(text, lineno):
    text = text.strip()
    if text == "":
        return ""
    if text[0] in "&*!":
        raise Unsupported(f"line {lineno}: anchors, aliases and tags are not read")
    if text[0] == '"':
        if not text.endswith('"') or len(text) < 2:
            raise Unsupported(f"line {lineno}: unterminated double-quoted scalar")
        return json.loads(text)
    if text[0] == "'":
        if not text.endswith("'") or len(text) < 2:
            raise Unsupported(f"line {lineno}: unterminated single-quoted scalar")
        return text[1:-1].replace("''", "'")
    if text == "[]":
        return []
    if text == "{}":
        return {}
    if text[0] == "[":
        # Only the empty `[]` is read: a comma split read `[a: b]` as the
        # scalar "a: b" where yaml.v3 reads a mapping (review a7fa61ec).
        raise Unsupported(f"line {lineno}: flow sequences are not read (only an empty [])")
    if text[0] == "{":
        raise Unsupported(f"line {lineno}: flow mappings are not read")
    if text in ("~", "null"):
        return None
    return text


def _split_key(content, lineno):
    """`key: value` / `key:` → (key, rest). Keys may hold ':' (a
    registry host `10.20.0.15:3000:`), so the split is the first ': '
    or a trailing ':'."""
    q = None
    for i, ch in enumerate(content):
        if q:
            if ch == q:
                q = None
            continue
        if ch in ('"', "'") and i == 0:
            q = ch
            continue
        if ch == ":" and (i + 1 == len(content) or content[i + 1] in " \t"):
            key = content[:i].strip()
            if key and key[0] in "\"'":
                key = _scalar(key, lineno)
            return key, content[i + 1:].strip()
    return None, None


class Reader:
    def __init__(self, text):
        self.lines = text.split("\n")
        self.n = len(self.lines)

    def _indent(self, i):
        line = self.lines[i]
        return len(line) - len(line.lstrip(" "))

    def _blank(self, i):
        s = self.lines[i].strip()
        return s == "" or s.startswith("#")

    def _next(self, i):
        while i < self.n and self._blank(i):
            i += 1
        return i

    def _block_scalar(self, i, header, parent_indent):
        """`|`, `|-`, `|+`, `>` (and `>-`, `>+`) at line i; the body is
        the following lines indented deeper than the parent."""
        style, chomp = header[0], header[1:]
        if style not in "|>" or chomp not in ("", "-", "+"):
            raise Unsupported(f"line {i + 1}: block scalar header {header!r} is not read")
        j = i + 1
        body_indent = None
        body = []
        while j < self.n:
            line = self.lines[j]
            if line.strip() == "":
                body.append("")
                j += 1
                continue
            ind = len(line) - len(line.lstrip(" "))
            if ind <= parent_indent or (body_indent is not None and ind < body_indent):
                break
            if body_indent is None:
                body_indent = ind
            body.append(line[body_indent:])
            j += 1
        trailing = 0
        while body and body[-1] == "":
            body.pop()
            trailing += 1
        text = "\n".join(body) if style == "|" else " ".join(body)
        if chomp == "":
            text += "\n"
        elif chomp == "+":
            text += "\n" * (1 + trailing)
        return text, j

    def _value(self, rest, i, indent):
        """The value after `key:` or `- `: inline scalar, block scalar,
        or a nested block on following lines."""
        rest = _strip_comment(rest)
        if rest.startswith("|") or rest.startswith(">"):
            return self._block_scalar(i, rest, indent)
        if rest != "":
            return _scalar(rest, i + 1), i + 1
        j = self._next(i + 1)
        if j < self.n and self._indent(j) > indent:
            return self._node(j, self._indent(j))
        if j < self.n and self._indent(j) == indent and self.lines[j].strip().startswith("- "):
            # A sequence at its key's own column (`files:` / `- path:`),
            # which YAML allows and some writers emit.
            return self._sequence(j, indent)
        return None, i + 1

    def _node(self, i, indent):
        content = self.lines[i].strip()
        if content == "-" or content.startswith("- "):
            return self._sequence(i, indent)
        return self._mapping(i, indent)

    def _mapping(self, i, indent):
        out = {}
        while i < self.n:
            i = self._next(i)
            if i >= self.n or self._indent(i) < indent:
                break
            if self._indent(i) > indent:
                raise Unsupported(f"line {i + 1}: unexpected indentation")
            content = self.lines[i].strip()
            if content.startswith("- "):
                break
            key, rest = _split_key(content, i + 1)
            if key is None:
                raise Unsupported(f"line {i + 1}: expected `key: value`, got {content!r}")
            value, i = self._value(rest, i, indent)
            if key in out:
                raise Unsupported(f"line {i}: duplicate key {key!r}")
            out[key] = value
        return out, i

    def _sequence(self, i, indent):
        out = []
        while i < self.n:
            i = self._next(i)
            if i >= self.n or self._indent(i) < indent:
                break
            if self._indent(i) > indent:
                raise Unsupported(f"line {i + 1}: unexpected indentation")
            content = self.lines[i].strip()
            if content == "-":
                value, i = self._value("", i, indent)
                out.append(value)
                continue
            if not content.startswith("- "):
                break
            item = content[2:].lstrip(" ")
            item_indent = indent + (len(content) - len(item))
            if item == "-" or item.startswith("- "):
                # `- - nested`: a sequence that begins on the dash line.
                self.lines[i] = " " * item_indent + item
                value, i = self._sequence(i, item_indent)
                out.append(value)
                continue
            key, _rest = _split_key(item, i + 1)
            if key is not None and item[0] not in "[{\"'":
                # `- key: value`: a mapping that begins on the dash line;
                # the rest of its keys sit at the item's column.
                self.lines[i] = " " * item_indent + item
                value, i = self._mapping(i, item_indent)
                out.append(value)
                continue
            value, i = self._value(item, i, indent)
            out.append(value)
        return out, i

    def documents(self):
        """The documents, split at `---` and `...` lines. Two things YAML
        allows and this reader once skipped are refused (review a7fa61ec,
        finding 1 and recommendation 2): content on the marker line itself
        (`--- {kind: …}` is a whole document to yaml.v3, and was dropped
        here), and lines left over after a document's root node (a
        `- x` after a root mapping, or a root at column 2 followed by one
        at column 0) — a reader that stops early answers for text it never
        read."""
        docs = []
        start = 0
        for k in range(self.n + 1):
            end = k == self.n
            line = "" if end else self.lines[k]
            marker = (not end) and (line in ("---", "...") or line.startswith("--- ") or line.startswith("... "))
            if marker and _strip_comment(line[3:]).strip():
                raise Unsupported(f"line {k + 1}: content on a document marker line ({line[:3]!r}) is not read")
            if end or marker:
                sub = Reader("\n".join(self.lines[start:k]))
                j = sub._next(0)
                if j < sub.n:
                    value, i = sub._node(j, sub._indent(j))
                    left = sub._next(i)
                    if left < sub.n:
                        raise Unsupported(f"line {start + left + 1}: content after the document's root node is not read")
                    docs.append(value)
                start = k + 1
        return docs


def read_yaml(text):
    return Reader(text).documents()


# ---- characters this reader does not read the way Talos does ------------
#
# Review 5ed37182, finding 1: Talos parses a patch with yaml.v3, whose
# scanner (a libyaml port) breaks lines at NEL (U+0085), LS (U+2028) and
# PS (U+2029) as well as CR and LF. This reader splits on "\n" alone, so a
# `# comment` ended by one of them hid the text after it — a whole second
# document, or a `cluster.apiServer.extraArgs` key — from every class
# check, and --only-read-classes answered 0. So, before any parse, of the
# declaration and of the live input alike: refuse those three, CR, TAB
# (YAML forbids it for indentation, and nothing here needs it), a
# byte-order mark anywhere but byte 0, and every other C0 or C1 control
# (and DEL) — anything but "\n". Ordinary non-ASCII stays: the shipped
# declarations carry em dashes in their comments.
BREAK_NAMES = {0x85: "NEL", 0x2028: "LS", 0x2029: "PS", 0x0D: "CR", 0x09: "TAB", 0xFEFF: "BOM"}


# ---- a declaration is in CANONICAL FORM -----------------------------------
#
# Review a7fa61ec, the third finding of one class: this hand reader and
# Talos's yaml.v3 read different STRUCTURE from one text (a `--- {…}` line
# was a whole document to Talos and nothing here; before it, a comment
# ended by LS; before that, a selector's text). Closing instances one at a
# time leaves the class open. So a DECLARATION is held to the one block
# form this reader re-emits from what it parsed: two-space indent,
# `key: value`, `- ` items (a mapping item's first key on the dash line),
# `|`/`|-` for a multi-line string, `---` alone between documents, no flow
# collections, tags, anchors, directives or `...`. A scalar is plain when
# plain text can only be read as itself, and single-quoted otherwise. The
# input — less comment-only lines, trailing ` # …` comments and blank
# lines — must be byte-identical to that re-emission, so any construct the
# reader might misread either appears verbatim in a form it provably
# parsed, or the declaration is refused (exit 2, naming the first line
# that differs). The live config, which Talos emits, is not held to this;
# Reader.documents() and _scalar refuse the shapes that could hide there.
_PLAIN = re.compile(r"[A-Za-z0-9_./+][A-Za-z0-9_./:+@ -]*")


class NotCanonical(Exception):
    pass


def _plain_ok(s):
    return (
        _PLAIN.fullmatch(s) is not None
        and s == s.rstrip()
        and ": " not in s
        and " #" not in s
        and not s.endswith(":")
        and s.lower() not in ("null", "~")
    )


def _inline(s):
    return s if _plain_ok(s) else "'" + s.replace("'", "''") + "'"


def _emit_key(k):
    k = str(k)
    if not _plain_ok(k):
        raise NotCanonical(f"the key {k!r} has no canonical (plain) form")
    return k


def _emit_block(prefix, s, ind, out):
    body = s[:-1] if s.endswith("\n") else s
    lines = body.split("\n")
    if s.endswith("\n\n") or not lines[0] or lines[0][0] == " " or any(l != l.rstrip() for l in lines):
        raise NotCanonical(f"the multi-line value under {prefix.strip()!r} has no canonical block form")
    out.append(f"{prefix} {'|' if s.endswith(chr(10)) else '|-'}")
    out.extend((" " * ind + l) if l else "" for l in lines)


def _emit_value(prefix, v, ind, out):
    if isinstance(v, dict):
        out.append(prefix)
        _emit_map(v, ind + 2, out)
    elif isinstance(v, list):
        out.append(prefix)
        _emit_seq(v, ind + 2, out)
    elif v is None:
        out.append(prefix)
    elif "\n" in str(v):
        _emit_block(prefix, str(v), ind + 2, out)
    else:
        out.append(f"{prefix} {_inline(str(v))}")


def _emit_map(d, ind, out):
    if not d:
        raise NotCanonical("an empty mapping has no block form")
    for k, v in d.items():
        _emit_value(" " * ind + _emit_key(k) + ":", v, ind, out)


def _emit_seq(items, ind, out):
    if not items:
        raise NotCanonical("an empty sequence has no block form")
    for it in items:
        dash = " " * ind + "- "
        if isinstance(it, dict) and it:
            for n, (k, v) in enumerate(it.items()):
                _emit_value((dash if n == 0 else " " * (ind + 2)) + _emit_key(k) + ":", v, ind + 2, out)
        elif isinstance(it, (dict, list)) or it is None or "\n" in str(it):
            raise NotCanonical("a sequence item that is empty, nested or multi-line has no canonical form")
        else:
            out.append(dash + _inline(str(it)))


def canonical_fault(text, docs):
    """None when `text` is in canonical form for `docs`; otherwise why,
    naming the first line that differs."""
    got = []
    for n, line in enumerate(text.split("\n"), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        stripped = _strip_comment(line)
        if stripped.strip():
            got.append((n, stripped))
    want = []
    try:
        for i, d in enumerate(docs):
            if i:
                want.append("---")
            if not isinstance(d, dict):
                raise NotCanonical("a document that is not a mapping has no canonical form")
            _emit_map(d, 0, want)
    except NotCanonical as e:
        return str(e)
    want = [l for l in want if l.strip()]
    for idx, (n, line) in enumerate(got):
        if idx >= len(want):
            return f"line {n} reads {line!r} after the canonical form has ended"
        if line != want[idx]:
            return f"line {n} reads {line!r}; its canonical form is {want[idx]!r}"
    if len(want) > len(got):
        return f"the canonical form continues with {want[len(got)]!r} after the file ends"
    return None


def read_raw(path):
    """The bytes of `path` (`-`: stdin) decoded as strict UTF-8 with NO
    newline translation: Python's text mode turns a lone CR into "\n"
    before any check could see it, and a byte that is not UTF-8 is an
    error (UNREADABLE), never a guess."""
    if path == "-":
        data = sys.stdin.buffer.read()
    else:
        with open(path, "rb") as f:
            data = f.read()
    return data.decode("utf-8")


def refuse_unread_breaks(text, what):
    """Exit 2 naming the first line and codepoint this reader would read
    differently from Talos; the text, less a leading byte-order mark,
    otherwise."""
    if text.startswith("﻿"):
        text = text[1:]
    for lineno, line in enumerate(text.split("\n"), 1):
        for ch in line:
            cp = ord(ch)
            if cp < 0x20 or 0x7F <= cp <= 0x9F or cp in (0x2028, 0x2029, 0xFEFF):
                name = BREAK_NAMES.get(cp, "control character")
                print(f"check-declared: {what} line {lineno} carries U+{cp:04X} ({name}), which Talos's YAML reads as a line break or control and this reader does not — refusing rather than read it differently", file=sys.stderr)
                sys.exit(2)
    return text


# ---- pick the machine config out of the live document ------------------

def is_machine(d):
    return isinstance(d, dict) and isinstance(d.get("machine"), dict)


def is_typed_doc(d):
    """A config document beside v1alpha1 (`kind:` at its top, no
    `machine:`, not a talosctl resource wrapper)."""
    return isinstance(d, dict) and "kind" in d and not is_machine(d) and "spec" not in d


def find_configs(docs):
    """(id, config, siblings) for every document carrying a machine
    config: a talosctl resource (`spec:` holding the config, as a mapping
    or as a block string) or a raw config with `machine:` at the top.
    `siblings` are the OTHER documents of that same config — a user
    volume rides beside v1alpha1 as a second document (backlog 52ea56ac)
    — read only from the config they belong to: talosctl prints the
    resource more than once, and gathering across copies would read
    every volume as DOUBLED."""
    found = []
    raw_siblings = [d for d in docs if is_typed_doc(d)]
    for d in docs:
        if not isinstance(d, dict):
            continue
        rid = ((d.get("metadata") or {}) if isinstance(d.get("metadata"), dict) else {}).get("id")
        spec = d.get("spec")
        if isinstance(spec, str):
            inner_docs = read_yaml(spec)
            siblings = [x for x in inner_docs if is_typed_doc(x)]
            for inner in inner_docs:
                if is_machine(inner):
                    found.append((rid, inner, siblings))
        elif isinstance(spec, dict) and is_machine(spec):
            found.append((rid, spec, []))
        elif is_machine(d):
            found.append((rid, d, raw_siblings))
    return found


def get(d, *path):
    for p in path:
        if not isinstance(d, dict):
            return None
        d = d.get(p)
    return d


# ---- the comparison ------------------------------------------------------

def show(v):
    """Any value as compact JSON — a scalar too, quoted and escaped, so a
    live value holding a newline cannot start a line of its own and forge
    a `check-declared: ` verdict (review 5ed37182, INFO)."""
    return json.dumps(norm(v), sort_keys=True, separators=(",", ":"))


def norm(v):
    """Scalars compare as text; None as empty."""
    if v is None:
        return ""
    if isinstance(v, (str, int, float, bool)):
        return str(v)
    if isinstance(v, list):
        return [norm(x) for x in v]
    if isinstance(v, dict):
        return {str(k): norm(x) for k, x in v.items()}
    return str(v)


findings = []  # (verdict, text)


def say(verdict, text):
    findings.append((verdict, text))
    print(f"{verdict:<10} {text}")


def keyed_list(cfg, path, key):
    items = get(cfg, *path)
    if items is None:
        return []
    if not isinstance(items, list):
        raise Unsupported(f"{'.'.join(path)} is not a list")
    out = []
    for it in items:
        if not isinstance(it, dict) or key not in it:
            raise Unsupported(f"an entry of {'.'.join(path)} has no {key!r}")
        out.append((str(it[key]), it))
    return out


def compare_list(name, dec, live, key, undeclared_when):
    dec_items = keyed_list(dec, name.split("."), key)
    live_items = keyed_list(live, name.split("."), key)
    seen = {}
    for k, _ in dec_items:
        if k in seen:
            print(f"check-declared: the declaration itself names {name}[{k}] twice — fix the declaration", file=sys.stderr)
            sys.exit(2)
        seen[k] = True
    for k, d in dec_items:
        copies = [it for kk, it in live_items if kk == k]
        label = f"{name}[{k}]"
        if not copies:
            say("ABSENT", f"{label}  declared, not live: {show(d)}")
            continue
        if len(copies) > 1:
            say("DOUBLED", f"{label}  live has {len(copies)} copies of a declared entry (declared once)")
        differing = [c for c in copies if norm(c) != norm(d)]
        if differing:
            say("DRIFT", f"{label}  declared {show(d)}  live {show(differing[0])}")
        elif len(copies) == 1:
            say("MATCH", label)
    for k, it in live_items:
        if k not in seen and undeclared_when(k):
            say("UNDECLARED", f"{name}[{k}]  live, no declaration names it: {show(it)}")


def compare_map(name, dec, live, undeclared_when):
    path = name.split(".")
    dec_map = get(dec, *path) or {}
    live_map = get(live, *path) or {}
    if not isinstance(dec_map, dict) or not isinstance(live_map, dict):
        raise Unsupported(f"{name} is not a mapping")
    for k, d in dec_map.items():
        label = f"{name}.{k}" if not isinstance(d, dict) else f"{name}[{k}]"
        if k not in live_map:
            say("ABSENT", f"{label}  declared, not live: {show(d)}")
        elif norm(live_map[k]) != norm(d):
            say("DRIFT", f"{label}  declared {show(d)}  live {show(live_map[k])}")
        else:
            say("MATCH", label)
    for k, v in live_map.items():
        if k not in dec_map and undeclared_when(k):
            label = f"{name}.{k}" if not isinstance(v, dict) else f"{name}[{k}]"
            say("UNDECLARED", f"{label}  live, no declaration names it: {show(v)}")


def compare_user_volumes(dec_docs, live_docs):
    """UserVolumeConfig documents keyed by `name`, in the same
    vocabulary as the machine entries. Every live user volume is in the
    class read for UNDECLARED."""
    def keyed(docs):
        return [(str(d.get("name")), d) for d in docs if d.get("kind") == "UserVolumeConfig"]
    dec_items = keyed(dec_docs)
    live_items = keyed(live_docs)
    names = [k for k, _ in dec_items]
    for k, d in dec_items:
        label = f"UserVolumeConfig[{k}]"
        copies = [it for kk, it in live_items if kk == k]
        if not copies:
            say("ABSENT", f"{label}  declared, not live: {show(d)}")
            continue
        if len(copies) > 1:
            say("DOUBLED", f"{label}  live has {len(copies)} copies of a declared document (declared once)")
        differing = [c for c in copies if norm(c) != norm(d)]
        if differing:
            say("DRIFT", f"{label}  declared {show(d)}  live {show(differing[0])}")
        elif len(copies) == 1:
            say("MATCH", label)
    for k, it in live_items:
        if k not in names:
            say("UNDECLARED", f"UserVolumeConfig[{k}]  live, no declaration names it: {show(it)}")


try:
    dec_text = refuse_unread_breaks(read_raw(declared_path), f"the declaration {declared_path}")
    dec_docs = read_yaml(dec_text)
except Unsupported as e:
    print(f"check-declared: cannot read {declared_path}: {e}", file=sys.stderr)
    sys.exit(2)
# ONE machine fragment, plus any number of UserVolumeConfig documents
# (w-1's gate volume, backlog 52ea56ac). Any other document is REFUSED
# by kind: a comparator that skipped half its declaration would report
# a clean node it never compared.
dec_machine = [d for d in dec_docs if is_machine(d)]
if len(dec_machine) != 1:
    print(f"check-declared: {declared_path} is not one machine-config fragment (`machine:` at the top)", file=sys.stderr)
    sys.exit(2)
dec = dec_machine[0]
dec_volumes = []
for d in dec_docs:
    if d is dec:
        continue
    if not (isinstance(d, dict) and d.get("kind") == "UserVolumeConfig" and d.get("name")):
        kind = d.get("kind") if isinstance(d, dict) else type(d).__name__
        print(f"check-declared: {declared_path} carries a {kind or 'kind-less'} document; this check compares the machine fragment and named UserVolumeConfig documents only", file=sys.stderr)
        sys.exit(2)
    dec_volumes.append(d)
dup = sorted({str(d["name"]) for d in dec_volumes if [x["name"] for x in dec_volumes].count(d["name"]) > 1})
if dup:
    print(f"check-declared: the declaration itself names UserVolumeConfig[{dup[0]}] twice — fix the declaration", file=sys.stderr)
    sys.exit(2)
_fault = canonical_fault(dec_text, dec_docs)
if _fault:
    print(f"check-declared: {declared_path} is not in canonical form — {_fault}. A declaration is held to the one block form this reader re-emits from what it parsed, so nothing it might misread reaches a node (review a7fa61ec)", file=sys.stderr)
    sys.exit(2)

# The classes this check reads — the four the comparison below walks, and
# the only entries whose presence live it can prove.
READ_CLASSES = (
    "machine.files",
    "machine.kubelet.extraMounts",
    "machine.kubelet.extraConfig",
    "machine.registries.mirrors",
)
FILES, MOUNTS, EXTRA_CONFIG, MIRRORS = READ_CLASSES


def unread_paths(d, path=()):
    """Every key path of the declaration that is neither a read class nor
    a mapping on the way to one."""
    out = []
    wanted = [tuple(c.split(".")) for c in READ_CLASSES]
    for k, v in d.items():
        p = path + (str(k),)
        if p in wanted:
            continue
        if isinstance(v, dict) and any(w[: len(p)] == p for w in wanted):
            out.extend(unread_paths(v, p))
        else:
            out.append(".".join(p))
    return out


# The kubelet keys a declaration may carry under extraConfig: the ones the
# tree declares today (cp-1..3's image-GC pair). extraConfig passes any
# KubeletConfiguration key straight to the kubelet — authentication,
# authorization mode, unsafe sysctls — which on a control plane would
# open the cluster, so node-converge applies only these (review 9cb9af67
# finding 10, backlog 9d56c616). A new key is a reviewed edit here.
EXTRA_CONFIG_KEYS = ("imageGCHighThresholdPercent", "imageGCLowThresholdPercent")


def unmanaged_extra_config(d):
    ec = get(d, *EXTRA_CONFIG.split("."))
    if ec is None:
        return []
    if not isinstance(ec, dict):
        # A sequence or scalar where the kubelet's keys belong is no key
        # this list admits (review a7fa61ec, recommendation 2).
        return [f"{EXTRA_CONFIG} (a {type(ec).__name__}, not a mapping of kubelet keys)"]
    return [f"{EXTRA_CONFIG}.{k}" for k in ec if str(k) not in EXTRA_CONFIG_KEYS]


# THE SELECTOR GRAMMAR a converge may apply — an ALLOWLIST (review
# be5ba8f7, finding 1, the coordinator's call). A text search for a
# `!system_disk` term was bypassed five ways: inside a negated group, a
# compared group, a CEL comment, a string literal and a list. So nothing
# is searched for: the whole match must BE `term (&& term)*`, each term
# exactly `!system_disk` or `disk.<ident> <op> <literal>`, with exactly
# one `!system_disk`. A literal is a number (optional `u`, optional
# `* GB|MB|TB`) or a double-quoted string holding no quote, backslash or
# `&` — so no `&&` can hide inside one and the split below is the
# expression's own structure. `(`, `)`, `[`, `]`, `//`, `/*` and any
# newline are refused wherever they appear.
SELECTOR_FORBIDDEN = ("(", ")", "[", "]", "//", "/*", "\n", "\r")
SELECTOR_TERM = re.compile(
    r'disk\.[A-Za-z_][A-Za-z0-9_]*\s*(==|!=|<=|>=|<|>)\s*'
    r'([0-9]+(\.[0-9]+)?u?(\s*\*\s*(GB|MB|TB))?|"[^"\\&]*")'
)


def selector_fault(m):
    """Why a diskSelector match is not in the grammar above, or None."""
    for f in SELECTOR_FORBIDDEN:
        if f in m:
            return f"it contains {f!r}, which the selector grammar refuses"
    terms = [t.strip() for t in m.split("&&")]
    for t in terms:
        if t != "!system_disk" and not SELECTOR_TERM.fullmatch(t):
            return f"its term {t!r} is neither !system_disk nor disk.<field> <op> <number or string>"
    n = terms.count("!system_disk")
    if n != 1:
        return f"it carries !system_disk {n} times, not once"
    return None


def unsafe_volumes(vols):
    """(label, why) for every declared UserVolumeConfig whose disk
    selector cannot be shown to exclude the system disk. The check reads
    a user volume back by name (compare_user_volumes), so a converge may
    apply one — but Talos provisions it on whatever disk the selector
    matches, and one landing on the install disk would take EPHEMERAL
    with it (w-1's Longhorn replicas, dev /work among them). So its
    provisioning.diskSelector.match must be in SELECTOR_TERM's grammar
    above: a conjunction of plain comparisons with exactly one
    `!system_disk` term (the coordinator's calls on the rebase over disk
    car 1 and on review be5ba8f7, 2026-09-30)."""
    out = []
    for d in vols:
        label = f"UserVolumeConfig[{d.get('name')}]"
        m = get(d, "provisioning", "diskSelector", "match")
        if not isinstance(m, str) or not m.strip():
            out.append((label, "names no provisioning.diskSelector.match, so nothing shows it excludes the system disk"))
            continue
        fault = selector_fault(m)
        if fault:
            out.append((label, f"its diskSelector {m!r} cannot be shown to exclude the system disk: {fault}"))
    return out


if mode == "classes":
    for p in unmanaged_extra_config(dec):
        print(f"UNREAD     {p}  declared, outside the kubelet keys a declaration may carry ({', '.join(EXTRA_CONFIG_KEYS)})")
    for p in unread_paths(dec):
        print(f"UNREAD     {p}  declared, in no class this check reads ({', '.join(READ_CLASSES)})")
    for label, why in unsafe_volumes(dec_volumes):
        print(f"REFUSED    {label}  {why}")
    unread = unmanaged_extra_config(dec) + unread_paths(dec) + [label for label, _ in unsafe_volumes(dec_volumes)]
    if unread:
        print(f"check-declared: {node}: {len(unread)} declared entr{'y' if len(unread) == 1 else 'ies'} a converge may not apply")
        sys.exit(3)
    print(f"check-declared: {node}: every declared entry is in a class this check reads")
    sys.exit(0)

try:
    configs = find_configs(read_yaml(refuse_unread_breaks(read_raw(live_path), "the live config")))
except Unsupported as e:
    print(f"check-declared: cannot read the live config: {e}", file=sys.stderr)
    sys.exit(2)
except OSError as e:
    print(f"check-declared: cannot read the live config: {e}", file=sys.stderr)
    sys.exit(2)
if not configs:
    print("check-declared: no machine config in the input (expected `talosctl get machineconfig -o yaml`, or a config with `machine:` at the top)", file=sys.stderr)
    sys.exit(2)
chosen = next((c for c in configs if c[0] == "v1alpha1"), configs[0])
rid, live, live_siblings = chosen
hostname = get(live, "machine", "network", "hostname")
if hostname is not None and str(hostname) != node:
    print(f"check-declared: the live config says hostname {hostname!r}; asked to check {node!r} — refusing to compare another node's config", file=sys.stderr)
    sys.exit(2)
print(f"read       machineconfig id={rid or '(raw)'} ({len(configs)} config document(s) in input; hostname {hostname or '(unset)'}) against {declared_path}")
# THE NODE IS UNVERIFIED without a hostname (review a79746c6, finding 7).
# The stored config (`talosctl read /system/state/config.yaml`) carries
# no `node:` field, and a declaration names neither address nor machine
# type, so the hostname is the only identity either side states. When
# the live config sets none (a DHCP or HostnameConfig hostname), the
# comparison still runs, but the check SAYS it could not tell whose
# config it read, on its own line and again in the summary, rather
# than letting a wrong node's MATCH read as this one's.
unverified = hostname is None
if unverified:
    print(f"UNVERIFIED the live config names no machine.network.hostname — nothing here proves it is {node}'s; confirm the -n address before trusting any verdict below")

try:
    compare_list(FILES, dec, live, "path", lambda p: p.startswith("/var/local/"))
    compare_list(MOUNTS, dec, live, "destination", lambda _d: True)
    compare_map(EXTRA_CONFIG, dec, live, lambda k: str(k).startswith("imageGC"))
    compare_map(MIRRORS, dec, live, lambda _h: True)
    compare_user_volumes(dec_volumes, live_siblings)
except Unsupported as e:
    print(f"check-declared: cannot compare: {e}", file=sys.stderr)
    sys.exit(2)

counts = {v: 0 for v in ("MATCH", "DRIFT", "ABSENT", "DOUBLED", "UNDECLARED")}
for v, _ in findings:
    counts[v] += 1
hard = counts["DRIFT"] + counts["ABSENT"] + counts["DOUBLED"]
verdict = "every declared entry matches" if hard == 0 else f"{hard} finding(s) against the declaration"
if unverified:
    verdict += f" (node identity UNVERIFIED: no hostname in the live config)"
print(
    f"check-declared: {node}: {counts['MATCH']} match, {counts['DRIFT']} drift, "
    f"{counts['ABSENT']} absent, {counts['DOUBLED']} doubled, {counts['UNDECLARED']} undeclared — {verdict}"
)
sys.exit(1 if hard else 0)
PY

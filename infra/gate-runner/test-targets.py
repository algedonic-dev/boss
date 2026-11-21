#!/usr/bin/env python3
"""Read grouped integration membership from the Rust module declarations Cargo compiles."""
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib


def membership(root):
    crate = root / "crates/core/boss-testing"
    manifest = crate / "Cargo.toml"
    if not manifest.exists():
        return {}
    doc = tomllib.loads(manifest.read_text())
    if doc.get("package", {}).get("autotests", True):
        return {}
    if list((crate / "tests").glob("*/main.rs")):
        raise ValueError("directory integration targets need an explicit packaging decision")
    owners = {}
    groups = {}
    # The marker is defined by the gate, not copied into this reader.
    # Use its own ERE through grep, including any future POSIX classes.
    gate = Path(__file__).resolve().parents[1] / "gate.sh"
    prefix = "TREE_WIDE_FILE_PATTERN='"
    patterns = [line[len(prefix):-1] for line in gate.read_text().splitlines()
                if line.startswith(prefix) and line.endswith("'")]
    if len(patterns) != 1:
        raise ValueError("unread canonical tree-wide file marker")
    sources = sorted((crate / "tests").glob("*.rs"))
    pins = set()
    if sources:
        result = subprocess.run(["grep", "-lE", "--", patterns[0], *map(str, sources)], capture_output=True, text=True)
        if result.returncode not in (0, 1):
            raise ValueError("cannot read declared integration pins: " + result.stderr)
        pins = set(result.stdout.splitlines())
    group_paths = set()
    for target in doc.get("test", []):
        if set(target) - {"name", "path", "required-features"}:
            raise ValueError("unread integration target options: " + repr(target))
        name = target["name"]
        path = crate / target.get("path", f"tests/{name}.rs")
        if path.parent == crate / "tests":
            members = [path]
        else:
            if path.parent != crate / "tests/groups":
                raise ValueError(f"unsupported integration target path: {path}")
            if path.is_symlink():
                raise ValueError(f"linked integration group: {path}")
            group_paths.add(path)
            text = path.read_text()
            declarations = re.compile(r'^#\[path = "\.\./([A-Za-z0-9_-]+)\.rs"\]\nmod ([A-Za-z0-9_]+);$', re.M)
            pairs = declarations.findall(text)
            rest = declarations.sub("", text)
            if any(line.strip() and not line.startswith("//") for line in rest.splitlines()):
                raise ValueError(f"unread integration membership in {path}")
            if not pairs:
                raise ValueError(f"empty integration group: {path}")
            for source, module in pairs:
                if module != source.replace("-", "_"):
                    raise ValueError(f"module identity changed: {source} -> {module}")
            # Rustfmt orders these declarations by module identity. The
            # equality above already binds each identity to its source,
            # normalizing source hyphens to Rust identifier underscores.
            if [module for _, module in pairs] != sorted(module for _, module in pairs):
                raise ValueError(f"integration group members must be sorted alphabetically by Rust module identity: {path}")
            members = [crate / "tests" / (source + ".rs") for source, _ in pairs]
            declared = [str(p) in pins for p in members]
            if any(declared) and not all(declared):
                raise ValueError(f"pin group also runs undeclared tests: {path}")
            groups[name] = [p.stem for p in members]
        for member in members:
            if member.is_symlink() or not member.is_file() or member.stem in owners:
                raise ValueError(f"missing or multiply owned integration source: {member}")
            owners[member.stem] = name
    files = {p.stem for p in (crate / "tests").glob("*.rs")}
    if files != set(owners):
        raise ValueError(f"integration source coverage differs: missing={sorted(files-set(owners))}, extra={sorted(set(owners)-files)}; add each missing path/mod pair in alphabetical Rust module order to tests/groups/<area>.rs, or declare a dedicated [[test]] target when it owns process state")
    if group_paths != set((crate / "tests/groups").glob("*.rs")):
        raise ValueError("integration group file has no Cargo target")
    return {"owners": owners, "groups": groups}


def main():
    mapped = membership(Path.cwd())
    if sys.argv[1:] == ["--json"]:
        print(json.dumps(mapped, sort_keys=True))
        return
    for line in sys.stdin:
        if not line.strip():
            continue
        crate, selector, name = line.split()
        if crate == "boss-testing" and selector.startswith("test:") and mapped:
            selector = "test:" + mapped["owners"][name]
        print(crate, selector, name)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError) as error:
        print(f"integration target membership refused: {error}", file=sys.stderr)
        raise SystemExit(1) from None

#!/usr/bin/env python3
"""Render the gate's repository URL from the estate, never a second default."""

import pathlib
import sys
import tomllib


def repository(text: str) -> str:
    data = tomllib.loads(text)
    forge = data.get("forge_url")
    path = data.get("forge_repository")
    if not isinstance(forge, str) or not forge.strip():
        raise ValueError("forge_url is missing or unreadable")
    forge = forge.strip()
    if any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in forge):
        raise ValueError("forge_url contains whitespace or control characters")
    if not isinstance(path, str) or any(
        not part or part in (".", "..")
        or not all(c.isascii() and (c.isalnum() or c in "._-") for c in part)
        for part in path.split("/")
    ):
        raise ValueError("forge_repository must be a relative repository path")
    return forge.rstrip("/") + "/" + path


if __name__ == "__main__":
    try:
        if len(sys.argv) != 2:
            raise ValueError("usage: forge-repository.py <estate.toml>")
        print(repository(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")))
    except (OSError, ValueError) as error:
        print(f"gate repository: {error}", file=sys.stderr)
        sys.exit(78)

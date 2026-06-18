#!/usr/bin/env bash
# apply-schema.sh — concatenate the per-module schema files (in manifest
# order) to stdout, for piping into psql. Replaces the single monolithic
# schema.sql: each Tier-2 module owns its own schema/NN-<module>.sql, so a
# module can be dropped (or skipped via --without) without touching the
# others — the headline capability is bootstrapping a ledger-less BOSS.
#
#   ./apply-schema.sh                    # full schema (all modules)
#   ./apply-schema.sh --without ledger   # omit files whose name matches
#   ./apply-schema.sh --without ledger --without scheduling
#
# Callers pipe to their own psql, e.g.:
#   ./apply-schema.sh | sudo -u postgres psql -d boss
set -euo pipefail

DIR="$(cd "$(dirname "$0")/schema" && pwd)"
MANIFEST="$DIR/manifest.txt"

WITHOUT=()
while [ $# -gt 0 ]; do
    case "$1" in
        --without) shift; WITHOUT+=("${1:?--without needs a module name}"); shift ;;
        *) echo "apply-schema.sh: unknown arg: $1" >&2; exit 2 ;;
    esac
done

if [ ! -f "$MANIFEST" ]; then
    echo "apply-schema.sh: manifest not found at $MANIFEST" >&2
    exit 1
fi

while IFS= read -r f; do
    [ -z "$f" ] && continue
    skip=
    for w in "${WITHOUT[@]}"; do
        case "$f" in *"$w"*) skip=1; break ;; esac
    done
    [ -n "$skip" ] && continue
    printf -- '-- >>> %s\n' "$f"
    cat "$DIR/$f"
    printf '\n'
done < "$MANIFEST"

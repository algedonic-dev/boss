#!/usr/bin/env bash
# Run TLC against a spec under docs/formal/.
#
# Usage:
#   ./infra/tla/run-tlc.sh LedgerPeriodLock
#   ./infra/tla/run-tlc.sh LedgerPeriodLock --workers 4 --depth 50
#
# The first positional arg is the spec name (no .tla extension).
# Extra args pass through to tla2tools.jar's TLC.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <spec-name> [extra TLC args]" >&2
    echo "  e.g. $0 LedgerPeriodLock" >&2
    exit 1
fi

SPEC="$1"
shift

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TLA_DIR="${REPO_ROOT}/docs/formal"
JAR="${REPO_ROOT}/infra/tla/tla2tools.jar"

if [[ ! -f "${JAR}" ]]; then
    echo "tla2tools.jar not found at ${JAR}" >&2
    echo "download with:" >&2
    echo "  curl -sL -o ${JAR} https://github.com/tlaplus/tlaplus/releases/latest/download/tla2tools.jar" >&2
    exit 1
fi

if [[ ! -f "${TLA_DIR}/${SPEC}.tla" ]]; then
    echo "spec not found: ${TLA_DIR}/${SPEC}.tla" >&2
    exit 1
fi

if [[ ! -f "${TLA_DIR}/${SPEC}.cfg" ]]; then
    echo "config not found: ${TLA_DIR}/${SPEC}.cfg" >&2
    exit 1
fi

# Run TLC from the spec directory so it picks up sibling modules
# without an explicit -searchpath. `-deadlock` because our specs
# allow stuttering as a no-op terminal state (no work to do).
cd "${TLA_DIR}"
exec java -jar "${JAR}" \
    -config "${SPEC}.cfg" \
    -deadlock \
    "$@" \
    "${SPEC}.tla"

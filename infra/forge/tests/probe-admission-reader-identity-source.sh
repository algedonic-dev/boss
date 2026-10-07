#!/usr/bin/env bash
# Credential-free source controls only; never the installed identity read.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python3 -B "$HERE/admission-identity-test.py"
printf 'admission-reader-identity-source: PASS\n'

#!/usr/bin/env bash
# The fingerprint of the Rust sources the release binaries are built
# from. Prints one short hex string; identical content always yields an
# identical value, regardless of file timestamps.
#
# WHY THIS EXISTS
# ---------------
# The deploy's freshness guard used to compare binary mtime against the
# newest source mtime. That is wrong in a way that only shows up once
# you work on branches: `git checkout`, `git rebase` and `git
# cherry-pick` all REWRITE files, so every source file they touch gets
# a current mtime even when its content is byte-identical.
#
# On 2026-08-08 a stacked-PR session did exactly that. `git status` was
# clean, no crate had changed, the binaries were built from precisely
# the tree on disk — and the deploy refused with "STALE", naming eight
# binaries and demanding a 50-minute rebuild that would have produced
# identical output. A guard that blocks correct deploys after routine
# git work is a guard people learn to bypass.
#
# This is the same lesson as `932cab24` ("hash the deployed binary;
# mtime lied"), which removed a `touch` from the build script for the
# mirror-image reason: there, mtime made a stale binary look fresh;
# here, it made a fresh binary look stale. Content is the only thing
# that answers the question.
#
# WHAT IS HASHED
# --------------
# `git ls-files -s` prints mode + BLOB SHA + path for tracked files —
# already content-addressed, so committed state costs nothing to hash.
# `git diff` over the same paths covers uncommitted edits. Together
# they describe the working tree's content exactly, and mtime never
# enters the calculation.
#
# Scope is `crates/` plus the lockfile: the inputs that decide what a
# release binary contains. A web-only or docs-only commit deliberately
# does NOT change the fingerprint, so it does not demand a pointless
# Rust rebuild.
#
# Not in a git checkout: prints nothing and exits 0. Callers treat an
# empty fingerprint as "cannot tell", and must not block on it — a
# tarball deployment has no git metadata and is not thereby stale.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

git rev-parse --is-inside-work-tree >/dev/null 2>&1 || exit 0

{
    git ls-files -s -- crates Cargo.lock Cargo.toml
    git diff -- crates Cargo.lock Cargo.toml
} | sha256sum | cut -c1-16

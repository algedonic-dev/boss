<!--
Thanks for contributing to BOSS. Before you submit, please skim
CONTRIBUTING.md — the load-bearing rules are there. The shape
of a useful PR is small + tested + commit-message-y in the body.
-->

## Summary

<!-- 1–3 sentences describing what this PR changes and why. -->

## What changed

<!-- Bulleted list of the concrete changes. Group by crate /
file area. -->

## Tests

<!-- What tests cover this change? BOSS is TDD-by-default — if
this is a bug fix, the test that reproduces the bug should be
in the same commit. If it's a new feature, the test that
exercises the new behavior should land alongside the
implementation. -->

- [ ] `cargo fmt` clean
- [ ] `cargo clippy -- -D warnings` clean
- [ ] `cargo test -p <affected-crate>` green
- [ ] `bun run typecheck` clean (frontend changes)
- [ ] Relevant `apps/web/tests/smoke/*.spec.ts` green
      (frontend changes)

## Extensibility check

<!-- Per CONTRIBUTING.md and docs/design/extending-boss.md, new
work is supposed to ship as registry rows / plugin bundles /
class entries before it touches core code. If this PR adds core
code, briefly justify why a registry / plugin / class wouldn't
have done the job. Skip this section for pure bug fixes /
refactors / docs. -->

## Related issues / docs

<!-- Closes #N, refs #M, links to design docs that prompted
this change. -->

# Changelog

All notable changes to BOSS are recorded here. Done work lives here;
forward-looking work lives in [TODO.md](TODO.md).

This is a **preliminary release** — APIs, schemas, and registry
contracts will move before the first non-preliminary tag, so changes
are summarized at the release level rather than itemized per commit.
The format loosely follows [Keep a Changelog](https://keepachangelog.com/);
versioning is not yet semver-stable.

## [Unreleased]

_Nothing yet._

## Preliminary release

The first public cut. Proves the shape of the idea rather than
shipping a stable product. Highlights of what is present:

- **Four primitives** — Subjects, Jobs, Steps, and an immutable
  Event log, with the JobKind / StepType / Class registries that
  let new work types and taxonomies land as data rather than code.
- **Event-sourced core** — `audit_log` as the system of record;
  projections and rebuilders are pure functions of it, guarded by
  the five-property correctness protocol (provenance, conservation,
  closure, idempotence, determinism).
- **Hexagonal service map** across the core, company-modeling, and
  tenant tiers, with `boss-gateway` as the single auth boundary and
  `boss-dispatcher` running data-driven step side-effects.
- **Two worked-example tenants** — the Algedonic Ales brewery
  (public demo) and the used-device-shop — both seeded with
  synthetic, anonymized data.
- **Quickstart paths** — Docker compose and a bare-metal
  `quickstart.sh`, plus CI, Dependabot, OpenSSF Scorecard, and SLSA
  build-provenance for release binaries.

See [docs/architecture-decisions.md](docs/architecture-decisions.md)
for the consolidated design-decision record.

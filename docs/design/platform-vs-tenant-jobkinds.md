# Platform vs. tenant JobKinds — separation principle

**Status**: load-bearing principle. The rule for what BOSS core
ships baked-in vs. what every tenant authors as data.

## The principle

BOSS core ships **the alphabet, not the programs**:

- **Alphabet** (platform code, every deployment) — the StepType
  registry (`crates/core/boss-jobs/src/step_registry.rs`), the
  SubjectKind registry (the `subject_kinds` table, whose system-owned
  rows `infra/postgres/schema/01-registries.sql` seeds: the five roots
  `person` / `location` / `object` / `intangible` / `calendar`, their
  specializations `account` / `customer` / `employee` / `vendor` /
  `campaign` / `asset` / `product` / `purchase_order`, and the
  `custom` escape hatch), the side-effect handler registry
  (`boss-dispatcher`, keyed by `step.done.<kind>`), the Class registry
  mechanism, the Job and Step primitives.
- **Programs** (tenant data, per `examples/<tenant>/seeds/`) —
  JobKinds. `morning-brew` is a program written in the alphabet
  for the brewery; `refurb-used` is a program for the
  used-device-shop. Both are tenant-owned. The platform should
  not ship either.

Concretely: the only platform-shipped JobKinds are kinds that are
**load-bearing infrastructure**, where removing them breaks BOSS
itself. `platform_kinds()` in
`crates/core/boss-jobs/src/registry.rs` ships exactly two: the
meta-kind `job-kind-design` (used to author every other JobKind)
and `design-doc-review` (which drives a design doc under
`docs/design/` through its open-question review and decision
capture). Anything else is a tenant program — even when it looks
generic.

Beware the "looks generic" trap. `onboarding` looks like every
company has it, so it's tempting to ship as a platform seed. But
every company's onboarding step graph is different (a brewery's
tier 1 is "first-week training," the used-device-shop's is
"RMA-handling certification"). Shipping *one* canonical onboarding
flow forces every tenant to fork the platform to get their actual
flow. The right move is: ship zero canonical onboarding flows, let
each tenant author their own.

## Why this matters

Three reasons, in load-bearing order:

1. **OSS launch story.** A new operator clones BOSS and authors a
   tenant. If core Rust shipped `sale` / `demo` / `field-service` /
   `marketing-motion` / `vendor-negotiation` baked in, the new
   operator would either adopt those (wrong shape for their
   business) or fork core to replace them. Both paths fail the
   "model your company on BOSS" pitch. With the principle
   enforced, the new tenant has zero inherited assumptions and
   authors whatever they need from the alphabet.
2. **Bookkeeping.** Tenant code in platform code creates upgrade
   friction. Every BOSS upgrade ships every tenant's "starter"
   JobKinds, including ones the tenant overrode locally. Reconcile
   gymnastics ensue. The platform-vs-tenant split makes upgrades
   touch only platform code; tenant data files are theirs alone.
3. **Conceptual hygiene.** The framing in `CLAUDE.md` is "BOSS is
   software for modeling systems as state machines." The state
   machine has an alphabet and programs. Conflating the two
   would muddy what BOSS *is* — making it look like a bundle of
   business workflows rather than a generic modeling toolkit
   that ships with worked examples.

## Today's split

The only platform-shipped JobKinds are `job-kind-design` (the
meta-kind for authoring every other JobKind) and `design-doc-review`
(the meta-kind for reviewing a design doc), both seeded via
`platform_kinds()` in `crates/core/boss-jobs/src/registry.rs`. Both
are infrastructure for evolving BOSS itself, not business workflows.

Tenant JobKinds live in tenant data:

- `examples/brewery/seeds/job_kinds.toml` — Algedonic Ales
  (`morning-brew`, `wholesale-keg-order`, `seasonal-release`,
  `equipment-preventive-maintenance`, `brewery-hire`, `payroll-run`,
  `sales-tax-filing`, …).
- `examples/used-device-shop/seeds/job_kinds.toml` — used-device-shop
  (`device-intake`, `refurb-used`, `field-service`, `sale`,
  `support-incident`, `vendor-negotiation`, `marketing-motion`,
  `hiring`, `offboarding`, …).

If a future tenant wants `payroll-run` / `sales-tax-filing`
(which the brewery currently owns), the cleaner shape is to lift
them into a third "shared starter pack" tenant directory that
other tenants explicitly opt into by copying the rows. That
avoids re-creating the platform-shipped-tenant-code anti-pattern.

## Where new work goes — the rule of thumb

A new contributor adding a JobKind picks one of two slots:

- Is the JobKind something every BOSS deployment must have to
  function as BOSS (e.g., the meta-kind for authoring other
  JobKinds)? → `registry::platform_kinds()` in
  `crates/core/boss-jobs/src/registry.rs`.
- Is it a workflow specific to a business model (sales, refurb,
  brewing, support, anything domain-shaped)? → that tenant's
  `examples/<tenant>/seeds/job_kinds.toml`.
- Is it generic-shaped but the step graph differs per business?
  → still tenant. Author the closest match in the tenant TOML;
  let other tenants copy + adapt.

Core Rust never carries a tenant-shaped JobKind. Tenant kinds live
in TOML; the only Rust slot is `platform_kinds()`, reserved for
kinds truly load-bearing for BOSS itself.

# Platform vs. tenant Workflows — separation principle

**Status**: load-bearing principle. The rule for what the platform
supplies and what each company authors as its own protocol data.

## The principle

BOSS core ships **the alphabet, not the programs**:

- **Alphabet** (platform code, every deployment) — the StepType
  registry (`crates/core/boss-jobs/src/step_registry.rs`), the
  SubjectKind registry (the `subject_kinds` table, whose system-owned
  rows `infra/postgres/schema/01-registries.sql` seeds: the five roots
  `person` / `location` / `object` / `intangible` / `calendar`, their
  specializations `account` / `customer` / `employee` / `vendor` /
  `campaign` / `asset` / `product` / `purchase_order` / `shipment` /
  `vendor-invoice` / `marketing-asset` / `invoice` / `message`, and the
  `custom` escape hatch), the side-effect handler registry
  (`boss-dispatcher`, keyed by `step.done.<kind>`), the Class registry
  mechanism, the Job and Step primitives.
- **Programs** (a tenant's own `seeds/workflows.toml`) —
  Workflows. `morning-brew` is a program written in the alphabet
  for the brewery. It is tenant-owned; its meaning belongs in the
  tenant registry rather than the platform's infrastructure bundle.

Concretely: the only platform-shipped Workflows are kinds that are
**load-bearing infrastructure**, where removing them breaks BOSS
itself. Their definitions are registry data under
[`infra/platform/workflows/`](../../infra/platform/workflows/).
They include the authoring and design-review protocols; a design's
structured questions and resolutions live on its packet. The
platform roster comes from that bundle, not a second list in prose.

Beware the "looks generic" trap. `onboarding` looks like every
company has it, so it's tempting to ship as a platform seed. But
every company's onboarding step graph is different (a brewery's
tier 1 is "first-week training," another company's is
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
   Workflows, including ones the tenant overrode locally. Reconcile
   gymnastics ensue. The platform-vs-tenant split makes upgrades
   touch only platform code; tenant data files are theirs alone.
3. **Conceptual hygiene.** The framing in `CLAUDE.md` is "BOSS is
   software for modeling systems as state machines." The state
   machine has an alphabet and programs. Conflating the two
   would muddy what BOSS *is* — making it look like a bundle of
   business workflows rather than a generic modeling toolkit
   that ships with worked examples.

## Today's split

Platform Workflows are authored one protocol per TOML file under
[`infra/platform/workflows/`](../../infra/platform/workflows/).
That directory defines the roster; its README describes the loader,
seed and publication contract. `platform_workflows()` in
`crates/core/boss-jobs/src/registry.rs` is intentionally empty: no
platform protocol is authored as a Rust literal.

Tenant Workflows live in tenant data:

- `examples/brewery/seeds/workflows.toml` — Algedonic Ales
  (`morning-brew`, `wholesale-keg-order`, `seasonal-release`,
  `equipment-preventive-maintenance`, `brewery-hire`, `payroll-run`,
  `sales-tax-filing`, …).

If a future tenant wants `payroll-run` / `sales-tax-filing`
(which the brewery currently owns), the cleaner shape is to lift
them into a "shared starter pack" tenant directory that
other tenants explicitly opt into by copying the rows. That
avoids re-creating the platform-shipped-tenant-code anti-pattern.

## Where new work goes — the rule of thumb

A new contributor adding a Workflow chooses its authoring home:

- Is the Workflow something every BOSS deployment must have to
  function as BOSS (e.g., the meta-kind for authoring other
  Workflows)? → one `<kind>.toml` file under
  `infra/platform/workflows/`, using that bundle's loader and
  publication contract.
- Is it a workflow specific to a business model (sales, refurb,
  brewing, support, anything domain-shaped)? → that tenant's
  own `seeds/workflows.toml`, following the
  [tenant contract](../tenant-contract.md). The brewery bundle above
  is the shipped example, not the home for a company's private model.
- Is it generic-shaped but the step graph differs per business?
  → still tenant. Author the closest match in the tenant TOML;
  let other tenants copy + adapt.

Both authoring homes carry protocol data, not new branches in core
Rust. Platform files seed missing kinds; publishing a new version uses
`boss workflow publish <kind> infra/platform/workflows/<kind>.toml`
under the existing workflow publication authority. In-flight packets
keep the version they were admitted under. The bundle README and
tenant contract define their respective validation and publication
doors; changing a protocol does not add a Rust authoring slot.

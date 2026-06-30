# TODO

BOSS's public roadmap — open, forward-looking work. **Done work lives
in [CHANGELOG.md](CHANGELOG.md)** — don't restate it here.

This is a **preliminary release**: the core shape is in place — the
four primitives (Subjects, Jobs, Steps, Events), the event-sourced
audit log, and the registry-driven extensibility model (JobKinds,
StepTypes, Classes, StepPlugins). The buckets below are what's
deliberately *not* done yet. Treat [CHANGELOG.md](CHANGELOG.md) and
[docs/architecture-decisions.md](docs/architecture-decisions.md) as the
source of truth for what already exists, and verify against the working
tree before acting on a detailed item.

## Where to read first (orientation for a new contributor)

- [`docs/architecture-decisions.md`](docs/architecture-decisions.md) — the consolidated decision record (JobKind v2 shape, dispatcher-as-event-router, step types as property bundles, all of it)
- [`docs/design/extending-boss.md`](docs/design/extending-boss.md) — the extensibility ladder (JobKinds, StepTypes, StepPlugins)
- [`docs/design/class-registry.md`](docs/design/class-registry.md) — registry-backed taxonomy pattern
- [`docs/design/correctness-protocol.md`](docs/design/correctness-protocol.md) — five-property invariant (provenance, conservation, closure, idempotence, determinism)
- [`docs/design/human-powered-state-machine.md`](docs/design/human-powered-state-machine.md) — reading frame for what BOSS *is*

The three buckets below sort by how soon a contributor should
reach for them, not by importance: **open questions** → items
blocked on a decision before they land; **post-release strategic**
→ bigger shapes deferred until a real tenant steers the priority;
**post-release polish** → automation + UX nice-to-haves.

---

## Open questions — back-compat cleanup

The pre-release back-compat audit found vestigial code preserving
compatibility with implementations that no longer exist after the
public cut. Each removal needs a small decision before landing.

- [ ] **BC2: `StepStatus` `parse_step_status` fallback.** The serde
      `alias = "pending"/"done"/"waived"` attributes are already
      removed; what remains is the `parse_step_status` fallback in
      `boss-jobs/src/postgres.rs` (+ any SPA helpers). **Question:**
      confirm no DB row / fixture still relies on the fallback arms,
      then collapse to the canonical parse.

- [ ] **BC20: `account_risk_scores` legacy module.** ~600 LoC
      across `boss-accounts/src/account_risk_scores.rs` +
      `boss-ml::generators` churn block. The plugin in
      `boss-ml-plugins::account_churn_risk_v1` is the canonical
      post-cutover home. **Question:** confirm the Phase 2 cutover
      landed (the comment says it did) — if so, delete legacy
      module + the duplicate ml-generator code + the
      `/api/people/accounts/risk-scores` route.

- [ ] **BC-tail: remaining safe-delete items deferred for bulky
      callsite work.** Each is mechanical but touches enough call
      sites that landing the tranche needs a dedicated pass:
      - **`run_days` shim** in `boss-sim/engines/day_runner.rs`
        — rename `run_days_with_handlers` → `run_days`, drop the
        shim + the `run_days_legacy_path_dispatches_nothing`
        test.
      - **`SubjectCadence::fires_on(day)`** in
        `boss-sim/shape_driven/tenant.rs` — inline into
        `fires_on_tick`, drop the public method, update ~10 test
        callsites.
      - **`sku` ↔ `part_sku` serde aliases** across
        `boss-inventory-sim-bridge` + `boss-shipping` — pick one
        canonical name.
      - **`paginated.normalise` bare-array fallback** in
        `apps/web/src/data/paginated.ts` — confirm every
        paginated endpoint returns the envelope shape before
        removing.
      - **`entity_path` inbox fallback** in `boss-messages` + SPA
        `InboxPage.svelte` — confirm every emitter sets
        `entity_path` before dropping the dispatcher.
      - **`commerce.paid_on` `_day` alias** is currently load-
        bearing (counterparty trigger uses `inject_day`); keep
        until the sim retires the `_day` injection path. Not
        back-compat — leave alone.

---

## Post-release — strategic

Big-shape work pulled out of the launch path on purpose. None of
this gates the release; reopen once a real tenant's needs steer
which one matters first.

- [ ] **Edge-strip hardening for the gateway trust boundary.**
      `boss-gateway` injects `x-boss-user` when a valid session
      exists but does not strip a client-supplied one at the edge;
      backend services trust the header verbatim. The deployment
      model (gateway is sole ingress; backends bound to `127.0.0.1`
      or firewalled) is now documented in
      [SECURITY.md](SECURITY.md) §Deployment trust model. The
      code-level hardening — unconditionally strip inbound
      `x-boss-*` at the gateway edge and re-inject only trusted
      values, plus bind backends to `127.0.0.1` in
      `infra/deploy-services.sh` and fail-closed on the
      `change-me` bootstrap-admin default — folds into **Integrated
      IAM** below, but the edge-strip is worth doing on its own
      before real auth lands.

- [ ] **`boss-rebuild-all --audit-log-seed` pipe deadlock on slow
      disk.** Surfaced on a bare-metal install test on an
      HDD-backed VM. The mode shells out `gzip -d -c <bundle>` and
      reads from gzip's stdout via a pipe; on slow disk the
      read-loop can't drain the pipe faster than gzip fills it, and
      the process hangs:
        - gzip blocked in `anon_pipe_write` (pipe full)
        - rebuild-all blocked in `wait4(gzip)`
      SSD-backed deployments (docker quickstart, bare-metal on
      SSD) drain the pipe fast enough that the deadlock never
      manifests; we hit it only on HDD-backed VMs.
      Workaround (proven on the test VM): two-step load —
      `gunzip -c <bundle> | psql ...` to load audit_log, then
      `boss-rebuild-all` WITHOUT `--audit-log-seed` to project.
      Fix shape: rewrite the `--audit-log-seed` reader in
      rebuild-all/src to drain the pipe in a non-blocking task
      (tokio::process::Command + AsyncRead) instead of the
      current blocking pattern.

- [ ] **Income statement + cash flow report aggregation
      audits.** The balance-sheet endpoint fix closed one shape of
      the bug class — every JE balances by trigger, but a reporting
      endpoint can still mis-roll the categories. The BS endpoint
      now has invariant S asserting A=L+E in the daily conservation
      sweep + a property test in `boss-ledger/tests/http_api.rs`.
      The sibling reports (`/api/ledger/income-statement` and
      `/api/ledger/cash-flow`) need the same shape: structural
      audit that each endpoint's output satisfies its own
      invariants (revenue − COGS − OpEx = net income; cash-flow
      report's net-change-in-cash matches GL `account=1000`
      delta over the period). Mirror the BS pattern: refactor
      to a single kind-bucketed query where it isn't already,
      add `T` (income statement) and `U` (cash flow) invariants
      to the sweep, add property tests with multi-period seed
      data.

- [ ] **Sim-time threading across all financial facts.** The
      `consume_part` fix threaded `sim_day` through one HTTP path
      so auto-restock Jobs land on the sim's timeline instead of
      wallclock today. The same shape applies to every other
      side-effect emitter: products.produce / products.consume,
      inventory.receive, bill-approval, payroll-run-submit. Every
      one of those stamps wallclock `Utc::now()` for the resulting
      `financial_facts.happened_on`, so the live sim's facts would
      collapse onto install-day instead of the sim's timeline and
      break every date-keyed report.

- [ ] **Remaining "synthesized-amount" structural issues in the
      ledger.** Same bug-class as the `invoice_issued` COGS
      shortcut (since fixed): a posting rule or bridge invents a JE
      amount instead of deriving it from real per-row data. Five
      remaining of the same shape — none block the launch now that
      COGS is honest:
        - **bill_approved posts a lump `amount_cents`** instead
          of summing `purchase_order_lines.unit_cost_cents ×
          qty` off the linked PO. Memo + line breakdown lost at
          the GL. Fix: `BillApproveEmitter` should compute the
          total from the bridge's `expected_items` array (or
          the live PO); `bill_approved` rule should accept a
          per-SKU breakdown.
        - **revenue_recognized V2 emits zero COGS.** Ratable
          revenue recognizes 100% gross margin until V3 ships.
          Fix when ratable contracts become a real demo flow.
        - **payroll synthesize uses bps tables on annual
          salary.** Flat tenant-wide withholding / employer-tax
          rates stand in for per-employee jurisdiction-specific
          calcs. OK for the sim; revisit when a real tenant
          brings real W-4s.
        - **tax_accrued / tax_remitted trust payload amount
          verbatim** rather than reconciling against the actual
          `gl_journal_lines` balance for the period. A drift
          between snapshot and ledger leaves a residual on the
          liability account. Fix: validator should pull the
          `SUM(credit - debit)` for the (account_code, period)
          window and reject mismatches.
        - **commerce bridge applies tax as flat bps of line
          sum** rather than per-jurisdiction lookup with line
          exempt handling. Flagged in-code as sim-only.

- [ ] **Period close JobKind / cadence.** The validation script's
      step 7b creates + closes past fiscal years via `POST
      /api/ledger/periods/{id}/close`. Live playgrounds still need
      a JobKind that fires at year-end so ongoing sim activity
      continues to close cleanly. Existing
      `[periodic.quarterly-sales-tax]` infra is a model; new
      JobKind `fiscal-year-close` with a `tax-remittance`-like
      terminal step that POSTs to
      `/api/ledger/periods/{id}/close`.

- [ ] **Ledger: WIP-variance year-end close.** The
      `finance.period.closed` rule (`boss-ledger/src/rules.rs`)
      closes revenue/expense to retained earnings but does not yet
      handle a `wip_variance_cents` payload field — the residual
      WIP (1310) balance that should write off at year-end via a
      DR retained-earnings / CR 1310 adjustment so 1310 closes to 0
      without re-inflating the drained expense account. The test
      `period_closed_writes_wip_variance_to_retained_earnings` is
      `#[ignore]`d pending this; un-ignore it when the posting
      lands. Decide whether the WIP account (1310) is hardcoded or
      payload-driven before implementing.

- [ ] **Sales-tax accrual on retail / taproom invoices.** The
      brewery's wholesale invoices are tax-exempt (resale), but
      retail + taproom line items should accrue sales tax. The
      `commerce.invoice.issue` bridge supports `tax_rate_bps` +
      `tax_jurisdiction` already, but the side-effect spec
      doesn't set them. Once retail / taproom JobKinds exist
      (see below), wire `tax_rate_bps` on their billing steps.
      The `[periodic.quarterly-sales-tax]` JobKind is disabled
      because it remitted without an accrual flow; re-enable when
      the flow is whole.

- [ ] **Taproom-pour and distribution-contract JobKinds.** 4120
      (Taproom) and 4140 (Distribution Contracts) are revenue
      accounts that no JobKind currently posts to.
      `seasonal-release` posts to 4130 (event-package);
      `wholesale-keg-order` → 4100; `direct-shop-order` → 4110
      (retail). Missing flows:
        - **taproom-shift-close**: aggregate per-shift pours
          and emit one invoice with `revenue_category =
          "taproom"`.
        - **distribution-contract**: recurring monthly invoice
          for distributor agreements; `revenue_category =
          "distribution"`.
      Without these the brewery's income statement is missing
      ~10% of plausible revenue.

- [ ] **External CRM integration is post-release.** The
      preliminary release ships with zero external CRM adapters.
      When a real tenant asks for one, design it as a port-shaped
      adapter crate under `crates/tenants/` — explicitly outside
      core.

- [ ] **Cloud-provider blueprints — opt-in recipes, no core
      dependence.** The release ships exactly two install paths
      (`infra/oss-quickstart/quickstart.sh` for bare-metal +
      the Docker compose stack). Cloud-provider provisioning
      recipes (Azure Bicep, GCP, Cloudflare Tunnel + Origin
      Cert, AWS, Hetzner) come back post-release as
      `infra/blueprints/<provider>/` directories, each with a
      README walking the operator through "stand up a VM in
      your cloud + run quickstart.sh on it." No part of BOSS
      core depends on any of these — they're worked-example
      recipes, not load-bearing infrastructure. Operators
      discover them via `infra/blueprints/README.md` index;
      none referenced from CLAUDE.md / README / CLI defaults.

- [ ] **Production infrastructure template — single-VM today,
      multi-VM when warranted.** Current prod posture is one VM
      running the full stack with `pg_dump` (or
      `infra/backup.sh`) backups plus `audit_log` itself as the
      disaster-recovery primitive (any snapshot replays cleanly
      via `boss-rebuild-all`). The template gets fleshed out
      post-release: a polished "stand up a prod VM in <30 min"
      runbook, documented backup cadence + restore drill, and
      **the CI/CD pipeline that ships binaries to it**
      (`.github/workflows/deploy-prod.yml` cross-compiles every
      `boss-*-api` for `x86_64-unknown-linux-gnu`, scps to the
      prod VM, restarts via `infra/deploy-services.sh` —
      today's `release.yml` covers only the boss CLI). Multi-
      VM topologies (warm-standby, active/active, edge LB) are
      out of scope until a real tenant's SLAs force the
      conversation.

- [ ] **Integrated IAM — Authelia (or any OIDC IDP) via
      forward-auth.** The release ships file-backed credentials —
      fine for evaluation, not production (see [SECURITY.md](SECURITY.md)
      §Deployment trust model). The gateway already mints sessions
      from a `Remote-User`-shaped input, which is exactly the
      contract Authelia / Keycloak / Dex / Pomerium speak when
      fronting a proxied app. Post-release deliverable:
      `infra/authelia/` as the reference bundle (systemd unit +
      sample `configuration.yml` + role/group-to-role mapping) plus
      a runbook that walks "bring your own VM" → "DNS + TLS" →
      "Authelia up + first user" without any SaaS dependency.
      Adds: WebAuthn / TOTP / SSO / proper email-OTP password
      reset / account lockout / audit-log of identity events.
      Cloudflare Access stays available as opt-in edge hardening
      (DDoS / WAF / origin-IP hiding) on top of the origin-tier
      IAM; the gateway code path doesn't care which IDP terminates
      the auth. Includes the edge-strip hardening called out at the
      top of this section.

- [ ] **Workflow modeling UX improvements.** The release ships a
      functional `/system/job-kinds` editor (read-only catalog at
      `/system/workflows`, full author surface at `/system/job-kinds`) plus
      `/system/step-plugins` for custom step UX bundles. The
      modeling experience is correct but rough — it leans on
      operators understanding the StepGraph / TierSpec / step
      registry up front. Polish:
        - Visual DAG editor with drag-to-reorder steps and
          drag-to-connect blocked-by edges (today's editor is
          form-driven tier list).
        - Step-template gallery — drop a `scheduling` /
          `repair` / `quote` / `sign-off` step from a palette
          rather than typing the kind slug.
        - Live preview pane: open a JobKind in "what would a
          new Job of this kind look like" mode.
        - Validation hints surfaced inline (metadata schema
          required-at-done fields, blocker cycles, missing
          authority roles for sign-off steps).
        - Per-tenant JobKind diff view so a head of department
          can see what changed between published versions
          without git-blame archaeology.
      Compounds with Integrated IAM above — once dept heads
      sign in with their real identity, the modeling UX they
      reach is what the platform is for.
      - **used-device-shop `job-kind-approver` parity.** Core defaults
        grant `step-signoff:job-kind-approver` to `platform-admin` and the
        brewery grants it to its leaders; the used-device-shop tenant has
        no grant, so if it ever drives `job-kind-design` Jobs its leaders
        can't approve. Add the grant when that tenant authors JobKinds.

- [ ] **Information theory on audit_log — triage, anomaly
      detection, error handling.** The audit_log is a stream
      of typed events with rich structure that BOSS today
      treats as a flat list. Information theory gives a math
      vocabulary for operating on the stream that fits the
      project's cybernetics framing (Beer's algedonic signals
      are exactly the high-information attention-grabbing
      events). Concrete directions:
        - **Per-topic entropy / surprise rate.** High-
          frequency / low-surprise (`jobs.step.updated`) gets
          compressed in operator views; low-frequency / high-
          surprise (`commerce.invoice.past_due`,
          `jobs.job.cancelled`, `audit_log integrity check
          failed`) bubbles up.
        - **Distributional anomaly detection (KL divergence).**
          For sliding 5-min / 1-hr / 1-day windows, compare
          topic-frequency distribution against a historical
          baseline. Sudden KL spike = something off.
        - **Cross-topic mutual information for chain-integrity
          checks.** Some topic pairs are tightly coupled
          (`step.completed.billing` → `commerce.invoice.created`).
          MI drops if the chain breaks — would have caught the
          BatchEngine-empty + missing-tax-route bugs
          automatically.
        - **Information-bottleneck triage** for operators.
          Smallest event subset whose conditional entropy of
          current state is below a threshold = the dashboard's
          recommended-attention list.
        - **Error-budget framing** per error-topic, SRE-style.
      Probably a new analytics layer on top of audit_log
      queries; pairs with `boss-audit-integrity-check.timer`.

---

## Post-release — polish

Lower-priority automation + UX nice-to-haves. Pull forward only
if a contributor wants them.

- [ ] **CI workflow: run the sim-validation gate on tagged
      releases.** Goal: `git tag vX.Y.Z && git push --tags`
      runs `validate-brewery-sim.sh` (a sim-year + clean-rebuild
      assertion) automatically, so a release can't ship a sim that
      doesn't reconstruct. Blocker is that the script is heavily
      systemd-coupled today (stops/starts boss-* units, drops the
      live DB via sudo) — it doesn't run as-is in a GitHub Actions
      runner. Two real paths to land it:
        (a) Refactor the regen to drive services via Docker
            compose (the `infra/oss-quickstart/` stack) instead
            of systemctl, then wire the workflow to run inside
            that stack on the runner. Probably ~4-6 hours of
            work. Full 365-day regen would take ~90 min on
            GH-hosted runners.
        (b) Pin a self-hosted runner on a beefy VM that
            already has systemd + the boss tooling installed.
            The workflow becomes trivial (`git pull && infra/
            postgres/validate-brewery-sim.sh`). Pays a real
            ongoing maintenance cost for the VM.
      Pull forward when there's a tenant cutting frequent
      releases who'd benefit from the automation.

- [ ] **Landing-page state-machine visualizations — second
      pass.** The current SVGs on the landing page introduce
      Subject / Job / Step / Event reasonably but stay at the
      "boxes and arrows" level. Goal: animate the live event
      tail driving a Job through its Step DAG so a first-time
      visitor sees the model *operating* instead of just
      diagrammed. Tie the tick rate to the same 1s landing-
      page refresh and use real audit-log frames so the
      visualization is the literal projection, not a mock.

- [ ] **Finished-product `/shop` integration — Phase 2 tail.**
      Phase 1 fully shipped (5→11 SKUs across 5 beer styles,
      `direct-shop-order` JobKind, `shipment_line_items`
      projection, production + sale side-effects via
      `products.produce` / `products.consume` handlers, `/shop`
      SPA + sidebar module gate). One open sub-item:
        - **Per-customer email-OTP gate.** Today the shop POST
          opens the Job under the catch-all `acc-direct-shop`
          Account regardless of who's checking out, and
          `/shop` + `/shop/{sku}` require a session. Public-
          by-default shape: routes readable without a session
          (matches `/api/jobs/kinds*`); checkout opens an
          email-OTP or magic-link step before the Job is
          created. Blocks on the email-OTP design call — which
          also gates "should /shop create a thin per-customer
          Account on checkout."

- [ ] **A/R aging SPA view filters by status.** With the period
      write-off pipeline shipped, invoices that sit past-due > 60
      sim-days flip to `'written-off'` and the GL drops them off
      A/R. The SPA's A/R aging view at `/finance` (or wherever the
      AR aging table lives) currently groups all non-paid invoices
      together; once the bundle ships with written-off rows, the
      view should exclude them from "outstanding" and either hide
      them entirely or surface them in a separate "written off
      (uncollectable)" row so the operator sees the real
      receivables vs the historical uncollectables at a glance.
      Small SPA change (~1 hour). Pair with the existing past-due
      display so the same view shows: outstanding (current) /
      outstanding (past-due) / written-off (uncollectable).

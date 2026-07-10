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

## Near-term queue (2026-07-03)

The active workstream order after the costing-fidelity arc (#51–#63),
the overhead-absorption review cleanup (#73), and the deep-gate
first-contact fixes (#74). Sorted by dependency, not size.

- [ ] **Full-year regen validation of #73 + #74.** Neither has had the
      365-day from-empty run (ephemeral machine + all conservation
      gates). Both changed year-scale behavior: #73's drain now NAKs on
      failed/partial ledger reads (the matched-count tripwire must
      *converge* across ~2,500 brews at warp, not dead-letter), and #74
      changed fact-id derivation for every fact plus the rebuild path
      (the determinism guard + end-of-year deep check exercise it at
      scale). Also produces the first steady-state gross-margin read
      with all the new mechanics (~77% at day 117 live; the full year
      with FG turnover settles it).
- [ ] **Costing PR 6 — WIP reconciliation.** The consume-side ~$421K
      residual isolated by the GR-IR work (moving-average/WIP costing:
      gl_1300 > physical), plus the per-line drain rounding sweep #73
      documented at the produce handler's unit-cost derivation (exact
      per-line conservation needs the produce endpoint to accept a line
      total instead of a unit cost).
- [ ] **Overhead rates as dispatcher rule args × the step's bbl.** The
      excise-accrual pattern (`ledger.tax.accrue`: rate as a rule arg,
      multiplied by the step's actual metadata at runtime), applied to
      absorption — retires the 15 hand-multiplied `amount_cents`
      constants in the brewery seed. The seed test pinning
      amount == rate × batch_bbl holds the contract until then.
- [x] **`HandlerError::Permanent`** — done. The house contract does
      the classification: services answer **422** for deterministic
      request-data errors (seed typos, malformed bodies), and the
      shared post/get helpers map exactly that status to `Permanent`;
      409 (convergent conflicts, e.g. insufficient stock — load-bearing
      for the 6b backorder path) and 404 (not-yet-projected) stay
      retryable, as do all 5xx. The runner Terms an event only when
      EVERY failed handler is deterministic (Permanent/MissingArg/
      BadArgType); any transient in the mix NAKs so the idempotent
      re-run converges it. Permanent Terms log the same `DEAD-LETTER:`
      pattern with `class=permanent`, so gates and operators keep one
      vocabulary.
- [x] **Dedup audit-event emits on redelivery** — done for the
      fact-backed occurrence events (`record_fact_in_tx` returns
      `FactRecorded { id, inserted }`; ledger manual-entry +
      inventory-movement, inventory receive (`ReceiveApplied`) and
      overhead-absorbed all gate their emits; consume/produce were
      already gated). DELIBERATELY left at-least-once: state-snapshot
      events (`ITEM_UPSERTED`, `PRODUCT_INVENTORY_UPSERTED`,
      `INVOICE_CREATED`, …) — they are the last-write-wins rebuild
      sources, so gating them would turn projection recovery into
      at-most-once; a duplicate snapshot is harmless by design. The
      CRUD surfaces (vendors/contacts/accounts-team) emit per client
      call and sit outside the NAK-redelivery path — revisit only if
      client retries show up in the log.
- [x] **Costing PR 7 — BOM expansion** — done where defensible:
      per-style ingredient variety (pilsner base + lager yeast for the
      lager; roasted barley for the stout; flaked wheat + Citra for
      the hazy; Cascade/Citra split for the IPA — six new sourceable
      SKUs at 2025 wholesale) and real case packaging
      (PKG-CAN-12OZ-CS24, ~$3.80/case bundle — the 12oz format's cans
      were free before, understating its COGS ~17%). Water is
      DELIBERATELY not a part: it is already costed inside the 6300
      process-utilities absorption driver, so a water BOM line would
      double-count (documented in parts.toml). Everything flows
      through drain-actual-wip automatically — no stamped amounts
      anywhere (per #77).

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

- [x] **Deep replay-check write-stall window** — fixed: both replay
      checks now shadow the mutable ledger tables with TEMP clones
      (`pg_temp` name resolution carries the whole existing replay
      path unchanged) under REPEATABLE READ, so live tables are never
      locked or written and concurrent writers see one consistent
      snapshot instead of a frozen world. The entry-level check's
      open-period DELETE row-locks went with it. Lock-freedom is
      pinned by a race test (concurrent writer with a 2s statement
      timeout across both the mechanism and the real deep check).
      #95's timer quiescence stays as belt-and-braces for regens.

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

- [ ] **Conservation-P: finished-goods cost-basis reconciliation
      (consume-side).** `finished_product_inventory.production_cost_cents`
      is an integer-rounded moving weighted average
      (`boss-products/{in_memory,postgres}.rs`) while the GL tracks exact
      transaction costs, so 1320 can diverge from physical-on-hand-at-cost
      by a small percentage. Fix shape: per-SKU `GL − (on_hand × cost)`
      diff on a fresh regen to attribute the drift, then decide whether
      the basis reconciles to exact cost or the closure check tolerates
      rounding. **Distinct** from the WIP-variance close above (that's the
      1310 residual; this is the 1320 cost-basis drift), and **coupled to
      costing PR4** (production-drivers-into-COGS changes the FG basis) —
      land after PR4 so the reconciliation targets the final basis.

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

## Coherence audit backlog (2026-07-01)

A read-only coherence audit (4 parallel agents: financial/ledger, frontend,
projections/rebuild, cross-boundary contracts) found the core sound
(double-entry enforced at 3 layers, WIP→FG→COGS reconciles, projections
mostly deterministic) but surfaced these edge / display / latent issues.
Grouped by priority; the display cluster is being tackled first.

### Display — user-visible wrong/wonky numbers (frontend)

- [ ] **Account-class rollup count isn't rendered in the cockpit.** The
      Rust sends a per-class `distinct` account count, but
      `apps/simulator/src/CockpitPage.svelte` has `DISTINCT_NOUN.account =
      ''`, so `distinctLabel('account', n)` returns `''` and "N accounts"
      never shows. Set `account` (and `vendor`) to a real noun. Completes the
      account-class rollup PR (its Rust half). *(→ folded into that PR.)*
- [ ] **"Paid YTD" is actually lifetime-paid.**
      `apps/web/src/accounts/AccountPage.svelte:177` sums all paid invoices
      with no year filter; overstates for any account with history. Filter
      `paid_on >= startOfYear`, or relabel.
- [ ] **~20 sites format money as `$${(cents/100).toLocaleString()}` and
      drop cents** ($42.50→"$42.5", $42.00→"$42"). PoPage, VendorPage,
      VendorInvoicePage, VendorsList, PartPage, AccountPage, RepairSurface,
      DevicePage, CatalogBrowser. Sweep to web-kit `formatMoney`/`formatUsd`.
- [ ] **Indirect cash-flow shows the subtotal but zero operating line
      items.** Rust moved the breakdown to a new `operating_activities`
      field (`boss-ledger/src/http/statements.rs:679,688`); the TS
      `CashFlowStatement` + `CashFlowTab` still render the now-always-empty
      `working_capital_adjustments`/`non_cash_adjustments`. Add + render
      `operating_activities`.
- [ ] **`written-off` invoices render a blank status chip.** TS
      `InvoiceStatus` union (`apps/web/src/finance/types.ts:5`) is missing
      `'written-off'` (a real terminal state) → `INVOICE_STATUS_LABEL[status]`
      is `undefined`; `InvoicesTab` also miscounts them as "unpaid". Add to
      union + label + chip variant. *(Overlaps the A/R-aging polish item above.)*

### Correctness — numbers that don't add up under the hood

- [ ] **`financial_facts` payloads diverge live-vs-rebuilt.** The publisher
      injects `_actor`/`_simulated` into the event payload
      (`boss-core/src/publisher.rs:123-140`) but the in-tx fact omits them
      (`boss-inventory/http/items.rs:239-252`, `boss-products/http.rs`), so
      `rebuild_facts` reconstructs a different payload. Masked because the
      shipped gate (`boss_ledger_replay_check.rs`) only diffs journal *lines*.
      Build the in-tx fact payload from the post-injection value (or strip the
      injected keys in `rebuild_facts`).
- [ ] **Message ages are nonsense on some messages.** `boss-messages/http.rs`
      stamps `sent_at` with sim-time on one path (:344) and `Utc::now()` on
      another (:394); the frontend ages against sim-time. Stamp sim-time on
      both send paths.
- [ ] **Income-tax net-income sums a dead `kind='cogs'` bucket** (always 0 —
      the chart has no `'cogs'` kind, COGS is `'expense'`).
      `boss-ledger/src/http/tax.rs:277-295`. Total is right by accident;
      switch to `code LIKE '5%'` and align with the income statement.
- [ ] **`gl_account_daily` panics on a missing account code.**
      `boss-ledger/src/postgres.rs:318,339` `account_ids[line.account_code]`
      indexes a HashMap (no-panic-in-library violation). Use a checked lookup
      → `LedgerError`. Also add `gl_account_daily` to a replay-check (live
      i128 vs rebuild SQL `trunc` parity is correct-but-unverified).
- [ ] **Shipping status roll-up is replay-order-dependent.**
      `boss-shipping/src/rebuild.rs:164-195` sets status from tracking scans in
      `id` order with no advance-only gate → live-vs-rebuilt `status`/`shipped_on`
      divergence on out-of-order delivery. Gate on `stage_index` monotonicity.
- [ ] **Inconsistent replay `ORDER BY` across rebuilders** (`id` vs
      `(timestamp, event_id)`). Under sim-time many events share a timestamp →
      tie-break on a random UUID. Order-invariant for pure upserts today;
      standardize on `ORDER BY id`.

### Latent / low

- [ ] **Cash-attribution split loses cents on 3+-line cash JEs.**
      `upsert_daily_rollup` (`boss-ledger/postgres.rs:344`) truncates the
      proportional split and never redistributes the remainder → surfaces as
      `reconciliation_gap_cents`. Give the largest-share offset the residual.
- [ ] **Cash-flow "indirect" branch is actually the direct method** (header
      comment inverted) and the **investing section is always empty** —
      equipment cash is attributed to 2100 A/P → classified operating.
      `boss-ledger/src/http/statements.rs:439+`.
- [ ] **`post_inventory_movement` hardcodes the FactRef kind**
      (`boss-ledger/src/http/facts.rs:465`) ignoring its `fact_kind` param.
      Latent (both kinds share a rule today).
- [ ] **New-invoice header total ≠ line-item sum on sub-cent inputs**
      (`apps/web/src/finance/NewInvoicePage.svelte:164` rounds the summed
      float; lines round independently) → trips the "line sum ≠ header"
      warning on a just-created invoice.
- [ ] **Zero-cost stock mutations double-apply on NAK-redelivery.** When
      `avg_cost == 0` no proof-fact is written, so the idempotency guard has
      nothing to check (`boss-products/postgres.rs:210`, `boss-inventory/
      postgres.rs:150`). Self-heals on rebuild. Write a zero-amount proof-fact.
- [ ] **Schedule-triggered dispatcher rules break the cascade graph.** TS
      `DispatcherRule.on_event` is non-optional; a scheduled rule omits it →
      `EVT(undefined)` node (`apps/web/src/dispatcher/cascadeToGraph.ts`).
      Latent — 0 scheduled rules today.
- [ ] **Messages `archived` rebuild overwrites `kind` + drops `archived_at`**
      (`boss-messages/src/rebuild.rs:179`). Projection-fidelity gap.
- [ ] **Commerce status-flip rebuild skips line items** if the `created` event
      is outside the audit slice (`boss-commerce/src/rebuild.rs:109`). Edge.
- [ ] **`AccountPage` `kFmt` always renders "$…K"** → $400 shows "$0K". Reuse
      the tiered `ExecPage` formatter.
- [ ] **Cockpit cadence label shows the per-pass batch cap as "per tick"**
      (`CockpitPage.svelte:209`) — overstates granularity at hourly ticks.
- [ ] **(unverified) `TaxFiling` TS unions narrower than the Rust `String`**
      (`liability_account`/`kind`) — mis-narrows if a new code is added
      server-side.

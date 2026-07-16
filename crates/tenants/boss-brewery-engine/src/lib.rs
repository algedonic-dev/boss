//! Brewery operating-engine library — shared between the
//! `boss-brewery-engine` CLI and integration tests.
//!
//! Two entry points:
//!
//! - [`run_brewery`] — convenience wrapper that uses
//!   [`InMemoryOutput`] and returns it for assertion-style use
//!   (test fixtures, the CLI's default mode).
//! - [`run_brewery_into`] — generic-output variant for running
//!   against a [`LiveApiOutput`] pointed at a real API stack so
//!   the engine's writes flow through `DomainPublisher` →
//!   `PgAuditWriter`. The 12-month seed-generation path uses
//!   this.
//!
//! See `docs/design/correctness-protocol.md`,
//! `docs/design/seed-vs-emergent-state.md`, and
//! `docs/design/projection-rebuilders.md` § E for context.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use boss_calendar_client::{CalendarClient, ReqwestCalendarClient};
use boss_core::calendar::BusinessCalendar;
use boss_inventory::types::{InventoryItem, VendorBehavior};
use chrono::NaiveDate;
use serde::Deserialize;

use boss_jobs::seed_loader::load_job_kinds_with_owning_team;
use boss_jobs::step_registry::StepRegistry;
use boss_sim::calendar::CalendarRegistry;
use boss_sim::engines::{
    CounterpartyEngine, CounterpartySpec, DelaySpec, PeriodicEngine, RunReport, SimBusEvent,
    SimEventBus, end_of_day_rollup, run_one_tick_with_handlers, run_ticks_with_handlers,
};
use boss_sim::output::{InMemoryOutput, SimOutput};
use boss_sim::rng::Rng;
use boss_sim::shape_driven::{ShapeDrivenState, TenantConfig};

/// JobKind-publish logic shared by the `boss-brewery-bootstrap`
/// binary and the future unified "prepare" step.
pub mod prepare;

/// Control + telemetry HTTP server for the live `boss-brewery-sim`
/// daemon (localhost-only; boss-simulator proxies to it).
pub mod sim_control;

/// Result of a brewery-engine run — the day-loop's RunReport
/// plus the populated InMemoryOutput so callers can assert on
/// emitted facts.
pub struct BreweryRunResult {
    pub report: RunReport,
    pub output: InMemoryOutput,
}

/// Initial inventory state. The brewery's JobKinds reference
/// part SKUs in their `consume_parts` metadata; those parts
/// must exist in the inventory items table before the
/// dispatcher fires the consume call, or the POST returns 404
/// and the side effect is silently dropped.
/// `parts.toml` enumerates every SKU plus its initial bin /
/// on-hand / reorder thresholds. The brewery-engine
/// live-api mode POSTs these to `/api/inventory/items/batch`
/// before the day loop starts. This is initial-conditions seeding
/// (allowed by `docs/design/seed-vs-emergent-state.md`); usage,
/// reorders, and PO/vendor-invoice flow emerge from the sim.
#[derive(Debug, Deserialize)]
struct PartsBundle {
    parts: Vec<InventoryItem>,
}

/// Load `parts.toml` from the seed bundle. Missing file is OK —
/// returns an empty list (older seed bundles predate the parts
/// catalog).
pub fn load_parts(seeds: &Path) -> Result<Vec<InventoryItem>> {
    let path = seeds.join("parts.toml");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut bundle: PartsBundle =
        toml::from_str(&body).with_context(|| format!("parsing {}", path.display()))?;
    // parts.toml authors a per-unit cost (the natural authoring shape);
    // the conserved row quantity is value_cents (PR 6a, value-primary).
    // Derive it ONCE here so every consumer — the batch upsert, the
    // opening JE, tests — sees the same exact total and the GL equals
    // physical from the first event.
    for part in &mut bundle.parts {
        part.value_cents = part.on_hand as i64 * part.avg_cost_cents;
    }
    Ok(bundle.parts)
}

/// Resolve the kind-scoped subject-mint URL for `api_base` —
/// `direct://<host>` goes straight at the subject-kinds service port,
/// anything else is a gateway-style base.
fn subject_mint_url(api_base: &str, kind: &str) -> String {
    if let Some(host) = api_base.strip_prefix("direct://") {
        format!("http://{host}:7830/api/subjects/{kind}")
    } else {
        format!("{}/api/subjects/{kind}", api_base.trim_end_matches('/'))
    }
}

/// Mint one identity row via `POST /api/subjects/{kind}` — the
/// generic write-side of identity-first for table-less kinds.
/// Blocking reqwest — call from spawn_blocking (or a blocking
/// context like prepare). Idempotent (the endpoint upserts).
pub fn mint_subject_identity(
    kind: &str,
    id: &str,
    label: Option<&str>,
    api_base: &str,
) -> Result<(), String> {
    let client = reqwest::blocking::Client::new();
    let url = subject_mint_url(api_base, kind);
    let body = match label {
        Some(l) => serde_json::json!({ "id": id, "label": l }),
        None => serde_json::json!({ "id": id }),
    };
    let resp = client
        .post(&url)
        .header("x-sim-origin", "true")
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("{} {}", url, resp.status()))
    }
}

/// Sync the sim's campaign pool into its domain home via
/// `POST /api/campaigns` (boss-campaigns, Q4) — one create per pool
/// slug, so tap-launch Jobs pass the uniform gate AND every pool
/// campaign is a real domain row with a `campaigns.campaign.created`
/// event behind it. Idempotent: the endpoint reports 200 (nothing
/// written, nothing emitted) for ids that already exist. Blocking
/// reqwest — call from spawn_blocking. Best-effort per id: a refused
/// create logs and moves on (the daemon boot path must not die on
/// one bad campaign slug).
pub fn mint_campaign_identities(campaign_ids: &[String], api_base: &str) {
    let client = reqwest::blocking::Client::new();
    let url = if let Some(host) = api_base.strip_prefix("direct://") {
        format!("http://{host}:7845/api/campaigns")
    } else {
        format!("{}/api/campaigns", api_base.trim_end_matches('/'))
    };
    for id in campaign_ids {
        let sent = client
            .post(&url)
            .header("x-sim-origin", "true")
            .json(&serde_json::json!({ "id": id }))
            .send();
        match sent {
            Ok(r) if r.status().is_success() => {}
            Ok(r) => {
                tracing::warn!(id = %id, status = %r.status(), "campaign create refused")
            }
            Err(e) => tracing::warn!(id = %id, error = %e, "campaign create failed"),
        }
    }
}

/// Seed the canonical brewery subjects so the JobKinds have
/// targets to anchor on. Production deployments derive this
/// from live tables; the standalone runner / test path uses
/// this hand-seed.
pub fn seed_brewery_subjects(state: &mut ShapeDrivenState) {
    // Inventory + finished-goods stock live in the live inventory /
    // products services now — the sim holds no on-hand mirror. Raw
    // parts are seeded into the inventory-api by the binary's
    // `seed_parts`; the workforce executor reads real on-hand back
    // when it gates production-consume / demand-gate steps.
    // Q6: the organization itself is a Subject — org-level JobKinds
    // (payroll, tax filings, AP runs, facility overhead, the
    // production heartbeat) open their Jobs about it. One row per
    // tenant; the id matches tenant.toml meta.tenant_id.
    state.seed_subject("company", "brewery");
    state.seed_subject("location", "loc-brewery-brewhouse");
    state.seed_subject("location", "loc-brewery-taproom");
    state.seed_subject("location", "loc-hq");
    for i in 0..50 {
        state.seed_subject("account", &format!("acc-bigseed-{i:04}"));
    }
    // Keep in sync with boss_brewery_data_seed::VENDOR_COUNT — the
    // auto-restock vendor resolver can pick any seeded vendor (incl.
    // the packaging supplier at index 12), and each must be a Subject
    // so the restock Job's subject resolves.
    for i in 0..13 {
        state.seed_subject("vendor", &format!("vnd-bigseed-{i:03}"));
    }
    // Campaign Subjects for the tap-launch + seasonal-release
    // marketing flows. The tap-launch JobKind (rate 0.04 ≈
    // 3/quarter in tenant.toml) attaches to a campaign Subject,
    // so the marketing surface needs anchors to fire against.
    // Six evergreen campaigns cover a typical brewery's seasonal
    // calendar, giving the marketing surface continuous activity.
    for slug in [
        "cmp-spring-saison",
        "cmp-summer-fest",
        "cmp-oktoberfest",
        "cmp-winter-stout",
        "cmp-anniversary",
        "cmp-collab-rotational",
    ] {
        state.seed_subject("campaign", slug);
    }

    // Populate the role-keyed Employee pool so `advance_steps` can
    // pick a real Employee actor for each step transition
    // (sim-fidelity). Reads
    // examples/brewery/seeds/employees.json — the same roster the
    // people projection holds — and groups by role. Without this,
    // every audit_log row would read `automation:brewery-sim` and
    // the demo would lose the "human-powered state machine" framing.
    let employees_path = std::path::Path::new("/opt/boss/examples/brewery/seeds/employees.json");
    if let Ok(bytes) = std::fs::read(employees_path)
        && let Ok(roster) = serde_json::from_slice::<Vec<serde_json::Value>>(&bytes)
    {
        for emp in roster {
            let id = emp.get("id").and_then(|v| v.as_str());
            let role = emp.get("role").and_then(|v| v.as_str());
            let status = emp
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("active");
            if status != "active" {
                continue;
            }
            if let (Some(id), Some(role)) = (id, role) {
                state.register_employee(role, id);
                // Subjects-by-kind too so `subject_kind=employee`
                // JobKind rates can pick assignees from the same
                // pool.
                state.seed_subject("employee", id);
            }
        }
    }
}

/// Mutable engine state a long-running daemon (boss-brewery-sim)
/// holds across ticks. The in-flight pending state of every
/// CounterpartyEngine chain (bank-ach 30bd delay, ar-aging
/// 30bd delay, malt-supplier 5bd, keg-courier 3-stage scans)
/// must survive tick boundaries; at 1-day chunks every chained
/// delay >1 day would otherwise be lost on each chunk.
///
/// Construct via [`BreweryEngineState::load`] once, then call
/// [`run_brewery_one_day`] in a loop to advance day-by-day
/// without losing pending-action state.
pub struct BreweryEngineState {
    pub kinds: Vec<boss_jobs::registry::JobKindSpec>,
    pub registry: StepRegistry,
    pub tenant: TenantConfig,
    pub state: ShapeDrivenState,
    pub rng: Rng,
    pub periodic: PeriodicEngine,
    pub counterparty: CounterpartyEngine,
    /// Cross-tick bus + report. When the daemon ticks per-tick
    /// (rather than per-sim-day), the bus must live across the
    /// ticks of a sim-day so the Counterparty engine's last-tick
    /// drain sees the full day's events. Reset in
    /// [`run_brewery_one_day`] to preserve its day-boundary
    /// semantics; held across ticks by [`run_brewery_one_tick`]
    /// + [`brewery_end_of_day`].
    pub bus: SimEventBus,
    pub report: RunReport,
}

impl BreweryEngineState {
    /// Build a fresh engine state from the seed bundle. Same
    /// initialization `run_brewery_into` does internally —
    /// extracted so daemons can hold one across many calls.
    ///
    /// Every step-completion side effect routes through the
    /// boss-dispatcher rule registry, which subscribes to
    /// `step.done.<kind>` on NATS and fires the registered HTTP
    /// handlers. The engine just drives Job/Step lifecycle here.
    /// `calendars` are the business calendars the engines consult
    /// (counterparty delays, periodic postponement, the sampler's
    /// holiday demand-multiplier) — passed as DATA so a single fetch
    /// feeds every consumer. The daemon fetches these from
    /// boss-calendar (see [`fetch_calendars`]) so there is one source
    /// of truth; the offline regen + test paths pass
    /// [`test_calendars`]. The `us-banking` calendar is copied onto the
    /// engine's `ShapeDrivenState` for the sampler; the periodic +
    /// counterparty engines each get their own registry built from the
    /// same data.
    pub fn load(
        seeds: &Path,
        calendars: Vec<boss_core::calendar::BusinessCalendar>,
    ) -> Result<Self> {
        let tenant_path = seeds.join("tenant.toml");
        let tenant = TenantConfig::load(&tenant_path)
            .with_context(|| format!("loading tenant config from {}", tenant_path.display()))?;
        // Offline / test path: no live model to read vendor behavior from, so
        // no synthesized supplier specs (the daemon path passes the
        // API-fetched behaviors via `load_with_tenant`).
        Self::load_with_tenant(seeds, tenant, calendars, Vec::new())
    }

    /// Same as [`load`], but with a caller-supplied [`TenantConfig`]
    /// instead of the seed `tenant.toml` — used by the control plane to
    /// boot the daemon from an operator-edited config override. Job
    /// kinds + subjects still come from the seed bundle; `calendars` are
    /// threaded through exactly as in [`load`].
    pub fn load_with_tenant(
        seeds: &Path,
        tenant: TenantConfig,
        calendars: Vec<boss_core::calendar::BusinessCalendar>,
        vendor_behaviors: Vec<(String, VendorBehavior)>,
    ) -> Result<Self> {
        let kinds_path = seeds.join("job_kinds.toml");
        let kinds = load_job_kinds_with_owning_team(&kinds_path, &tenant.meta.tenant_id)
            .with_context(|| format!("loading job kinds from {}", kinds_path.display()))?;
        let registry = StepRegistry::v1();

        let mut state = ShapeDrivenState::new();
        seed_brewery_subjects(&mut state);
        // The sampler's weekday/weekend/holiday demand multipliers read
        // us-banking off state. Seed it from the fetched data; if it
        // wasn't fetched, the all-business fallback applies (no holiday
        // suppression — degrades safely).
        if let Some(us_banking) = calendars.iter().find(|c| c.code == "us-banking") {
            state.set_us_banking(us_banking.clone());
        }

        // The periodic + counterparty engines each own a registry; both
        // are built from the same fetched data.
        let periodic = PeriodicEngine::new(
            tenant.periodic_specs(),
            CalendarRegistry::from_data(calendars.clone()),
        );
        // The counterparty specs = the tenant.toml chains (ar-aging, bank,
        // broadcasts) PLUS one synthesized supplier spec per vendor, built
        // from each vendor's behavior read from the model. The vendor specs
        // replace the (removed) hand-authored grain/hops blocks: each real
        // vendor now responds to its own procurement with an invoice paced by
        // its own lead time + fulfilment, instead of two placeholder chains.
        let mut counterparty_specs = tenant.counterparty_specs();
        counterparty_specs.extend(synthesize_vendor_specs(&vendor_behaviors));
        let counterparty =
            CounterpartyEngine::new(counterparty_specs, CalendarRegistry::from_data(calendars));
        let rng = Rng::new(tenant.meta.seed);

        Ok(Self {
            kinds,
            registry,
            tenant,
            state,
            rng,
            periodic,
            counterparty,
            bus: SimEventBus::new(),
            report: RunReport::default(),
        })
    }
}

/// The distinct business-calendar codes the tenant's counterparty +
/// periodic specs reference, plus an unconditional `"us-banking"` (the
/// shape-driven sampler always needs it for its holiday demand
/// multipliers). Sorted + de-duplicated. This is the fetch list the
/// daemon hands to [`fetch_calendars`].
pub fn brewery_calendar_codes(tenant: &TenantConfig) -> Vec<String> {
    let mut codes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // The sampler reads us-banking regardless of any spec referencing it.
    codes.insert("us-banking".to_string());
    for spec in tenant.counterparty_specs() {
        if let Some(c) = spec.delay.business_calendar {
            codes.insert(c);
        }
    }
    for spec in tenant.periodic_specs() {
        if let Some(c) = spec.business_calendar {
            codes.insert(c);
        }
    }
    codes.into_iter().collect()
}

/// Fetch each named calendar from the boss-calendar service via
/// `boss-calendar-client`, returning the ones that resolve. A calendar
/// that 404s (absent) or whose fetch errors (service down) is logged
/// and skipped — the `CalendarRegistry`/`ShapeDrivenState` fallbacks
/// cover the miss (an absent calendar degrades to "all days are
/// business days", same permissive policy as the rest of the sim).
///
/// `api_base` is the sim's API base (`direct://host` or an http
/// origin). The calendar service's URL is resolved from boss_ports for
/// the `direct://` loopback; an explicit gateway base is used verbatim.
pub async fn fetch_calendars(api_base: &str, codes: &[String]) -> Vec<BusinessCalendar> {
    let base = calendar_base_url(api_base);
    let client = ReqwestCalendarClient::new(base);
    let mut out = Vec::new();
    for code in codes {
        match client.get_business_calendar(code).await {
            Ok(Some(cal)) => {
                tracing::info!(code = %code, closed = cal.closed.len(), "fetched business calendar");
                out.push(cal);
            }
            Ok(None) => {
                tracing::warn!(
                    code = %code,
                    "business calendar absent in boss-calendar; using all-business fallback"
                );
            }
            Err(e) => {
                tracing::warn!(
                    code = %code,
                    error = %e,
                    "fetching business calendar failed; using all-business fallback"
                );
            }
        }
    }
    out
}

/// Resolve the boss-calendar service base URL from the sim's
/// `api_base`. The `direct://host` loopback marker maps to the
/// calendar service's own localhost port via boss_ports; an explicit
/// gateway base routes through that one origin.
fn calendar_base_url(api_base: &str) -> String {
    if api_base.starts_with("direct://") {
        std::env::var("BOSS_CALENDAR_URL").unwrap_or_else(|_| boss_ports::url("calendar"))
    } else {
        api_base.trim_end_matches('/').to_string()
    }
}

/// Fetch the employee roster from boss-people (`GET /api/people`),
/// returning `emp_id → role` for every employee the SYSTEM knows that has
/// a role assigned. This is the authoritative actor identity the cockpit
/// attributes workforce calls by: the sim's own seed roster
/// (`employees.json`) can lag the running system — a hire onboarded
/// mid-run, or any assignee the system holds but the seed doesn't — which
/// surfaced as `unassigned-role` in the actor panels. Sourcing the map
/// from the model means an assignee only reads as `unassigned-role` when
/// the system genuinely has no role for it.
///
/// Blocking (`reqwest::blocking`); call from the async daemon via
/// `spawn_blocking`. A fetch failure (service down, non-2xx, decode error)
/// returns an empty map and is logged — the caller keeps its seed-derived
/// fallback so telemetry attribution degrades gracefully rather than
/// blanking out.
pub fn fetch_employees(api_base: &str) -> std::collections::HashMap<String, String> {
    #[derive(serde::Deserialize)]
    struct Row {
        id: String,
        #[serde(default)]
        role: Option<String>,
    }
    let mut out = std::collections::HashMap::new();
    let url = format!("{}/api/people", people_base_url(api_base));
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "building people HTTP client failed; keeping seed roster");
            return out;
        }
    };
    // x-sim-origin marks this as simulator traffic, consistent with the
    // daemon's writes; the GET itself is an ungated read.
    match client.get(&url).header("x-sim-origin", "true").send() {
        Ok(resp) if resp.status().is_success() => match resp.json::<Vec<Row>>() {
            Ok(rows) => {
                for r in rows {
                    if let Some(role) = r.role {
                        out.insert(r.id, role);
                    }
                }
                tracing::info!(
                    count = out.len(),
                    "fetched employee roster from boss-people"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "decoding /api/people failed; keeping seed roster")
            }
        },
        Ok(resp) => {
            tracing::warn!(status = %resp.status(), url = %url, "GET /api/people non-2xx; keeping seed roster")
        }
        Err(e) => {
            tracing::warn!(error = %e, url = %url, "fetching /api/people failed; keeping seed roster")
        }
    }
    out
}

/// Resolve the boss-people service base URL from the sim's `api_base`,
/// mirroring [`calendar_base_url`].
fn people_base_url(api_base: &str) -> String {
    if api_base.starts_with("direct://") {
        std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people"))
    } else {
        api_base.trim_end_matches('/').to_string()
    }
}

/// Fetch each vendor's behavior profile from boss-inventory
/// (`GET /api/inventory/vendors`) → `(vendor_id, VendorBehavior)` for every
/// vendor that carries one (the supplier categories, bootstrapped from their
/// category Class template at seed). This is the model the simulator reads to
/// drive each vendor's supply response — [`synthesize_vendor_specs`] turns it
/// into one counterparty chain per vendor. Blocking (`reqwest::blocking`);
/// call from the async daemon via `spawn_blocking`. Empty on failure (the
/// vendor chains simply aren't synthesized, logged — the same as before).
pub fn fetch_vendors(api_base: &str) -> Vec<(String, VendorBehavior)> {
    #[derive(serde::Deserialize)]
    struct Row {
        id: String,
        #[serde(default)]
        behavior: Option<VendorBehavior>,
    }
    let mut out = Vec::new();
    let url = format!("{}/api/inventory/vendors", inventory_base_url(api_base));
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "building inventory HTTP client failed; no vendor specs");
            return out;
        }
    };
    match client.get(&url).header("x-sim-origin", "true").send() {
        Ok(resp) if resp.status().is_success() => match resp.json::<Vec<Row>>() {
            Ok(rows) => {
                for r in rows {
                    if let Some(b) = r.behavior {
                        out.push((r.id, b));
                    }
                }
                tracing::info!(
                    count = out.len(),
                    "fetched vendor behaviors from boss-inventory"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "decoding /api/inventory/vendors failed; no vendor specs")
            }
        },
        Ok(resp) => {
            tracing::warn!(status = %resp.status(), url = %url, "GET /api/inventory/vendors non-2xx; no vendor specs")
        }
        Err(e) => {
            tracing::warn!(error = %e, url = %url, "fetching /api/inventory/vendors failed; no vendor specs")
        }
    }
    out
}

/// Resolve the boss-inventory service base URL, mirroring [`people_base_url`].
fn inventory_base_url(api_base: &str) -> String {
    if api_base.starts_with("direct://") {
        std::env::var("BOSS_INVENTORY_URL").unwrap_or_else(|_| boss_ports::url("inventory"))
    } else {
        api_base.trim_end_matches('/').to_string()
    }
}

/// Build one supplier `CounterpartySpec` per vendor from its behavior — the
/// simulator parsing the model's vendor understanding into its driving
/// config. Each spec reacts to that vendor's own `step.done.procurement`
/// (matched on `subject_id`), waits the vendor's `lead_time_days ± spread`
/// (its response time), then emits `inventory.vendor_invoice_received` with
/// probability `fulfilment_rate` (a short-ship is a no-emit). That event
/// routes to the from-po endpoint, where the vendor's invoice lands
/// `received`. No AP-pay followup — payment is the existing daily
/// `inventory.bill.payment_batch` run. Replaces the inert hand-authored
/// grain/hops chains.
pub fn synthesize_vendor_specs(
    vendor_behaviors: &[(String, VendorBehavior)],
) -> Vec<CounterpartySpec> {
    vendor_behaviors
        .iter()
        .map(|(vendor_id, b)| {
            let mut match_payload = serde_json::Map::new();
            match_payload.insert(
                "subject_id".to_string(),
                serde_json::Value::String(vendor_id.clone()),
            );
            CounterpartySpec {
                name: format!("supplier-{vendor_id}"),
                actor_kind: Some("vendor".to_string()),
                listens_to: "step.done.procurement".to_string(),
                delay: DelaySpec {
                    mean_days: b.lead_time_days,
                    spread_days: b.lead_spread_days,
                    business_calendar: Some("us-banking".to_string()),
                },
                emit_probability: b.fulfilment_rate,
                emits: "inventory.vendor_invoice_received".to_string(),
                emit_else: None,
                payload: serde_json::json!({ "vendor": vendor_id }),
                followups: Vec::new(),
                scans: Vec::new(),
                match_payload,
            }
        })
        .collect()
}

/// Inline business calendars — the FALLBACK used only when the seed
/// file is absent (synthetic/minimal seed dirs in tests). Production
/// paths use DATA instead: the live daemon fetches from boss-calendar
/// ([`fetch_calendars`]); the offline regen reads the seed bundle
/// ([`calendars_from_seeds`]). Kept aligned with `for_tests` so the
/// unit suite has self-contained fixtures.
pub fn test_calendars() -> Vec<BusinessCalendar> {
    let reg = CalendarRegistry::for_tests();
    ["us-banking", "us-tax", "weekdays-only"]
        .into_iter()
        .map(|c| reg.get(Some(c)).clone())
        .collect()
}

/// Load the tenant's business calendars from the seed bundle
/// (`business_calendars.json`) — the same DATA boss-calendar is seeded
/// from, so the offline regen + in-memory runs resolve business days
/// from the single source of truth rather than a hardcoded copy. Falls
/// back to [`test_calendars`] only if the seed file is absent or
/// unparseable (minimal seed dirs in tests).
pub fn calendars_from_seeds(seeds: &Path) -> Vec<BusinessCalendar> {
    let path = seeds.join("business_calendars.json");
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<Vec<BusinessCalendar>>(&s) {
            Ok(cals) => return cals,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "business_calendars.json parse failed; using inline fallback")
            }
        },
        Err(_) => {
            tracing::warn!(path = %path.display(), "business_calendars.json absent; using inline fallback")
        }
    }
    test_calendars()
}

/// Advance the engine by exactly one tick. The `boss-brewery-sim`
/// daemon calls this in a per-tick loop with wall-clock sleeps
/// between ticks so events spread across the wall-clock window
/// instead of bursting once per sim-day.
///
/// The `engine.bus` carries cross-tick state across an entire
/// sim-day (Counterparty's last-tick drain reads it). After the
/// last tick of a sim-day (`tick_idx + 1 == ticks_per_day`),
/// callers MUST invoke [`brewery_end_of_day`] to flush per-day
/// counters + the SimOutput's per-day buffer + clear the bus.
///
/// Operating-day skip: this returns `Ok(())` without doing work
/// when the day isn't an operating day per `tenant.meta`. Daemons
/// can advance the calendar regardless; the engine just no-ops
/// for non-operating days.
pub fn run_brewery_one_tick(
    engine: &mut BreweryEngineState,
    day: NaiveDate,
    tick_idx: u32,
    output: &mut dyn SimOutput,
) -> Result<()> {
    if !engine.tenant.meta.is_operating_day(day) {
        return Ok(());
    }
    let ticks_per_day = engine.tenant.meta.ticks_per_day();
    run_one_tick_with_handlers(
        day,
        tick_idx,
        ticks_per_day,
        &engine.kinds,
        &engine.registry,
        &engine.tenant,
        &mut engine.state,
        &mut engine.rng,
        output,
        &mut engine.periodic,
        &mut engine.counterparty,
        &mut engine.bus,
        &mut engine.report,
    )
}

/// End-of-sim-day flush — companion to [`run_brewery_one_tick`].
/// Daemons MUST call this after the last tick of each sim-day to
/// drain the per-day bus + push the SimOutput's per-day buffer.
pub fn brewery_end_of_day(
    engine: &mut BreweryEngineState,
    day: NaiveDate,
    output: &mut dyn SimOutput,
) -> Result<()> {
    end_of_day_rollup(day, &mut engine.bus, output, &mut engine.report)
}

/// Run the brewery engine for `days` days starting at `start`
/// (defaults to the tenant's configured start date) into the
/// given `SimOutput`. The caller owns the output so it can be
/// `InMemoryOutput` (tests + assertion checks) or
/// `LiveApiOutput` (live-stack runs that drive audit_log via
/// the API services).
///
/// Every step-completion side effect routes through the
/// boss-dispatcher rule registry via NATS. The engine just
/// drives Job/Step lifecycle.
pub fn run_brewery_into(
    seeds: &Path,
    days: u32,
    start: Option<NaiveDate>,
    output: &mut dyn SimOutput,
) -> Result<RunReport> {
    let mut engine = BreweryEngineState::load(seeds, calendars_from_seeds(seeds))?;
    let start = start.unwrap_or(engine.tenant.meta.start_date);
    let end = start + chrono::Duration::days(days as i64 - 1);
    let ticks_per_day = engine.tenant.meta.ticks_per_day();

    run_ticks_with_handlers(
        start,
        end,
        ticks_per_day,
        &engine.kinds,
        &engine.registry,
        &engine.tenant,
        &mut engine.state,
        &mut engine.rng,
        output,
        &mut engine.periodic,
        &mut engine.counterparty,
    )
}

/// Live, clock-coordinated brewery run — the workforce model.
///
/// Unlike [`run_brewery_into`] (the in-memory test path, which sprints
/// days), this drives a live API stack the way a real deployment runs:
/// the formula clock is configured **once** and then free-runs, and the
/// sim coordinates *against* it. As the clock reaches each sim-day the
/// engine generates that day's Jobs (POSTed synchronously; the server
/// materializes their steps + emits `step.ready`), and the workforce
/// executor works open steps — claiming Ready work and completing Active
/// steps once their duration has elapsed against the clock. A trailing
/// drain lets the tail of in-flight steps + dispatcher-spawned restocks
/// settle. The 12-month seed regen uses this.
///
/// `warp_factor` is the single pacing knob (sim-seconds per wall-second;
/// wall-time ≈ sim-span ÷ warp); `poll_sleep_ms` is the wall pause
/// between workforce check-ins.
/// Start the live-api external-party callback receiver.
///
/// The dispatcher's `webhook.notify` handler POSTs `{topic, payload}` here
/// for each event an external counterparty cares about (invoice created,
/// shipment created, …). We buffer them; `run_brewery_live` drains the
/// buffer onto the bus each day so the CounterpartyEngine reacts and emits
/// its deferred response back through the public API. The simulator never
/// subscribes to the system's event stream — it only receives these
/// callbacks, preserving the sim/system boundary.
///
/// A sync `TcpListener` in its own thread: the run loop is
/// `reqwest::blocking` and must not nest a tokio runtime, and this adds no
/// dependency. Returns an empty (never-filled) buffer when
/// `BOSS_SIM_CALLBACK_BIND` is unset.
///
/// `pub` so the long-running `boss-brewery-sim` daemon (a separate bin that
/// links this crate) can start the same receiver and drain its queue onto
/// `engine.bus` each sim-day, exactly the way [`run_brewery_live`] does. The
/// engine bin reaches it transitively through `run_brewery_live`; the daemon
/// drives its own per-tick loop, so it needs the constructor directly.
pub fn start_callback_receiver() -> Arc<Mutex<VecDeque<SimBusEvent>>> {
    use std::io::{BufRead, BufReader, Read, Write};

    let buffer: Arc<Mutex<VecDeque<SimBusEvent>>> = Arc::new(Mutex::new(VecDeque::new()));
    let Ok(bind) = std::env::var("BOSS_SIM_CALLBACK_BIND") else {
        return buffer;
    };
    let listener = match std::net::TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, bind = %bind,
                "callback receiver bind failed; counterparties will stay dark");
            return buffer;
        }
    };
    tracing::info!(bind = %bind, "external-party callback receiver listening");

    let buf = buffer.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Ok(read_half) = stream.try_clone() else {
                continue;
            };
            let mut reader = BufReader::new(read_half);

            // Parse just enough HTTP: header lines until a blank line,
            // capturing Content-Length, then exactly that many body bytes.
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let t = line.trim_end();
                        if t.is_empty() {
                            break;
                        }
                        if let Some(v) = t.to_ascii_lowercase().strip_prefix("content-length:") {
                            content_length = v.trim().parse().unwrap_or(0);
                        }
                    }
                    Err(_) => break,
                }
            }
            let mut body = vec![0u8; content_length];
            if reader.read_exact(&mut body).is_ok()
                && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body)
                && let Some(topic) = v.get("topic").and_then(|t| t.as_str())
            {
                let payload = v
                    .get("payload")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                if let Ok(mut q) = buf.lock() {
                    q.push_back(SimBusEvent::new(topic, "webhook", payload));
                }
            }
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    buffer
}

/// Build the sim [`Workforce`](boss_sim::workforce::Workforce) executor
/// from an engine's StepRegistry: kind→typical-duration and
/// kind→required-at-done fields. Shared by the offline regen
/// ([`run_brewery_live`]) and the live `boss-brewery-sim` daemon so both
/// drive steps with identical pacing metadata. The caller owns clock
/// setup — the regen calls `configure_clock`; the daemon leaves the clock
/// to clock-api.
pub fn build_workforce(
    engine: &BreweryEngineState,
    api_base: &str,
) -> boss_sim::workforce::Workforce {
    use boss_sim::workforce::{RequiredField, Workforce};
    // kind → typical duration hours (drives duration-gated completion).
    // Scaled by the tenant's workforce speed multiplier (control-plane
    // knob): <1 = faster completion, >1 = slower. Absent/<=0 → 1.0.
    let speed = engine
        .tenant
        .meta
        .step_speed_multiplier
        .filter(|m| *m > 0.0)
        .unwrap_or(1.0);
    let step_durations: std::collections::HashMap<String, f64> = engine
        .registry
        .all()
        .into_iter()
        .filter_map(|t| {
            t.typical_duration_hours
                .map(|h| (t.kind.to_string(), h * speed))
        })
        .collect();
    // kind → required-at-done fields, so the worker supplies any the
    // JobKind didn't default — the executor filling the step's form.
    let required_fields: std::collections::HashMap<String, Vec<RequiredField>> = engine
        .registry
        .all()
        .into_iter()
        .map(|t| {
            let reqs = t
                .fields
                .iter()
                .filter(|f| f.required)
                .map(|f| RequiredField {
                    name: f.name.to_string(),
                    field_type: f.field_type.to_string(),
                })
                .collect();
            (t.kind.to_string(), reqs)
        })
        .collect();
    // Operator identities (real logins — role `platform-admin`, e.g.
    // the bootstrap admin) are excluded from sim driving: the
    // dispatcher may route authority-gated steps to them (a design
    // review), and those must wait for the human instead of being
    // completed by the sim at warp within seconds (the 2026-07-14
    // design-review incident). The daemon extends this set with the
    // API-discovered roster (see boss_brewery_sim.rs).
    let operators = engine
        .state
        .employees_by_role
        .get("platform-admin")
        .cloned()
        .unwrap_or_default();
    Workforce::new(api_base, step_durations, required_fields).with_excluded_assignees(operators)
}

#[allow(clippy::too_many_arguments)]
pub fn run_brewery_live(
    seeds: &Path,
    days: u32,
    start: Option<NaiveDate>,
    api_base: &str,
    warp_factor: f64,
    poll_sleep_ms: u64,
    output: &mut dyn SimOutput,
) -> Result<RunReport> {
    let mut engine = BreweryEngineState::load(seeds, calendars_from_seeds(seeds))?;
    let start = start.unwrap_or(engine.tenant.meta.start_date);
    let end = start + chrono::Duration::days(days as i64 - 1);
    let ticks_per_day = engine.tenant.meta.ticks_per_day();

    let mut workforce = build_workforce(&engine, api_base);

    // Set the clock ONCE; it free-runs from here and is never touched again.
    workforce.configure_clock(start, end, warp_factor)?;

    // External-party callbacks (the dispatcher's webhook.notify) land in this
    // buffer; we drain it onto the bus each day so the CounterpartyEngine
    // reacts to live events. Stays empty when no webhook is wired
    // (BOSS_SIM_CALLBACK_BIND unset), so non-regen runs are unaffected.
    let callbacks = start_callback_receiver();

    // Wall-clock anchor so the end-of-run summary can report the effective
    // workforce pass rate (checkins / elapsed) — the key throughput signal.
    let run_started = std::time::Instant::now();
    let poll = std::time::Duration::from_millis(poll_sleep_ms);
    let mut day = start;
    while day <= end {
        // Pace generation to the clock: wait until it reaches `day`,
        // driving in-flight work while we wait.
        loop {
            let (now, _) = workforce.clock_now()?;
            if now.date_naive() >= day {
                break;
            }
            workforce.work_once()?;
            std::thread::sleep(poll);
        }
        if engine.tenant.meta.is_operating_day(day) {
            output.start_of_day(day)?;
            // Drain external-party callbacks onto the bus before the
            // CounterpartyEngine runs this tick, so it reacts to the live
            // events the dispatcher forwarded since the last day.
            if let Ok(mut q) = callbacks.lock() {
                for ev in q.drain(..) {
                    engine.bus.emit(ev);
                }
            }
            for tick_idx in 0..ticks_per_day {
                run_one_tick_with_handlers(
                    day,
                    tick_idx,
                    ticks_per_day,
                    &engine.kinds,
                    &engine.registry,
                    &engine.tenant,
                    &mut engine.state,
                    &mut engine.rng,
                    output,
                    &mut engine.periodic,
                    &mut engine.counterparty,
                    &mut engine.bus,
                    &mut engine.report,
                )?;
            }
            end_of_day_rollup(day, &mut engine.bus, output, &mut engine.report)?;
        }
        workforce.work_once()?;
        day = day.succ_opt().expect("date sequence overflow");
    }

    let dayloop_checkins = workforce.stats.checkins;
    // Drain: keep driving until the workforce goes idle for several
    // consecutive rounds (the tail of in-flight steps + dispatcher-spawned
    // restocks settle), so the run doesn't end mid-pipeline. Capped so a
    // perpetually-churning pipeline can't hang the run.
    let mut idle = 0;
    let mut rounds = 0;
    while idle < 5 && rounds < 5_000 {
        let before = (workforce.stats.claimed, workforce.stats.completed);
        workforce.work_once()?;
        if (workforce.stats.claimed, workforce.stats.completed) == before {
            idle += 1;
        } else {
            idle = 0;
        }
        std::thread::sleep(poll);
        rounds += 1;
    }

    let s = &workforce.stats;
    let elapsed = run_started.elapsed().as_secs_f64();
    tracing::info!(
        checkins = s.checkins,
        dayloop_checkins,
        drain_rounds = rounds,
        claimed = s.claimed,
        completed = s.completed,
        deferred = s.deferred,
        in_progress = s.in_progress,
        errors = s.errors,
        elapsed_secs = elapsed,
        passes_per_sec = if elapsed > 0.0 {
            s.checkins as f64 / elapsed
        } else {
            0.0
        },
        "workforce run complete",
    );

    output.flush()?;
    engine.report.jobs_created = engine.state.counters.jobs_created;
    engine.report.counterparty_pending = engine.counterparty.pending() as u64;
    Ok(engine.report)
}

/// Run the brewery engine for `days` days into a fresh
/// `InMemoryOutput`. Tests + the CLI's default mode use this.
pub fn run_brewery(seeds: &Path, days: u32, start: Option<NaiveDate>) -> Result<BreweryRunResult> {
    let mut output = InMemoryOutput::default();
    let report = run_brewery_into(seeds, days, start, &mut output)?;
    Ok(BreweryRunResult { report, output })
}

#[cfg(test)]
mod synth_tests {
    use super::*;
    use boss_inventory::types::{BehaviorProvenance, BehaviorSource};

    fn behavior(lead: f64, fulfil: f64) -> VendorBehavior {
        VendorBehavior {
            lead_time_days: lead,
            lead_spread_days: 1.0,
            fulfilment_rate: fulfil,
            ap_payment_days: 5.0,
            ap_spread_days: 2.0,
            provenance: BehaviorProvenance {
                source: BehaviorSource::HandSet,
                template: Some("grain-supplier".into()),
            },
        }
    }

    #[test]
    fn synthesizes_one_supplier_spec_per_vendor_keyed_to_real_id() {
        let specs = synthesize_vendor_specs(&[
            ("vnd-bigseed-001".to_string(), behavior(3.0, 0.98)),
            ("vnd-bigseed-002".to_string(), behavior(1.5, 0.99)),
        ]);
        assert_eq!(specs.len(), 2);
        let s = &specs[0];
        assert_eq!(s.name, "supplier-vnd-bigseed-001");
        assert_eq!(s.listens_to, "step.done.procurement");
        assert_eq!(s.emits, "inventory.vendor_invoice_received");
        assert_eq!(s.actor_kind.as_deref(), Some("vendor"));
        assert_eq!(s.emit_probability, 0.98); // fulfilment_rate
        assert_eq!(s.delay.mean_days, 3.0); // lead_time_days
        // Keyed to the REAL vendor id so only this vendor's procurement
        // fires it — the fix for the inert placeholder-id chains.
        assert_eq!(
            s.match_payload.get("subject_id").and_then(|v| v.as_str()),
            Some("vnd-bigseed-001")
        );
        // No followup — AP is the existing daily batch-pay run.
        assert!(s.followups.is_empty());
    }

    #[test]
    fn synthesize_empty_when_no_behaviors() {
        assert!(synthesize_vendor_specs(&[]).is_empty());
    }
}

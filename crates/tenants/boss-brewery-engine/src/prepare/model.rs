//! [`prepare_model`] — the brewery's single tenant-prepare entry
//! point. Against a running BOSS stack — each service on its own port
//! by default, or all behind one gateway URL — it seeds the entire
//! brewery tenant model through the public API, in dependency order:
//!
//! 1. classes — POST /api/classes/batch (the taxonomy that employee +
//!    account writes validate against, so it lands first), then the
//!    departments — POST /api/departments/batch (what an employee's
//!    `department` validates against; each row's `function` is a Class).
//! 2. policy — tenant role grants ([`boss_policy::bootstrap`]); these are
//!    capability-level (`resource = "workflow"`, not a specific kind), so
//!    they need no published Workflows, and the design-Job approval in
//!    step 4 needs the `workflow-approver` grant resolved first.
//! 3. data — operators, employees, accounts, vendors, messages,
//!    finished-goods, raw materials, equipment, assets, and opening
//!    balances ([`super::seed_tenant_data`]).
//! 4. Workflows LAST ([`super::publish_workflows`]) — each one opens a real
//!    `workflow-design` Job with role-bearing `approve` + `publish` steps
//!    that the dispatcher auto-assigns the moment they go ready. Their
//!    holders (the `workflow-approver`-granted leaders, it-director,
//!    platform-admin) must already be seeded AND queryable, or the
//!    assignment dead-letters against a cold roster — so we barrier on the
//!    people projection before opening them.
//!
//! This collapses what reset-to-baseline / seed-brewery-tenant.sh
//! drove as four scattered binary + curl steps into one library call,
//! so the offline regen, the live demo, and CI all run identical code.
//!
//! Two adjacent concerns stay with the caller by design:
//!
//! - **Clock loop-bounds.** [`super::seed_tenant_data`] rebases the
//!   clock to the sim epoch internally (the pre-sim provisioning
//!   window) so seed writes stamp at day 0; the epoch_end + warp_factor
//!   that bound the sim's run are set by whoever drives the clock
//!   (reset-to-baseline / the sim daemon), not here.
//! - **Platform baseline.** The platform operator-baseline
//!   (`bootstrap-admin`, minted from a deploy credentials file) and
//!   the projection rebuilds are platform-init / read-model concerns —
//!   not tenant data, and not API-orchestratable — so they remain in
//!   the deploy/reset orchestration.

use std::path::Path;

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use tracing::info;

use super::{SeedBases, publish_workflows, seed_tenant_data};

/// Seed the entire brewery tenant model through the public API.
///
/// `gateway_base` selects the routing: `None` sends each service to
/// its own localhost port (`boss_ports` defaults) — the reset /
/// quickstart path, which seeds with the gateway stopped. `Some(url)`
/// routes every `/api/*` prefix through one gateway URL (a deployment
/// whose gateway is up during seeding). `seeds_dir` is the brewery
/// seed bundle (`examples/brewery/seeds`). Idempotent throughout —
/// safe to re-run.
pub fn prepare_model(gateway_base: Option<&str>, seeds_dir: &Path) -> Result<()> {
    // classes / jobs / policy each take a single base; the tenant-data
    // seeder takes the full per-service map. A gateway override points
    // all of them at one URL; otherwise each resolves to its own port.
    let classes_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("classes"));
    let calendar_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("calendar"));
    let jobs_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("jobs"));
    let policy_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("policy"));
    let people_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("people"));
    let data_bases = match gateway_base {
        Some(g) => SeedBases::all(g),
        None => SeedBases::from_ports(),
    };

    info!(
        gateway = ?gateway_base,
        seeds = %seeds_dir.display(),
        "preparing brewery tenant model"
    );

    // 1. Classes first — employee role + account-type writes validate
    //    against the Class registry.
    seed_classes(&classes_base, seeds_dir)?;

    // 1a. Departments — an employee's `department` validates against
    //     the departments registry (c87e3d6d), so the tenant's roster
    //     of departments lands before any employee; after the classes,
    //     because each row's `function` is a Class (backlog e22ee67a).
    seed_departments(&jobs_base, seeds_dir)?;

    // 1b. Business calendars — reference data (banking/tax holidays) the
    //     dispatcher's timing triggers and the simulator resolve business
    //     days from. Like classes: load before anything that consumes them.
    seed_business_calendars(&calendar_base, seeds_dir)?;

    // 1c. The tenant's own identity — Q6: the organization being
    //     modeled is itself a Subject, one row per tenant. The
    //     org-level Workflows (payroll, tax filings, AP runs, facility
    //     overhead, the production heartbeat) open their Jobs about
    //     it, so the identity must exist before the periodic engine
    //     opens the first one. Hard-fail: a missing company identity
    //     starves every org-level Job at the existence gate.
    let subjects_base = gateway_base
        .map(str::to_string)
        .unwrap_or_else(|| boss_ports::url("subject-kinds"));
    let tenant = boss_sim::shape_driven::TenantConfig::load(&seeds_dir.join("tenant.toml"))
        .context("loading tenant.toml for the company identity")?;
    crate::mint_subject_identity(
        "company",
        &tenant.meta.tenant_id,
        Some(&tenant.meta.display_name),
        &subjects_base,
    )
    .map_err(|e| anyhow::anyhow!("minting the company identity: {e}"))?;
    info!(company = %tenant.meta.tenant_id, "company identity minted");

    // 2. Tenant policy grants — core ships only platform rules; the
    //    brewery org chart's row-level access matrix arrives here. These
    //    grants are capability-level (`resource = "workflow"`, not a
    //    specific published kind), so they don't depend on the Workflow
    //    registry being populated — and the design-Job approval in step 4
    //    needs the `workflow-approver` grant to resolve to its
    //    operational-leader holders.
    boss_policy::bootstrap::publish_policy_rules(
        &policy_base,
        &seeds_dir.join("policy_rules.toml"),
        false,
        None,
    )?;

    // 3. Tenant data — operators, employees, accounts, vendors,
    //    messages, finished-goods, raw materials, equipment, assets,
    //    + opening balances. None of it opens Jobs (so it needs no
    //    Workflows yet); it DOES seed the workforce step 4 depends on.
    seed_tenant_data(&data_bases, seeds_dir, None)?;

    // 4. Workflows LAST — publishing each brewery Workflow opens a real
    //    `workflow-design` Job whose `approve` (sign-off, authority
    //    `workflow-approver`) and `publish` (it-director / platform-admin)
    //    steps are role-bearing. The dispatcher auto-assigns role-bearing
    //    steps the instant they go ready, so those holders must already be
    //    seeded AND queryable — otherwise the assignment NAKs against an
    //    empty roster and dead-letters (the prepare flow still completes
    //    the steps directly, but the dispatcher's parallel attempt is what
    //    exhausts its redelivery budget). Barrier on the people projection
    //    first, then open the design Jobs. dev=true auto-walks the sign-off
    //    (unattended seed, same as `boss-brewery-bootstrap --dev`);
    //    publish_workflows takes the workflows.toml FILE (not the dir).
    wait_for_people_projection(&people_base)?;
    publish_workflows(
        &jobs_base,
        &seeds_dir.join("workflows.toml"),
        "brewery-bootstrap",
        true,
        false,
        None,
    )?;

    info!("brewery tenant model prepared");
    Ok(())
}

/// Block until the people read-model reflects the just-seeded workforce,
/// so the role-bearing steps opened immediately after (the
/// `workflow-design` Jobs) can be assigned to real holders instead of
/// dead-lettering against a cold roster. Polls `/api/people` until the
/// count is non-trivial and stable across three reads (the hire backlog
/// has drained), or 90s elapse. Mirrors the sim-readiness barrier in
/// `validate-brewery-sim.sh`, but inside the shared prepare path so every
/// caller (offline regen, live demo, CI) gets it. Best-effort: a timeout
/// logs and proceeds rather than aborting the seed.
fn wait_for_people_projection(people_base: &str) -> Result<()> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let url = format!("{}/api/people", people_base.trim_end_matches('/'));
    let (mut prev, mut stable) = (0usize, 0u32);
    for _ in 0..90 {
        // Signed: the roster answers a caller by grant, and one with no
        // identity is refused (backlog cda177ef) — which this loop would
        // read as an empty roster for all 90 seconds.
        let count = client
            .get(&url)
            .header(
                "x-boss-user",
                r#"{"id":"automation:brewery-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#,
            )
            .send()
            .ok()
            .and_then(|r| r.json::<serde_json::Value>().ok())
            .and_then(|v| v.as_array().map(|a| a.len()))
            .unwrap_or(0);
        if count > 100 && count == prev {
            stable += 1;
            if stable >= 3 {
                info!(people = count, "roster ready — opening design Jobs");
                return Ok(());
            }
        } else {
            stable = 0;
        }
        prev = count;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    info!(
        people = prev,
        "people projection did not stabilize in 90s; opening design Jobs anyway"
    );
    Ok(())
}

/// POST the brewery's Class registry (`seeds/classes.json`) to
/// `/api/classes/batch`. Classes are the taxonomy employee + account
/// writes validate against, so they land before any of those.
///
/// `x-sim-origin: true` lets the batch land as seed-origin data
/// (matching the reset-to-baseline curl); the seed-loader identity
/// carries platform-admin provenance.
fn seed_classes(api_base: &str, seeds_dir: &Path) -> Result<()> {
    let path = seeds_dir.join("classes.json");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let url = format!("{}/api/classes/batch", api_base.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("x-sim-origin", "true")
        .header(
            "x-boss-user",
            r#"{"id":"automation:classes-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#,
        )
        .body(body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("POST {url} → {status} {}", resp.text().unwrap_or_default());
    }
    info!(path = %path.display(), "brewery classes seeded");
    Ok(())
}

/// POST the brewery's business calendars (`seeds/business_calendars.json`)
/// to `/api/calendar/business-calendars/batch`. These are the banking +
/// tax calendars the dispatcher's timing triggers and the simulator
/// resolve business days from — DATA, not hardcoded Rust. Same
/// seed-origin provenance as the Class registry.
fn seed_business_calendars(api_base: &str, seeds_dir: &Path) -> Result<()> {
    let path = seeds_dir.join("business_calendars.json");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let url = format!(
        "{}/api/calendar/business-calendars/batch",
        api_base.trim_end_matches('/')
    );
    let resp = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("x-sim-origin", "true")
        .header(
            "x-boss-user",
            r#"{"id":"automation:calendar-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#,
        )
        .body(body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("POST {url} → {status} {}", resp.text().unwrap_or_default());
    }
    info!(path = %path.display(), "brewery business calendars seeded");
    Ok(())
}

/// POST the tenant's departments (`seeds/departments.toml`) to
/// `/api/departments/batch` — the door `boss tenant publish` uses,
/// insert-if-absent by code (backlog e22ee67a). An employee's
/// `department` is validated against this registry since the Class
/// collapse (c87e3d6d), and `taproom` and `packaging` are declared
/// ONLY here — no migration seeds them — so the prepare that seeds
/// the employees must land them first. Each row's `function` is a
/// Class under `(department, function)`, so this runs after the
/// classes.
fn seed_departments(api_base: &str, seeds_dir: &Path) -> Result<()> {
    let path = seeds_dir.join("departments.toml");
    let rows = boss_jobs::department::declare::load_departments_toml(&path)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let url = format!("{}/api/departments/batch", api_base.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("x-sim-origin", "true")
        .header(
            "x-boss-user",
            r#"{"id":"automation:departments-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#,
        )
        .json(&rows)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("POST {url} → {status} {}", resp.text().unwrap_or_default());
    }
    info!(path = %path.display(), departments = rows.len(), "brewery departments seeded");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// One HTTP exchange: answer `status` to the first request and hand
    /// back its request line and body. A stand-in for the jobs API's
    /// batch door, so the test reads what the engine SENT rather than
    /// what its source says it sends.
    fn one_request_answering(status: &'static str) -> (String, mpsc::Receiver<(String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let base = format!("http://{}", listener.local_addr().expect("its address"));
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the engine connects");
            let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
            let mut request_line = String::new();
            reader.read_line(&mut request_line).expect("a request line");
            let mut length = 0usize;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).expect("a header line");
                if header.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().expect("a numeric length");
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).expect("the body");
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .expect("the answer");
            tx.send((
                request_line.trim().to_string(),
                String::from_utf8(body).expect("a UTF-8 body"),
            ))
            .expect("the test is listening");
        });
        (base, rx)
    }

    fn brewery_seeds() -> std::path::PathBuf {
        boss_testing::repo_root().join("examples/brewery/seeds")
    }

    /// Backlog e22ee67a: the prepare seeded classes, policy and
    /// employees but never the tenant's departments, so an instance
    /// prepared by the engine alone had 100 employees in `taproom` and
    /// `packaging`, which no migration seeds.
    #[test]
    fn the_prepare_publishes_every_declared_department_through_the_batch_door() {
        let (base, rx) = one_request_answering("200 OK");
        seed_departments(&base, &brewery_seeds()).expect("a 200 is a publish");
        let (request_line, body) = rx.recv().expect("the engine sent a request");
        assert!(
            request_line.starts_with("POST /api/departments/batch "),
            "the departments door, insert-if-absent by code: {request_line}"
        );
        let sent: Vec<boss_jobs::department::declare::DepartmentInput> =
            serde_json::from_str(&body).expect("the body is the door's row shape");
        let declared = boss_jobs::department::declare::load_departments_toml(
            &brewery_seeds().join("departments.toml"),
        )
        .expect("the brewery's departments.toml loads");
        assert_eq!(sent, declared, "every declared row, as declared");
        for code in ["taproom", "packaging"] {
            assert!(
                sent.iter().any(|d| d.code == code),
                "{code}: a department only this file declares"
            );
        }
    }

    #[test]
    fn a_refused_publish_stops_the_prepare_naming_the_door() {
        let (base, _rx) = one_request_answering("422 Unprocessable Entity");
        let err = seed_departments(&base, &brewery_seeds())
            .expect_err("a refusal is not a publish")
            .to_string();
        assert!(
            err.contains("/api/departments/batch") && err.contains("422"),
            "the error names the door and the answer: {err}"
        );
    }
}

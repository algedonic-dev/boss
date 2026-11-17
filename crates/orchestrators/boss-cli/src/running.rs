//! `boss running` — merged, deployed and running are three facts.
//!
//! WHY THIS IS A VERB. The second of the three questions backlog-item
//! 26b3d203 names as hand-rolled every session: "which release is
//! deployed and does it carry this change?" On 2026-08-29 alone it was
//! written fresh at least six times, and it went wrong in both
//! directions:
//!
//!   - A migration was present in the deployed commit while the live
//!     registry still answered the OLD value, because the train had
//!     DEPLOYED but not CONVERGED and convergence is what runs
//!     migrate.sh. A source-level check would have recorded a true
//!     statement about the tree beside a false one about production.
//!   - A StepType was merged AND deployed while the API still reported
//!     45 kinds, because the registry is built from a TOML embedded in
//!     the binary at startup and the pods had not rolled.
//!   - A CI image was nearly reported stale off the wrong docker
//!     daemon — the user-level one rather than the rootful one the
//!     runner actually uses.
//!
//! THE DISTINCTION IS THE POINT. Three different facts get called
//! "deployed" in conversation:
//!
//!   MERGED    the forge's main carries it            (`boss merged`)
//!   DEPLOYED  the latest cluster converge records its applied generation
//!   RUNNING   the process serving requests was built from it
//!
//! They are routinely different, and the gap between the last two is
//! where a proof records something true about a tree and false about
//! production. This verb reports all three side by side so the gap is
//! visible rather than assumed.

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::time::Duration;

/// What the three layers say. `None` everywhere means "could not read",
/// never "absent" — the same fail-closed rule the gate's concurrency
/// guard and `boss merged` both use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Generations {
    /// The forge's main, via ls-remote. The merge authority.
    pub main: Option<String>,
    /// The latest cluster converge's rolled or unchanged image generation.
    pub deployed: Option<String>,
    /// The commit the live jobs API reports serving, from
    /// `capabilities.commit`. This is the only one that describes a
    /// PROCESS rather than a recorded deployment.
    pub running: Option<String>,
}

/// How far apart the layers are, in words a reader can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alignment {
    /// All three agree — the ordinary steady state.
    Aligned,
    /// The deployment record and serving build differ. The pods may not
    /// have rolled; anything built into the binary (StepType registry,
    /// embedded seeds) is still the OLD value even though the file is
    /// on disk.
    NotRolled,
    /// The deploy host is behind main. A merge has not reached it.
    NotDeployed,
    /// Something could not be read. Never reported as agreement.
    Unknown(String),
}

fn short(s: &Option<String>) -> &str {
    s.as_deref().map_or("UNREADABLE", |v| {
        // Callers compare full shas; humans read twelve characters.
        if v.len() >= 12 { &v[..12] } else { v }
    })
}

/// Do two revisions name the same commit?
///
/// NOT `==`. The three layers report the same commit at different
/// lengths: a release directory is named with an abbreviated sha
/// (`releases/3139df18`) while the API reports the full forty. Comparing
/// them directly reports NOT ROLLED forever, which is the failure this
/// verb exists to detect — so it would have lied in exactly its own
/// subject matter.
///
/// Caught by running it against the live cluster, not by the unit tests,
/// which all used equal-length fixtures. That is why this one has a test
/// with mismatched lengths.
fn same_commit(a: &str, b: &str) -> bool {
    let n = a.len().min(b.len());
    // Seven is git's own floor for an unambiguous abbreviation.
    n >= 7 && a[..n].eq_ignore_ascii_case(&b[..n])
}

/// The decision, pure.
///
/// Deliberately reports NotRolled BEFORE NotDeployed: when both are
/// true the process is the binding constraint, and it is the one people
/// forget. A packet whose change is on disk but not in the running
/// process is the exact shape that made a proof refuse today.
pub fn align(g: &Generations) -> Alignment {
    let (Some(main), Some(dep)) = (&g.main, &g.deployed) else {
        return Alignment::Unknown(
            "could not read main or the latest cluster converge record — an unread layer is \
             not an aligned one"
                .into(),
        );
    };
    let Some(run) = &g.running else {
        return Alignment::Unknown(
            "could not read the running commit from the live API. The process \
             is the only layer that describes what is actually serving; \
             without it, deployed is not evidence"
                .into(),
        );
    };
    if !same_commit(dep, run) {
        return Alignment::NotRolled;
    }
    if !same_commit(dep, main) {
        return Alignment::NotDeployed;
    }
    Alignment::Aligned
}

fn commit(value: &Value) -> Result<String> {
    let text = value.as_str().context("missing commit")?;
    ensure!(
        (7..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid commit {text:?}"
    );
    Ok(text.to_owned())
}

struct Deployment {
    head: String,
    packet: String,
    at: String,
}

// Ask for the latest record, never a successful older one: an open or failed
// attempt without deployment evidence must not inherit a previous green.
fn deployed_from_record(body: &Value) -> Result<Deployment> {
    ensure!(
        body["full"] == true && body["limit"] == 1 && body["offset"] == 0,
        "latest converge read did not return a full first-row envelope"
    );
    let rows = body["data"].as_array().context("converge rows missing")?;
    ensure!(
        rows.len() == 1 && body["total"].as_u64().is_some_and(|n| n >= 1),
        "latest converge record is absent or ambiguous"
    );
    let job = &rows[0];
    ensure!(
        job["kind"] == "maintenance-cluster-converge",
        "latest record is not a cluster converge"
    );
    ensure!(
        matches!(job["status"].as_str(), Some("open" | "closed")),
        "invalid converge packet status"
    );
    let packet = job["id"].as_str().context("converge packet id missing")?;
    uuid::Uuid::parse_str(packet).context("invalid converge packet id")?;
    let opened = job["opened_at"]
        .as_str()
        .context("converge opening time missing")?;
    let opened_at =
        chrono::DateTime::parse_from_rfc3339(opened).context("invalid converge opening time")?;
    let steps = job["steps"].as_array().context("converge steps missing")?;
    let mut runs = steps.iter().filter(|s| s["spec_slug"] == "run");
    let run = runs.next().context("converge run step missing")?;
    ensure!(runs.next().is_none(), "ambiguous converge run step");
    let metadata = &run["metadata"];
    let (head, at) = if metadata.get("rolled").is_some() {
        ensure!(
            matches!(run["status"].as_str(), Some("active" | "completed")),
            "converge roll is not recorded on an active or completed run"
        );
        let head = commit(&metadata["rolled"])?;
        let at = metadata["rolled_at"]
            .as_str()
            .context("converge roll time missing")?;
        ensure!(
            chrono::DateTime::parse_from_rfc3339(at).context("invalid converge roll time")?
                >= opened_at,
            "converge roll predates its packet"
        );
        (head, at)
    } else {
        ensure!(
            job["status"] == "closed" && run["status"] == "completed" && metadata["result"] == "ok",
            "latest converge has not recorded a roll or a successful unchanged check"
        );
        (commit(&metadata["unchanged"])?, opened)
    };
    Ok(Deployment {
        head,
        packet: packet.to_owned(),
        at: at.to_owned(),
    })
}

async fn merged_commit(clone: &str, remote: &str) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new("git")
            .args(["-C", clone, "ls-remote", remote, "refs/heads/main"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("git ls-remote timed out after 20s")??;
    ensure!(
        output.status.success(),
        "git ls-remote failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let head =
        crate::receipt::exact_ref_head(&String::from_utf8_lossy(&output.stdout), "refs/heads/main")
            .context("git ls-remote did not name exactly refs/heads/main")?;
    commit(&Value::String(head))
}

async fn read_json(
    http: &crate::machine_http::MachineClient,
    base: &str,
    path: &str,
) -> Result<Value> {
    crate::sor_client::api_at(http, base, reqwest::Method::GET, path, None)
        .await?
        .context("read returned no JSON body")
}

pub async fn run(
    clone: Option<String>,
    remote: Option<String>,
    jobs_url: Option<String>,
) -> Result<()> {
    let clone = clone.unwrap_or_else(|| ".".to_string());
    let remote = remote.unwrap_or_else(|| "origin".to_string());
    // No default (backlog 6cf47547): the literal fallback here was the
    // last CLI read the address lint still allowed, and a default that
    // is right on one host and silently wrong on another is the defect
    // `resolve_jobs_base` exists to refuse (aa783636) — so `running`
    // resolves the way every other read verb does: flag, env, refuse.
    let jobs_url = crate::sor_client::resolve_jobs_base(jobs_url.as_deref())?;
    let jobs_url = jobs_url.trim_end_matches('/');

    let http = crate::sor_client::machine_client_with(
        reqwest::Client::builder().timeout(Duration::from_secs(20)),
    )?;
    let (main, deployment, running) = tokio::join!(
        merged_commit(&clone, &remote),
        read_json(
            &http,
            jobs_url,
            "/api/jobs?kind=maintenance-cluster-converge&full=true&limit=1"
        ),
        read_json(&http, jobs_url, "/api/jobs/health"),
    );
    let deployment = deployment.and_then(|body| deployed_from_record(&body));
    let running = running.and_then(|body| commit(&body["capabilities"]["commit"]));
    let g = Generations {
        main: main.as_ref().ok().cloned(),
        deployed: deployment.as_ref().ok().map(|d| d.head.clone()),
        running: running.as_ref().ok().cloned(),
    };

    println!("boss running:");
    println!("  merged   main({remote})  {}", short(&g.main));
    println!("  deployed cluster record  {}", short(&g.deployed));
    println!("  running  jobs api        {}", short(&g.running));
    if let Ok(record) = &deployment {
        println!("  deployment record {} at {}", record.packet, record.at);
    }
    if let Err(error) = main {
        println!("  merged unread: clone {clone:?}, remote {remote:?}: {error:#}");
    }
    if let Err(error) = deployment {
        println!("  cluster converge unread: {error:#}");
    }
    if let Err(error) = running {
        println!("  running unread at {jobs_url}/api/jobs/health: {error:#}");
    }
    match align(&g) {
        Alignment::Aligned => {
            println!("  ALIGNED — main, recorded cluster generation and serving build agree")
        }
        Alignment::NotRolled => println!(
            "  NOT ROLLED — the recorded cluster generation and serving build differ. \n\
             \x20   Read the deployment record and live health before proving this change."
        ),
        Alignment::NotDeployed => {
            println!("  NOT DEPLOYED — the recorded cluster generation differs from main.")
        }
        Alignment::Unknown(why) => println!("  UNKNOWN — {why}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixtures are FULL-LENGTH on purpose. The first version of these
    /// tests used three-character shas, which is below git's seven-char
    /// floor for an unambiguous abbreviation — and that unrealism is
    /// exactly what let a real bug through: the live layers report the
    /// same commit at different lengths, and equality called the steady
    /// state NOT ROLLED. A fixture that cannot exhibit the failure
    /// cannot catch it.
    fn sha(seed: &str) -> String {
        format!("{seed:0<40}")
    }

    fn g(main: &str, dep: &str, run: &str) -> Generations {
        Generations {
            main: Some(sha(main)),
            deployed: Some(sha(dep)),
            running: Some(sha(run)),
        }
    }

    /// THE BUG THE FIXTURES HID. The layers report the same commit at
    /// different lengths — a release directory carries an abbreviated
    /// sha, the API reports the full one. Equality would call the
    /// steady state NOT ROLLED, so the verb would have been wrong about
    /// precisely the thing it exists to detect.
    #[test]
    fn a_short_release_name_matches_a_full_sha() {
        let live = Generations {
            main: Some("3139df18f68e71306a4a08f47d74afbf5e3ad09d".into()),
            deployed: Some("3139df18".into()),
            running: Some("3139df18f68e71306a4a08f47d74afbf5e3ad09d".into()),
        };
        assert_eq!(align(&live), Alignment::Aligned);
    }

    /// ...and an abbreviation that is too short to be unambiguous is not
    /// a match. Git's own floor is seven.
    #[test]
    fn an_ambiguous_abbreviation_is_not_a_match() {
        assert!(!same_commit("313", "3139df18f68e"));
        assert!(same_commit("3139df1", "3139df18f68e"));
    }

    #[test]
    fn all_three_agreeing_is_the_only_aligned_state() {
        assert_eq!(
            align(&g("aaaaaaa", "aaaaaaa", "aaaaaaa")),
            Alignment::Aligned
        );
    }

    /// THE CASE THAT MADE A PROOF REFUSE TODAY. The migration was in the
    /// deployed commit and the live registry still answered the old
    /// value, because the pods had not rolled. Deployed is a filesystem
    /// fact; running is a process fact.
    #[test]
    fn on_disk_but_not_in_the_process_is_not_rolled() {
        assert_eq!(
            align(&g("aaaaaaa", "aaaaaaa", "bbbbbbb")),
            Alignment::NotRolled
        );
    }

    /// And when BOTH are behind, the process is still the binding
    /// constraint — reporting NotDeployed first would send a reader to
    /// the deploy when the roll is what they are waiting on.
    #[test]
    fn the_process_is_reported_before_the_deploy() {
        assert_eq!(
            align(&g("ccccccc", "ddddddd", "eeeeeee")),
            Alignment::NotRolled
        );
    }

    #[test]
    fn a_merge_that_has_not_reached_the_host_is_not_deployed() {
        assert_eq!(
            align(&g("ccccccc", "ddddddd", "ddddddd")),
            Alignment::NotDeployed
        );
    }

    /// An unread layer is never an aligned one — the same fail-closed
    /// rule as the gate's concurrency guard and `boss merged`.
    #[test]
    fn an_unread_layer_is_never_agreement() {
        for miss in [
            Generations {
                main: None,
                ..g("aaaaaaa", "aaaaaaa", "aaaaaaa")
            },
            Generations {
                deployed: None,
                ..g("aaaaaaa", "aaaaaaa", "aaaaaaa")
            },
            Generations {
                running: None,
                ..g("aaaaaaa", "aaaaaaa", "aaaaaaa")
            },
        ] {
            assert!(
                matches!(align(&miss), Alignment::Unknown(_)),
                "a missing layer must not read as aligned: {miss:?}"
            );
        }
    }
}

/// Top-level registration for `boss running` — see `merged::Cmd` for
/// why the variant lives in its verb's module (84f9fbc0).
#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Merged, deployed and running are three different facts.
    ///
    /// The gap between the last two is where a proof records something
    /// true about a tree and false about production (26b3d203).
    Running {
        /// Clone to read main from. Defaults to the working directory.
        #[arg(long)]
        clone: Option<String>,
        /// Remote that receives trains — the merge authority.
        #[arg(long)]
        remote: Option<String>,
        /// Jobs API to ask what is actually serving.
        #[arg(long)]
        jobs_url: Option<String>,
    },
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Running {
            clone,
            remote,
            jobs_url,
        } => run(clone, remote, jobs_url).await,
    }
}

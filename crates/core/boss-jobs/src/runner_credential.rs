//! The credential door: the jobs API knows the ops runner by a
//! credential it PRESENTS, never by the id it types (design f623e425 Q1,
//! option A, decided by David 2026-09-25; backlog 6c9183de).
//!
//! WHY. A key a human signs has one declared writer
//! ([`crate::field_writer`]), and that writer is believed only through a
//! [`CredentialedCaller`] request extension — which nothing inserted, so
//! no caller could satisfy a declared writer and no protocol declares
//! one. The runner's identity on the wire was `x-boss-user:
//! {"id":"automation:ops-runner",…}`, built in infra/ops/ops-runner.sh,
//! plus the machine token every agent's door, the conductor and the
//! dispatcher also hold. A writer rule keyed on that id is design option
//! D, rejected: it refuses the well-behaved and admits the forger the
//! review named. This module is the "resolve step in the machine door"
//! the design costs option A at.
//!
//! WHAT IT RESOLVES. A request carrying [`HEADER`] has the value
//! compared, in constant time, against every slot in the mounted slot
//! directory ([`DIR_ENV`], default [`DEFAULT_DIR`]): one file per host
//! and slot, `<host>.<slot>`, with the machine token's three slots —
//! `current`, `next`, `previous` — so a rotation can stage the new value
//! before the host holds it and keep the old one until the host has
//! moved on (design 6805c764's shape). A value exactly one host's slots
//! hold inserts `CredentialedCaller { principal: runner:ops, actor_id:
//! automation:ops-runner, host: Some(<host>) }`; the host binding is then
//! [`crate::field_writer::admits`]'s. A value two hosts hold resolves to
//! neither — a broker fault is read as no credential, never as whichever
//! host a directory listing reached first.
//!
//! IT NEVER REFUSES (DR rule 62dac114: no refusal on David's path until
//! one-person DR is proven). No header, a value no slot holds, an
//! unreadable directory, an ambiguous value — each passes the request on
//! with no credential, exactly as every request passed before this door
//! existed; only a write to a key a protocol reserves can then be
//! refused, by the writer rule, and no live protocol declares one. The
//! header is REMOVED before the request goes on, so no handler, log or
//! proxy downstream ever holds the value.
//!
//! WHO FILLS THE SLOTS. The credential broker, from the next car of the
//! design: a `credential.rotate.*` handler that mints the value, writes it
//! into the Secret this directory is the mount of, verifies it by effect
//! through [`WHOAMI_PATH`], and delivers it to the host's runner file.
//! Until that Secret is mounted there is no directory, every request
//! reads as uncredentialed, and nothing any caller sees changes — the
//! machine token's deploy-order posture. So `runner:ops` is NOT yet in
//! [`crate::field_writer::RESOLVABLE_PRINCIPALS`]: a door with no
//! credential behind it resolves nobody, and a protocol that declared the
//! writer now would lock the runner out of its own plan — the approval
//! David signs would never be offered. The principal enters that list in
//! the car that delivers a credential the runner can present.
//!
//! NO VALUE LEAVES THIS MODULE. Not in a log line, an error, the whoami
//! answer or a hash of one; a slot is named by its file name only.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::get;
use boss_core::machine_token;
use serde_json::{Value, json};

use crate::field_writer::CredentialedCaller;

/// The header a runner presents its credential in. An `x-boss-` name on
/// purpose: the gateway strips every inbound `x-boss-*` header at the
/// edge, so a browser can never carry one to this door.
pub const HEADER: &str = "x-boss-runner-credential";

/// The slot directory — the mount of the broker's Secret.
pub const DIR_ENV: &str = "BOSS_RUNNER_CREDENTIAL_DIR";
pub const DEFAULT_DIR: &str = "/etc/boss/runner-credential";

/// The writer name a resolved runner credential satisfies, and the actor
/// it belongs to (the id ops-runner.sh signs its answers as).
pub const PRINCIPAL: &str = "runner:ops";
pub const ACTOR: &str = "automation:ops-runner";

/// The slots a host's credential may sit in, in the order a value two
/// slots of ONE host hold is named by.
pub const SLOTS: [&str; 3] = ["current", "next", "previous"];

/// What the door made of the presented header — the broker's
/// verify-by-effect read. Under `/api/jobs/` so every door that routes
/// the jobs API by path already reaches it.
pub const WHOAMI_PATH: &str = "/api/jobs/runner-credential";

/// The most directory entries one resolution reads. A Secret mount holds
/// one file per key plus kubelet's `..data` links; past this it is not
/// that directory, and a directory past it resolves NO ONE, with a
/// warning — never the partial read a cut in listing order would be.
const MAX_ENTRIES: usize = 256;

/// The slot directory, from the environment or its default.
pub fn dir() -> PathBuf {
    std::env::var_os(DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DIR))
}

/// The slot a resolved credential matched, beside the caller — for the
/// whoami answer, which a rotation reads to see which value a host sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedSlot(pub &'static str);

/// What one presented value resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one host's slots hold it.
    Resolved { host: String, slot: &'static str },
    /// No slot holds it (or there is no directory to hold one).
    Unmatched,
    /// Two or more hosts' slots hold it: resolved to none of them.
    Ambiguous { hosts: Vec<String> },
}

/// `<host>.<slot>` → `(host, slot)`, for a host id as the estate spells
/// one (lowercase, digits, inner hyphens) and one of [`SLOTS`]. Anything
/// else — kubelet's `..data`, a dotfile, an unknown slot — names no slot.
pub fn slot_of(file_name: &str) -> Option<(&str, &'static str)> {
    let (host, slot) = file_name.rsplit_once('.')?;
    let slot = SLOTS.into_iter().find(|s| *s == slot)?;
    let ok = !host.is_empty()
        && host.len() <= 63
        && host
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !host.starts_with('-')
        && !host.ends_with('-');
    ok.then_some((host, slot))
}

/// Resolve `presented` against every slot under `dir`. Blocking — the
/// door runs it on the blocking pool. Every slot is read and compared
/// before any answer is chosen, so timing says nothing about which slot,
/// or how many, a guess met.
pub fn resolve(dir: &Path, presented: &str) -> Resolution {
    let presented = presented.trim();
    if presented.is_empty() {
        return Resolution::Unmatched;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Resolution::Unmatched,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "the runner credential slots cannot be listed; the request passes with no credential");
            return Resolution::Unmatched;
        }
    };
    // One past the cap is read so that crossing it is SEEN: a directory cut
    // at the cap would be read in `read_dir`'s order, and a value two hosts
    // hold, one of them cut, would resolve to the other (backlog 1e50e66b).
    let listed: Vec<std::fs::DirEntry> = entries
        .filter_map(Result::ok)
        .take(MAX_ENTRIES + 1)
        .collect();
    if listed.len() > MAX_ENTRIES {
        tracing::warn!(dir = %dir.display(), cap = MAX_ENTRIES, "the runner credential directory holds more entries than the cap, so it is not the broker's Secret mount; the request passes with no credential");
        return Resolution::Unmatched;
    }
    let mut names: Vec<String> = listed
        .into_iter()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| slot_of(n).is_some())
        .collect();
    names.sort();
    let matched: Vec<(String, &'static str)> = names
        .iter()
        .filter_map(|name| {
            let (host, slot) = slot_of(name)?;
            match machine_token::read_slot_checked(dir, name) {
                Ok(Some(value)) => machine_token::verify(&value, Some(presented))
                    .then(|| (host.to_string(), slot)),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(slot = %name, error = %e, "a runner credential slot is unreadable and is skipped");
                    None
                }
            }
        })
        .collect();
    let mut hosts: Vec<String> = matched.iter().map(|(h, _)| h.clone()).collect();
    hosts.sort();
    hosts.dedup();
    match hosts.as_slice() {
        [] => Resolution::Unmatched,
        [host] => {
            let slot = SLOTS
                .into_iter()
                .find(|s| matched.iter().any(|(_, m)| m == s))
                .unwrap_or(SLOTS[0]);
            Resolution::Resolved {
                host: host.clone(),
                slot,
            }
        }
        _ => Resolution::Ambiguous { hosts },
    }
}

/// The door, as a middleware: take the header off the request, resolve
/// it, and on a match insert the [`CredentialedCaller`] a declared
/// writer believes. Never refuses (module doc).
pub async fn resolve_runner_credential(
    State(dir): State<Arc<PathBuf>>,
    mut req: Request,
    next: Next,
) -> Response {
    let presented: Vec<String> = req
        .headers()
        .get_all(HEADER)
        .iter()
        .map(|v| v.to_str().map(str::to_string).unwrap_or_default())
        .collect();
    req.headers_mut().remove(HEADER);
    let presented = match presented.as_slice() {
        [] => return next.run(req).await,
        [one] => one.clone(),
        many => {
            tracing::warn!(
                count = many.len(),
                "a request presented more than one runner credential; it passes with none"
            );
            return next.run(req).await;
        }
    };
    let path = req.uri().path().to_string();
    let resolution = tokio::task::spawn_blocking(move || resolve(&dir, &presented))
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "the runner credential resolution did not finish; the request passes with no credential");
            Resolution::Unmatched
        });
    match resolution {
        Resolution::Resolved { host, slot } => {
            req.extensions_mut().insert(CredentialedCaller {
                principal: PRINCIPAL.to_string(),
                actor_id: ACTOR.to_string(),
                host: Some(host),
            });
            req.extensions_mut().insert(ResolvedSlot(slot));
        }
        Resolution::Unmatched => {
            tracing::warn!(%path, "a runner credential no slot holds was presented; the request passes with no credential");
        }
        Resolution::Ambiguous { hosts } => {
            tracing::error!(%path, ?hosts, "a runner credential is held by more than one host's slots; it resolves to none of them");
        }
    }
    next.run(req).await
}

/// `GET` [`WHOAMI_PATH`]: what the door made of the header presented —
/// the principal, actor, host and slot, or `resolved: false`. Never the
/// value, nor anything derived from it.
async fn whoami(
    caller: Option<axum::Extension<CredentialedCaller>>,
    slot: Option<axum::Extension<ResolvedSlot>>,
) -> Json<Value> {
    Json(match caller {
        Some(axum::Extension(c)) => json!({
            "resolved": true,
            "principal": c.principal,
            "actor_id": c.actor_id,
            "host": c.host,
            "slot": slot.map(|axum::Extension(ResolvedSlot(s))| s),
        }),
        None => json!({ "resolved": false, "header": HEADER }),
    })
}

/// Mount the door over `app`: the whoami route, and the resolving layer
/// around every route `app` holds.
pub fn mount(app: Router, dir: PathBuf) -> Router {
    app.route(WHOAMI_PATH, get(whoami))
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(dir),
            resolve_runner_credential,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(files: &[(&str, &str)]) -> PathBuf {
        let dir = boss_testing::scratch_dir("runner-credential-unit");
        for (name, value) in files {
            boss_testing::write_file(&dir.join(name), value);
        }
        dir
    }

    #[test]
    fn a_slot_is_a_host_id_and_one_of_three_slot_names() {
        assert_eq!(slot_of("forge.current"), Some(("forge", "current")));
        assert_eq!(slot_of("boss-gcp.previous"), Some(("boss-gcp", "previous")));
        assert_eq!(slot_of("w-1.next"), Some(("w-1", "next")));
        for not in [
            "..data",
            "..2026_09_29_05_31_00.123",
            ".forge.current",
            "Forge.current",
            "forge.other",
            "forge",
            ".current",
            "-forge.current",
            "for ge.current",
            "forge.current.bak",
        ] {
            assert_eq!(slot_of(not), None, "{not}");
        }
    }

    #[test]
    fn a_value_resolves_to_the_one_host_that_holds_it() {
        let dir = slots(&[("forge.current", "aaaa"), ("boss-gcp.current", "bbbb")]);
        assert_eq!(
            resolve(&dir, "bbbb"),
            Resolution::Resolved {
                host: "boss-gcp".into(),
                slot: "current"
            }
        );
        assert_eq!(resolve(&dir, " aaaa\n"), resolve(&dir, "aaaa"));
        assert_eq!(resolve(&dir, "aaab"), Resolution::Unmatched);
        assert_eq!(resolve(&dir, ""), Resolution::Unmatched);
    }

    /// A blank slot is no credential, never one every blank header meets;
    /// a slot that is not a regular file is skipped, not fatal.
    #[test]
    fn a_blank_or_irregular_slot_holds_no_credential() {
        let dir = slots(&[("forge.current", "  \n"), ("forge.previous", "cccc")]);
        boss_testing::create_dir(&dir.join("boss-gcp.current"));
        assert_eq!(resolve(&dir, "  "), Resolution::Unmatched);
        assert_eq!(
            resolve(&dir, "cccc"),
            Resolution::Resolved {
                host: "forge".into(),
                slot: "previous"
            }
        );
    }

    #[test]
    fn a_value_two_hosts_hold_resolves_to_neither_and_one_host_twice_is_one() {
        let dir = slots(&[
            ("forge.current", "dddd"),
            ("forge.previous", "dddd"),
            ("boss-gcp.next", "eeee"),
            ("w-1.current", "eeee"),
        ]);
        assert_eq!(
            resolve(&dir, "dddd"),
            Resolution::Resolved {
                host: "forge".into(),
                slot: "current"
            }
        );
        assert_eq!(
            resolve(&dir, "eeee"),
            Resolution::Ambiguous {
                hosts: vec!["boss-gcp".into(), "w-1".into()]
            }
        );
    }

    /// `resolve` under a subscriber that keeps what it logs, so a test can
    /// read the warning an operator would.
    fn resolve_logged(dir: &Path, presented: &str) -> (Resolution, String) {
        #[derive(Clone, Default)]
        struct Kept(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Kept {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = Kept::default();
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        let resolution = tracing::subscriber::with_default(subscriber, || resolve(dir, presented));
        let text = String::from_utf8_lossy(&log.0.lock().unwrap()).into_owned();
        (resolution, text)
    }

    /// A slot directory of `entries` entries in all: the named slots, and
    /// fillers no slot is named by (kubelet's own entries are the real
    /// case of an entry that is not a slot).
    fn crowded(entries: usize, named: &[(&str, &str)]) -> PathBuf {
        let dir = slots(named);
        for i in 0..entries - named.len() {
            boss_testing::write_file(&dir.join(format!("filler-{i:03}")), "x");
        }
        dir
    }

    /// PAST THE CAP IS NOT A READ AT ALL (review of 8e5de104, finding 3;
    /// backlog 1e50e66b). The cap was a `.take()` before the sort, so past
    /// it WHICH slots were read hung on `read_dir`'s order — and a value
    /// two hosts hold, one of them cut, resolved to the other: the
    /// ambiguity rule broken by a listing order. The fixture is that exact
    /// case, one entry over the cap, so a partial read answers Resolved or
    /// Ambiguous whichever entry it cuts, and never the Unmatched a refused
    /// read answers. At the cap itself the directory still resolves.
    #[test]
    fn a_directory_past_the_cap_resolves_no_one_and_says_so() {
        let shared = "rcTWOtwoTWOtwoTWOtwoTWOtwoTWOtwoTWOtwoTWO0";
        let over = crowded(
            MAX_ENTRIES + 1,
            &[("forge.current", shared), ("boss-gcp.current", shared)],
        );
        let (resolution, log) = resolve_logged(&over, shared);
        assert_eq!(resolution, Resolution::Unmatched);
        assert!(
            log.contains("WARN") && log.contains(&MAX_ENTRIES.to_string()),
            "a refused read is a warning naming the cap: {log}"
        );
        assert!(!log.contains(shared), "the value reached the log: {log}");

        let at = crowded(MAX_ENTRIES, &[("forge.current", shared)]);
        assert_eq!(
            resolve(&at, shared),
            Resolution::Resolved {
                host: "forge".into(),
                slot: "current"
            }
        );
    }

    #[test]
    fn no_directory_is_no_credential() {
        let dir = boss_testing::scratch_dir("runner-credential-none").join("absent");
        assert_eq!(resolve(&dir, "aaaa"), Resolution::Unmatched);
    }

    /// Every principal a Workflow row may declare as a writer is one this
    /// door (the only credential door) resolves — the lint's list and the
    /// door cannot drift (CLAUDE.md §9a). Empty today, by design (module
    /// doc): the principal enters with the credential's delivery.
    #[test]
    fn every_resolvable_principal_is_one_a_door_resolves() {
        for p in crate::field_writer::RESOLVABLE_PRINCIPALS {
            assert_eq!(
                *p, PRINCIPAL,
                "`{p}` is declared resolvable, and no door resolves it"
            );
        }
    }
}

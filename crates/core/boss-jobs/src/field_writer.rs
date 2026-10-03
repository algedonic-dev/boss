//! A key a human signs has one declared writer, and the server — not
//! the caller — says who that writer is (design f623e425, David
//! 2026-09-25; backlog 6c9183de).
//!
//! WHY. An ops-request's `approve` step carries the keys the runner
//! renders on the host — `plan`, `verb`, `host`, `args`,
//! `rendered_plan_sha256` — and a passkey stamp binds the step's whole
//! shape. Until this module any caller with Update on the step could
//! merge those keys, so the passkey could be asked to sign a plan the
//! runner never rendered. The runner's re-render and the ceremony
//! binding REFUSE a swap after the fact; nothing PREVENTED one.
//!
//! THE DECLARATION IS REGISTRY DATA. A Workflow row names the one
//! writer of a key on the field itself (`StepField::writer`, e.g.
//! `writer = "runner:ops"`); the field list is fixed at admission and
//! the step PUT refuses a body that changes it (b433bdf3), so the
//! declaration cannot be written away by the caller it binds. A step
//! with no declared writer answers exactly as before.
//!
//! WHO THE WRITER IS NEVER COMES FROM THE CALLER. The `x-boss-user` id
//! is self-asserted: every machine-door caller holds the same estate
//! token and can type `automation:ops-runner`. A rule keyed on that id
//! is design option D, rejected because a guard that LOOKS like
//! protection against the adversary the review named, and is not, is a
//! mostly-sure guard. So a declared writer is satisfied ONLY by a
//! [`CredentialedCaller`] request extension, which a client cannot set:
//! only a server-side door that resolved a presented credential inserts
//! one. That door is [`crate::runner_credential`] (the resolve step of
//! design f623e425 option A); until the broker's Secret it reads is
//! mounted and filled (the credential kind and its broker handler, the
//! next car), no caller satisfies a declared writer, which is why no
//! live protocol declares one yet — the declaration lands on the
//! ops-request row with the credential delivery, never before it. The
//! viability lint holds that: a `writer` not in
//! [`RESOLVABLE_PRINCIPALS`] is refused at publish.
//!
//! THE HOST BINDING. A credential bound to a host writes only packets
//! whose `host` (job metadata) is that host — "a runner for host h
//! writes only requests whose host is h" — so the credential of one
//! runner cannot author the plan another host's runner is asked to run.
//! That holds only while `host` cannot move under it, so on a packet
//! whose steps declare a writer both job doors refuse a change to it
//! after admission ([`host_changed`], review S2 of car 1e603cfd).
//!
//! THE OTHER DOORS (the same review). A re-pin never moves a live
//! step's declared writer nor writes a key one reserves
//! ([`repin_refusals`], S1); the merge door drops an unchanged re-send
//! of a reserved key rather than apply it to a row the writer may have
//! moved since the read ([`strip_unchanged_reserved`], S3).

use boss_core::job::StepField;
use serde::Serialize;
use serde_json::Value;

/// The caller as a server-side credential door resolved it. Inserted
/// into the request's extensions by that door and by nothing else; a
/// client has no way to set a request extension, which is the whole
/// point — this is the only identity a declared writer believes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialedCaller {
    /// The writer name the credential satisfies (`runner:ops`).
    pub principal: String,
    /// The actor the credential belongs to, for the record.
    pub actor_id: String,
    /// The host the credential is bound to, when it is bound to one.
    pub host: Option<String>,
}

/// A key whose value this write would change, and the writer its
/// protocol declares for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReservedKey {
    pub key: String,
    pub writer: String,
}

/// Every writer principal a server-side credential door resolves into a
/// [`CredentialedCaller`] — the only names a Workflow row may declare as
/// a field's `writer` (the viability lint refuses any other, backlog
/// 6c9183de review S4). EMPTY until the first door is mounted: no door
/// resolves any principal today, so a declared writer would lock its key
/// against every caller. The car that mounts a door adds its principal
/// here, and only then may a protocol declare it — which is how "the
/// declaration lands with the credential delivery, never before it"
/// stopped being a sentence and became a refusal.
///
/// STILL EMPTY WITH THE DOOR MOUNTED (2026-09-29). The resolve step
/// ([`crate::runner_credential`]) resolves `runner:ops`, but only from a
/// slot the broker has filled, and no broker fills one yet: listed now,
/// a protocol could declare the writer and lock the runner out of the
/// plan David approves. `runner:ops` enters here in the car that
/// delivers a credential the runner can present; the door's test pins
/// every entry to a principal it resolves.
pub const RESOLVABLE_PRINCIPALS: &[&str] = &[];

/// The job-metadata key a host-bound credential is judged against.
pub const HOST_KEY: &str = "host";

/// Every key with a declared writer whose value differs between the
/// stored metadata and the metadata as this write would leave it. A key
/// added, changed or removed (`null` through the merge door) is a
/// change; an unchanged re-send is not, so a read-modify-write caller
/// that sends the stored value back is never refused for it.
pub fn reserved_keys_changed(fields: &[StepField], old: &Value, new: &Value) -> Vec<ReservedKey> {
    fields
        .iter()
        .filter_map(|f| {
            let writer = f.writer.as_deref()?;
            (old.get(&f.name) != new.get(&f.name)).then(|| ReservedKey {
                key: f.name.clone(),
                writer: writer.to_string(),
            })
        })
        .collect()
}

/// Remove from `patch` every key with a declared writer whose patched
/// value equals `stored` — a set to the stored value, or a `null` for a
/// key the row does not hold — and answer whether any was removed
/// (backlog 6c9183de, review S3 of car 1e603cfd, 2026-09-26).
///
/// WHY. The merge door judges a patch against the row it READ, and the
/// adapter applies it to the row as it STANDS. An unchanged re-send is
/// not a change, so a caller who is not the writer is admitted with it
/// — and if the writer wrote between that read and the merge, the
/// re-send put the old value back: the runner's newer plan reverted by
/// a caller who may not write plans. A key this write does not change
/// has no business in the write, so it never reaches the adapter.
///
/// NEVER THE WRITER'S OWN (review of car f3365343, 2026-09-28, follow-up
/// a). The strip guards a key's writer AGAINST other callers. Applied to
/// the writer too, it answered a runner pass that raced another pass 204
/// and threw its write away — the one caller whose value the key exists
/// to hold. So a key `caller` is the admitted writer of (for a packet
/// whose host is `job_host`) stays in the patch and lands as sent.
pub fn strip_unchanged_reserved(
    fields: &[StepField],
    stored: &Value,
    patch: &mut serde_json::Map<String, Value>,
    caller: Option<&CredentialedCaller>,
    job_host: Option<&str>,
) -> bool {
    let before = patch.len();
    for f in fields.iter() {
        let Some(writer) = f.writer.as_deref() else {
            continue;
        };
        if admits(caller, writer, job_host).is_ok() {
            continue;
        }
        let unchanged = match patch.get(&f.name) {
            Some(Value::Null) => stored.get(&f.name).is_none(),
            Some(v) => stored.get(&f.name) == Some(v),
            None => false,
        };
        if unchanged {
            patch.remove(&f.name);
        }
    }
    patch.len() != before
}

/// Whether any of a packet's steps declares a field writer — the packets
/// whose job-metadata [`HOST_KEY`] is fixed at admission (review S2).
pub fn declares_a_writer<'a>(steps: impl IntoIterator<Item = &'a boss_core::job::Step>) -> bool {
    steps
        .into_iter()
        .any(|s| s.fields.iter().any(|f| f.writer.is_some()))
}

/// Whether a job-metadata write moves [`HOST_KEY`]: `old` is the stored
/// metadata, `new` what the write leaves (absent = removed).
pub fn host_changed(old: &Value, new: &Value) -> bool {
    old.get(HOST_KEY) != new.get(HOST_KEY)
}

/// The refusal a job door answers a moved [`HOST_KEY`] with, on a
/// packet whose steps declare a writer.
pub fn host_refusal_body(job_id: &str, stored_host: Option<&Value>) -> Value {
    serde_json::json!({
        "error": "this packet's host is fixed at admission: a key on it is reserved to a \
                  host-bound writer, and that writer is judged against this host",
        "job_id": job_id,
        "refused_keys": [HOST_KEY],
        "stored_host": stored_host,
        "hint": "a request for another host is a new packet: file it again naming that host",
        "rule": "a key a human signs has one declared writer, and the server knows who that \
                 writer is (design f623e425; backlog 6c9183de review S2)",
    })
}

/// Why a re-pin may not rewrite a LIVE step from `now` (the packet's own
/// fields and metadata) to `next` (the target's) — empty when it may
/// (backlog 6c9183de, review S1 of car 1e603cfd, 2026-09-26). The
/// convert door rewrote a live step's `fields` from the target
/// wholesale and re-projected its defaults, consulting no writer, so a
/// protocol version could release a reserved key, re-assign it, or
/// write into it. Two refusals:
///
/// - A field's declared writer MOVES — dropped, changed, or declared
///   where there was none. Dropped or changed releases a key whose
///   value only the old writer could have put there; ADDED presents a
///   value anyone wrote as the new writer's. Either misstates who
///   wrote the record, so neither happens to a step in flight.
/// - The re-pin itself would change a reserved key's value. The
///   re-projection is the protocol writing, and the protocol is not
///   the declared writer.
pub fn repin_refusals(
    now_fields: &[StepField],
    next_fields: &[StepField],
    now_metadata: &Value,
    next_metadata: &Value,
) -> Vec<String> {
    let writer_of = |fields: &[StepField], name: &str| {
        fields
            .iter()
            .find(|f| f.name == name)
            .and_then(|f| f.writer.clone())
    };
    let mut names: Vec<&str> = now_fields
        .iter()
        .chain(next_fields)
        .filter(|f| f.writer.is_some())
        .map(|f| f.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    let shown = |w: &Option<String>| w.as_deref().unwrap_or("none").to_string();
    let mut out: Vec<String> = names
        .iter()
        .filter_map(|name| {
            let (was, will) = (writer_of(now_fields, name), writer_of(next_fields, name));
            (was != will).then(|| {
                format!(
                    "field `{name}` declares writer `{}` on the packet and `{}` in the target: \
                     a live step's declared writer does not move on a re-pin",
                    shown(&was),
                    shown(&will),
                )
            })
        })
        .collect();
    let mut changed = reserved_keys_changed(now_fields, now_metadata, next_metadata);
    changed.extend(reserved_keys_changed(
        next_fields,
        now_metadata,
        next_metadata,
    ));
    changed.sort_by(|a, b| a.key.cmp(&b.key));
    changed.dedup_by(|a, b| a.key == b.key);
    out.extend(changed.into_iter().map(|r| {
        format!(
            "the re-pin would change `{}`, which only `{}` may write",
            r.key, r.writer
        )
    }));
    out
}

/// Whether `caller` is the declared `writer` for a packet whose `host`
/// is `job_host`. `Err` carries why not, in words the refusal returns.
pub fn admits(
    caller: Option<&CredentialedCaller>,
    writer: &str,
    job_host: Option<&str>,
) -> Result<(), String> {
    let Some(caller) = caller else {
        return Err(format!(
            "the request presented no credential the server resolved, and only a credential \
             for `{writer}` may write this key — the `x-boss-user` id is self-asserted and is \
             never read as a writer"
        ));
    };
    if caller.principal != writer {
        return Err(format!(
            "the presented credential is for `{}`, not `{writer}`",
            caller.principal
        ));
    }
    match (caller.host.as_deref(), job_host) {
        (None, _) => Ok(()),
        (Some(bound), Some(host)) if bound == host => Ok(()),
        (Some(bound), Some(host)) => Err(format!(
            "the presented credential is bound to host `{bound}`, and this packet's host is `{host}`"
        )),
        (Some(bound), None) => Err(format!(
            "the presented credential is bound to host `{bound}`, and this packet names no host"
        )),
    }
}

/// The reserved keys this caller may not change, each with why. Empty
/// means the write may proceed as far as declared writers go.
pub fn refused(
    reserved: &[ReservedKey],
    caller: Option<&CredentialedCaller>,
    job_host: Option<&str>,
) -> Vec<(ReservedKey, String)> {
    reserved
        .iter()
        .filter_map(|r| {
            admits(caller, &r.writer, job_host)
                .err()
                .map(|why| (r.clone(), why))
        })
        .collect()
}

/// The 409 body: the step, the door, every refused key with its
/// declared writer, who asked (the self-asserted id, reported and not
/// believed) and the resolved credential when there was one — the shape
/// the `human_only` refusal takes, so a caller reads both the same way.
pub fn refusal_body(
    step_id: &str,
    step_title: &str,
    door: &str,
    asked_by: &str,
    caller: Option<&CredentialedCaller>,
    refused: &[(ReservedKey, String)],
) -> Value {
    serde_json::json!({
        "error": "this write changes a key its protocol reserves to one declared writer",
        "step_id": step_id,
        "step_title": step_title,
        "door": door,
        "asked_by": asked_by,
        "credential": caller.map(|c| serde_json::json!({
            "principal": c.principal,
            "actor_id": c.actor_id,
            "host": c.host,
        })),
        "refused_keys": refused
            .iter()
            .map(|(r, why)| serde_json::json!({ "key": r.key, "writer": r.writer, "why": why }))
            .collect::<Vec<_>>(),
        "rule": "a key a human signs has one declared writer, and the server knows who that \
                 writer is (design f623e425; backlog 6c9183de)",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(name: &str, writer: Option<&str>) -> StepField {
        StepField {
            filled_by: Default::default(),
            writer: writer.map(str::to_string),
            ..StepField::new(name, "string")
        }
    }

    fn runner(host: Option<&str>) -> CredentialedCaller {
        CredentialedCaller {
            principal: "runner:ops".into(),
            actor_id: "automation:ops-runner".into(),
            host: host.map(str::to_string),
        }
    }

    #[test]
    fn only_a_declared_key_whose_value_changes_is_reserved() {
        let fields = [field("plan", Some("runner:ops")), field("comment", None)];
        let old = json!({ "plan": "PLAN a", "comment": "x" });
        // Unchanged plan, changed comment: nothing reserved.
        assert!(
            reserved_keys_changed(&fields, &old, &json!({ "plan": "PLAN a", "comment": "y" }))
                .is_empty()
        );
        // Changed, added and removed are each a change.
        for new in [json!({ "plan": "PLAN wipe" }), json!({ "comment": "x" })] {
            assert_eq!(
                reserved_keys_changed(&fields, &old, &new),
                vec![ReservedKey {
                    key: "plan".into(),
                    writer: "runner:ops".into()
                }]
            );
        }
        assert_eq!(
            reserved_keys_changed(&fields, &json!({}), &json!({ "plan": "p" })).len(),
            1
        );
    }

    /// Review S3: only a reserved key the patch leaves as stored is
    /// dropped — a set to the stored value, or a null for an absent key.
    /// A change stays (for the writer check to judge) and so does every
    /// undeclared key.
    #[test]
    fn only_an_unchanged_reserved_key_is_stripped_from_a_patch() {
        let fields = [
            field("plan", Some("runner:ops")),
            field("verb", Some("runner:ops")),
            field("comment", None),
        ];
        let stored = json!({ "plan": "PLAN a", "comment": "x" });
        let mut patch = json!({ "plan": "PLAN a", "verb": null, "comment": "x" })
            .as_object()
            .cloned()
            .unwrap();
        assert!(strip_unchanged_reserved(
            &fields, &stored, &mut patch, None, None
        ));
        assert_eq!(Value::Object(patch), json!({ "comment": "x" }));

        let mut patch = json!({ "plan": "PLAN b" }).as_object().cloned().unwrap();
        assert!(!strip_unchanged_reserved(
            &fields, &stored, &mut patch, None, None
        ));
        assert_eq!(Value::Object(patch), json!({ "plan": "PLAN b" }));
    }

    /// Follow-up a of the review of car f3365343: the declared writer's
    /// own unchanged re-send is never stripped — it is the record — while
    /// the same patch from a credential for another host still is.
    #[test]
    fn the_declared_writers_own_re_send_is_never_stripped() {
        let fields = [field("plan", Some("runner:ops")), field("comment", None)];
        let stored = json!({ "plan": "PLAN a" });
        let sent = json!({ "plan": "PLAN a" }).as_object().cloned().unwrap();

        let mut patch = sent.clone();
        let own = runner(Some("forge"));
        assert!(!strip_unchanged_reserved(
            &fields,
            &stored,
            &mut patch,
            Some(&own),
            Some("forge")
        ));
        assert_eq!(Value::Object(patch), json!({ "plan": "PLAN a" }));

        let mut patch = sent;
        assert!(strip_unchanged_reserved(
            &fields,
            &stored,
            &mut patch,
            Some(&own),
            Some("boss-gcp")
        ));
        assert!(patch.is_empty());
    }

    #[test]
    fn no_credential_never_satisfies_a_declared_writer() {
        let why = admits(None, "runner:ops", Some("forge")).unwrap_err();
        assert!(why.contains("self-asserted"), "{why}");
    }

    #[test]
    fn a_credential_for_another_principal_is_refused() {
        let other = CredentialedCaller {
            principal: "runner:other".into(),
            ..runner(None)
        };
        assert!(admits(Some(&other), "runner:ops", Some("forge")).is_err());
    }

    #[test]
    fn a_host_bound_credential_writes_only_its_own_hosts_packets() {
        assert_eq!(
            admits(Some(&runner(Some("forge"))), "runner:ops", Some("forge")),
            Ok(())
        );
        assert!(admits(Some(&runner(Some("forge"))), "runner:ops", Some("boss-gcp")).is_err());
        assert!(admits(Some(&runner(Some("forge"))), "runner:ops", None).is_err());
        assert_eq!(
            admits(Some(&runner(None)), "runner:ops", Some("forge")),
            Ok(())
        );
    }
}

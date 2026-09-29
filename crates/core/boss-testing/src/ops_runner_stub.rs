//! THE OPS RUNNER'S STEP WRITE, AS A STUBBED SYSTEM OF RECORD SEES IT —
//! one copy for every verb test that drives `infra/ops/ops-runner.sh`
//! through the real allowlist (backlog 2aa2b19e).
//!
//! The runner completes a step in two writes, the gate runner's shape
//! (e39a9d2a): the keys it records go through the step merge door,
//! `PATCH /api/jobs/{id}/steps/{step_id}/metadata`, and the status goes
//! ALONE, `PUT /api/jobs/{id}/steps/{step_id}`. It used to be one PUT
//! of `{status, metadata}` carrying every key the runner had READ at
//! poll time plus its own, and seven verb tests each spelled a `curl`
//! stub that kept that PUT and read the step's metadata out of it — the
//! same fact about the runner in seven files. They read it through here
//! now, so the next change to how the runner writes is one edit.

use std::path::Path;

/// `curl` stub text for a `#!/bin/sh` stub, placed before its reads.
/// Any write — a call carrying `--data-binary @file` — answers 200 on
/// `-w` and ends the stub; the one made through a STEP MERGE DOOR
/// (`.../steps/<id>/metadata`), which carries the keys the runner
/// recorded on the step, is also copied to `$STUB_STEP_METADATA`. The
/// request-level door (`/api/jobs/<id>/metadata`) and the status PUT
/// are answered and not kept.
pub const RECORD_STEP_METADATA: &str = "for a in \"$@\"; do case \"$a\" in @*)\n\
     for u in \"$@\"; do case \"$u\" in */steps/*/metadata) cp \"${a#@}\" \"$STUB_STEP_METADATA\";; esac; done\n\
     printf 200; exit 0;; esac; done\n";

/// The lines of `out` that `infra/ops/verbs/<verb>.json`'s declared
/// `effect` matches, judged by the runner's own engine (jq's `test`,
/// Oniguruma — not Rust's regex), so a verb test asserts the effect the
/// runner would record rather than a substring of it. `None` when jq is
/// absent (the gate image has it). Panics when the verb declares no
/// `effect`: asking for the effect of a verb that has none is the test's
/// mistake, not an empty answer (backlog 1058e686).
pub fn effect_lines(verb: &str, out: &str) -> Option<Vec<String>> {
    std::process::Command::new("jq")
        .arg("--version")
        .output()
        .ok()?;
    let file = crate::repo_root().join(format!("infra/ops/verbs/{verb}.json"));
    let spec: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {}: {e}", file.display())),
    )
    .unwrap_or_else(|e| panic!("{} is not JSON: {e}", file.display()));
    let re = spec["effect"]
        .as_str()
        .unwrap_or_else(|| panic!("{verb} declares no `effect`"))
        .to_string();
    let hits = out
        .lines()
        .filter(|line| {
            let o = std::process::Command::new("jq")
                .args([
                    "-n",
                    "--arg",
                    "re",
                    &re,
                    "--arg",
                    "l",
                    line,
                    "$l | test($re)",
                ])
                .output()
                .expect("jq runs");
            assert!(o.status.success(), "jq judges {re:?}: {o:?}");
            String::from_utf8_lossy(&o.stdout).trim() == "true"
        })
        .map(str::to_string)
        .collect();
    Some(hits)
}

/// What the runner recorded on the step, as a stub carrying
/// [`RECORD_STEP_METADATA`] kept it at `path`; `None` when it recorded
/// nothing there. Panics, naming the file, on a body that is not JSON.
pub fn step_metadata_written(path: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(path).ok().map(|s| {
        serde_json::from_str(&s).unwrap_or_else(|e| {
            panic!(
                "the step metadata the runner wrote ({}) is not JSON: {e}: {s}",
                path.display()
            )
        })
    })
}

//! No manifest line sets the `agent-role` word to `enforce` until the
//! agents registry rows carry the roles the operator decided, read back.
//!
//! Backlog 4e51bf23, review cdff423f N7 and N8. The jobs API's login
//! door can judge a registered agent by its registry row's role, behind
//! the word `agent-role`: one more key of the `boss-machine-gate`
//! ConfigMap, read at `/etc/boss/machine-gate/agent-role`. The car that
//! built it lands with NO key, which is `off`, because of what was
//! measured beside the defect on 2026-10-08: `agent-claude`'s row holds
//! `engineering-agent`, a role that holds none of the live policy rules,
//! and `agent-codex`'s holds `null`. With those rows, ONE manifest line
//! — `agent-role: enforce` — locks the coordinating session and every
//! builder out of the system of record at the next converge: they could
//! not read a packet, gate, report or release. And the way back is
//! David's hands only, because the dev session can neither read nor
//! patch that ConfigMap.
//!
//! The sibling pin (`the_machine_gate_reports_and_refuses_nothing.rs`)
//! holds the `mode` and `policy-check` keys and does not know this one,
//! so nothing stood in front of that line. This file does, the way the
//! machine gate's own enforce is held for row C: THE CAR THAT SETS THE
//! WORD EDITS THIS PIN, in the same commit, and says in its park prose
//! what it read back from `GET /api/agents`. A car that sets the word
//! and leaves this file alone is red here, by name.
//!
//! N8, the second refusal: the key's value must be a string a ConfigMap
//! can hold. Written bare, `agent-role: off` is a YAML boolean, not the
//! word — the revert car's one line would be rejected or coerced at the
//! moment it is needed. `"off"` quoted, or `report`, is the way back.
//!
//! THE VALUE IS READ THE WAY YAML READS IT (delta review 012ccafa, D1).
//! This pin first matched lines by hand and handed everything after the
//! colon to `Mode::parse`. `agent-role: enforce # row C` got through:
//! the pin read the word `enforce # row c`, which the parse calls
//! unknown and reads as `report`, while a YAML reader ends a plain
//! scalar at ` #` and hands the pod `enforce`. That is the one line this
//! file exists to refuse, in a manifest whose authors comment heavily.
//! So the manifest is now read through `boss_testing::rbac`, the one
//! YAML reader the credential pins share, and nothing here splits a
//! line. Three things follow from that reader and are wanted:
//!
//!  - a shape it does not read — a tag (`!!str enforce`), an anchor, a
//!    duplicate key, a double-quoted escape, a `kind: List` — is
//!    REFUSED, naming the file and line, never passed;
//!  - EVERY document named `boss-machine-gate` is judged, not the first:
//!    an apply takes the last of two, so a second one carrying the word
//!    is the live one;
//!  - a value that is not a string (a bare boolean, a number, null, a
//!    block scalar, a collection) is refused, as is the key under
//!    `binaryData`, which mounts as the same file.
//!
//! The reader unquotes, so whether `off` was written in quotes is asked
//! of the value's own line: the word the reader returned, between a pair
//! of quotes.
//!
//! The matcher's own shapes are checked before the manifest is: a
//! matcher that reads nothing would pass every manifest. A second test
//! holds the door's constant to the path the pod mounts (D2).
//!
//! tree-wide pin — it reads infra/cluster/manifests/boss.yaml, which no
//! changed-file map attributes to this crate, so every scoped gate runs
//! it (`tree_wide_pins` in infra/gate.sh).

use boss_core::machine_gate::Mode;
use boss_testing::rbac::{self, Node, Object, Value};
use boss_testing::repo_root;

const MANIFEST: &str = "infra/cluster/manifests/boss.yaml";
const CONFIG_MAP: &str = "boss-machine-gate";

/// The key, read off the door's own constant: the file name the reader
/// opens inside the mounted directory. Never spelled here.
fn key() -> &'static str {
    boss_jobs::agents::ROW_ROLE_MODE_FILE
        .rsplit('/')
        .next()
        .expect("a file name")
}

#[derive(Debug)]
enum Refusal {
    /// The word is `enforce`, by the one parse every mode word takes.
    Enforce(String),
    /// The value is not a string as written.
    NotAString(String),
    /// The manifest, or the key's value, is a shape the reader does not
    /// read. Refused, not passed.
    Unread(String),
}

/// Was `word` written inside quotes on `line`?
fn quoted_on(line: &str, word: &str) -> bool {
    line.contains(&format!("\"{word}\"")) || line.contains(&format!("'{word}'"))
}

/// One `boss-machine-gate` document's answer for `key`.
fn judge(object: &Object, text: &str, key: &str) -> Result<(), Refusal> {
    let shown = |node: &Node| {
        let line = text.lines().nth(node.line.saturating_sub(1)).unwrap_or("");
        format!("line {}: {}", node.line, line.trim())
    };
    if let Some(node) = object.root.path(&["binaryData", key]) {
        return Err(Refusal::NotAString(shown(node)));
    }
    let Some(data) = object.root.get("data") else {
        return Ok(());
    };
    let node = match &data.value {
        Value::Null => return Ok(()),
        Value::Map(_) => match data.get(key) {
            Some(node) => node,
            None => return Ok(()),
        },
        _ => return Err(Refusal::Unread(shown(data))),
    };
    let Value::Str(word) = &node.value else {
        return Err(Refusal::NotAString(shown(node)));
    };
    if Mode::parse(Some(word)).0 == Mode::Enforce {
        return Err(Refusal::Enforce(shown(node)));
    }
    let line = text.lines().nth(node.line.saturating_sub(1)).unwrap_or("");
    let bare = !quoted_on(line, word);
    if bare && (rbac::yaml_bool(node).is_some() || word.parse::<f64>().is_ok()) {
        return Err(Refusal::NotAString(shown(node)));
    }
    Ok(())
}

/// Every document named `boss-machine-gate` in `text`, judged; answers
/// how many there were.
fn judged(source: &str, text: &str, key: &str) -> Result<usize, Refusal> {
    let objects = rbac::read_stream(source, text).map_err(Refusal::Unread)?;
    let mut documents = 0;
    for object in objects
        .iter()
        .filter(|o| o.kind == "ConfigMap" && o.name == CONFIG_MAP)
    {
        judge(object, text, key)?;
        documents += 1;
    }
    Ok(documents)
}

/// A `boss-machine-gate` ConfigMap whose `data:` block is `lines`.
fn config_map(lines: &str) -> String {
    format!(
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {CONFIG_MAP}\n  namespace: boss\n\
         data:\n{lines}\n"
    )
}

/// The same, with `data` written on its own line (`data: {{...}}`).
fn config_map_inline(data: &str) -> String {
    format!(
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {CONFIG_MAP}\n  namespace: boss\n\
         {data}\n"
    )
}

#[test]
fn no_manifest_line_sets_agent_role_to_enforce_or_to_a_yaml_boolean() {
    let key = key();
    assert_eq!(key, "agent-role", "the door's word, off its own constant");
    let read = |text: &str| judged("shape", text, key);

    // The matcher first, against every shape it must read.
    let today = config_map("  mode: report\n  actor-role: report");
    let second_document = format!(
        "{today}---\n{}",
        config_map("  mode: report\n  agent-role: enforce")
    );
    let other_namespace = format!(
        "{today}---\n{}",
        config_map("  agent-role: enforce").replace("namespace: boss", "namespace: playground")
    );
    for shape in [
        config_map("  agent-role: enforce"),
        config_map("  agent-role: Enforce"),
        config_map("  agent-role: \"ENFORCE\""),
        config_map("  agent-role: ' enforce '"),
        config_map("  \"agent-role\": \"enforce\""),
        config_map("  mode: report\n  agent-role: enforce"),
        config_map_inline("data: {mode: report, agent-role: enforce}"),
        config_map_inline("data: {\"mode\": \"report\", \"agent-role\": \"enforce\"}"),
        // Delta review 012ccafa D1: YAML ends a plain scalar at ` #`.
        config_map("  agent-role: enforce # row C"),
        config_map("  agent-role: \"enforce\" # row C"),
        config_map("  agent-role: 'enforce'\t# row C"),
        config_map("  agent-role: enforce   \t"),
        config_map("  agent-role : enforce"),
        // The word on the line below its key is still the word.
        config_map("  agent-role:\n    enforce"),
        config_map_inline("data: {mode: report, agent-role: enforce} # row C"),
        // ...and an apply takes the LAST document of a name.
        second_document,
        other_namespace,
    ] {
        assert!(
            matches!(read(&shape), Err(Refusal::Enforce(_))),
            "read as enforce: {shape:?} gave {:?}",
            read(&shape)
        );
    }
    for shape in [
        config_map("  agent-role: off"),
        config_map("  agent-role: Off"),
        config_map("  agent-role: on"),
        config_map("  agent-role: true"),
        config_map("  agent-role: no"),
        config_map("  agent-role:"),
        config_map("  agent-role: ~"),
        config_map("  agent-role: |\n    enforce"),
        config_map("  agent-role: >-\n    enforce"),
        config_map("  agent-role: 1"),
        config_map("  agent-role: [enforce]"),
        config_map("  agent-role: off # note"),
        config_map_inline("data: {mode: report, agent-role: off}"),
        config_map_inline("binaryData:\n  agent-role: ZW5mb3JjZQo="),
    ] {
        assert!(
            matches!(read(&shape), Err(Refusal::NotAString(_))),
            "not a string as written: {shape:?} gave {:?}",
            read(&shape)
        );
    }
    // Shapes the reader does not read: refused, never passed.
    for shape in [
        config_map("  agent-role: !!str enforce"),
        config_map("  agent-role: &word enforce"),
        config_map("  agent-role: report\n  agent-role: enforce"),
        config_map("  agent-role: \"enf\\x6frce\""),
        config_map_inline("data: enforce"),
        format!(
            "apiVersion: v1\nkind: List\nitems:\n  - kind: ConfigMap\n    metadata:\n      \
             name: {CONFIG_MAP}\n    data:\n      agent-role: enforce\n"
        ),
    ] {
        assert!(
            matches!(read(&shape), Err(Refusal::Unread(_))),
            "refused as unread: {shape:?} gave {:?}",
            read(&shape)
        );
    }
    for shape in [
        today.clone(),
        config_map("  policy-check: enforce"),
        config_map("  agent-role: report"),
        config_map("  agent-role: report # note"),
        config_map("  agent-role: \"off\""),
        config_map("  agent-role: 'off'"),
        config_map("  agent-role: \"off\" # the way back"),
        config_map("  agent-role: \"report\""),
        config_map("  x-agent-role: enforce"),
        config_map("  # agent-role: enforce"),
        config_map("  agent-role-note: enforce"),
        config_map_inline("data:"),
        // Only boss-machine-gate is mounted where the door reads.
        format!(
            "{today}---\n{}",
            config_map("  agent-role: enforce").replace(CONFIG_MAP, "some-other-map")
        ),
    ] {
        assert!(
            read(&shape).is_ok(),
            "admitted: {shape:?} gave {:?}",
            read(&shape)
        );
    }
    assert_eq!(read(&today).ok(), Some(1), "one document judged");
    assert_eq!(
        read(&format!("{today}---\n{today}")).ok(),
        Some(2),
        "EVERY document of the name is judged, not the first"
    );
    assert_eq!(
        read(&today.replace(CONFIG_MAP, "some-other-map")).ok(),
        Some(0),
        "a manifest without the ConfigMap judges nothing, and says so"
    );

    // Then the manifest. A reader aimed at the wrong document answers
    // "no such key" as confidently as the right one, so it is held to an
    // answer already known: this is the ConfigMap that carries `mode`.
    let manifest = std::fs::read_to_string(repo_root().join(MANIFEST))
        .unwrap_or_else(|e| panic!("{MANIFEST}: {e}"));
    let carries_mode = rbac::read_stream(MANIFEST, &manifest)
        .unwrap_or_else(|e| panic!("{e}"))
        .iter()
        .filter(|o| o.kind == "ConfigMap" && o.name == CONFIG_MAP)
        .any(|o| o.root.path(&["data", "mode"]).is_some());
    assert!(
        carries_mode,
        "ConfigMap {CONFIG_MAP} in {MANIFEST} carries the machine gate's `mode` key; if it does \
         not, this pin is reading the wrong document"
    );
    match judged(MANIFEST, &manifest, key) {
        Ok(documents) => assert!(
            documents >= 1,
            "{MANIFEST} declares ConfigMap {CONFIG_MAP}; a pin that judged no document judged \
             nothing"
        ),
        Err(Refusal::Enforce(line)) => panic!(
            "{MANIFEST}: ConfigMap {CONFIG_MAP} sets `{key}` to enforce (`{line}`).\n\
             Under that word the jobs API judges every registered agent by its agents-registry \
             row and drops the role it sends. Measured 2026-10-08: agent-claude's row held \
             `engineering-agent`, which holds no policy rule, and agent-codex's held null. With \
             rows like those, this one line locks the coordinating session and every builder \
             out of the system of record at the next converge, and the dev session cannot patch \
             the word back.\n\
             WHAT MUST BE TRUE FIRST: the agents registry rows carry the roles the operator \
             decided, READ BACK from the live registry (`boss-api GET /api/agents`), not \
             assumed from a publish, and the tenant declaration says the same so a later \
             `boss tenant publish --take agents` does not undo it.\n\
             THIS PIN IS EDITED IN THE SAME CAR THAT SETS THE WORD (backlog 4e51bf23; review \
             cdff423f N7), and that car's park prose quotes the rows it read. If you are that \
             car, change this pin to hold the word to `enforce` on this one line; if you are \
             not, take the line out."
        ),
        Err(Refusal::NotAString(line)) => panic!(
            "{MANIFEST}: ConfigMap {CONFIG_MAP} gives `{key}` a value that is not a string as \
             written (`{line}`). An unquoted off, on, yes, no, true or false is a YAML boolean, \
             an empty value is null, and a ConfigMap's data holds strings: the apply is refused \
             or the word is not the one written. Write \"off\" in quotes, or `report` — and put \
             the word on the key's own line, never in a block scalar (review cdff423f N8)."
        ),
        Err(Refusal::Unread(what)) => panic!(
            "{MANIFEST}: this pin could not read what ConfigMap {CONFIG_MAP} gives `{key}`: \
             {what}\n\
             It reads the manifest the way YAML does and refuses what its reader does not read \
             (a tag, an anchor, a duplicate key, a double-quoted escape, a List), \
             because a shape it cannot read is not a word it has judged. Write the key as one \
             plain or quoted word on its own line under `data:` (delta review 012ccafa D1)."
        ),
    }
}

/// THE DOOR READS THE FILE THE POD MOUNTS (delta review 012ccafa, D2).
/// The ConfigMap is mounted whole at one directory, so its key
/// `agent-role` is the file `<that directory>/agent-role`, and that is
/// what the door's default path must spell — not a sibling word's file:
/// `policy-check` beside it reads `enforce` on the live cluster today,
/// and a door reading THAT judges every agent by its row at the next
/// boot, which is the lockout this file is about.
#[test]
fn the_doors_default_path_is_the_agent_role_key_of_the_mounted_config_map() {
    let manifest = std::fs::read_to_string(repo_root().join(MANIFEST))
        .unwrap_or_else(|e| panic!("{MANIFEST}: {e}"));
    let objects = rbac::read_stream(MANIFEST, &manifest).unwrap_or_else(|e| panic!("{e}"));
    let named = |node: &Node| node.get("name").and_then(Node::str).map(str::to_string);
    let items = |node: &Node| match &node.value {
        Value::Seq(items) => items.clone(),
        _ => Vec::new(),
    };
    let mut mounts = Vec::new();
    for object in &objects {
        let walked = object.root.walk();
        let volumes: Vec<String> = walked
            .iter()
            .filter(|(k, _)| *k == "volumes")
            .flat_map(|(_, n)| items(n))
            .filter(|v| v.path(&["configMap", "name"]).and_then(Node::str) == Some(CONFIG_MAP))
            .map(|v| {
                assert!(
                    v.path(&["configMap", "items"]).is_none(),
                    "{MANIFEST} line {}: the {CONFIG_MAP} volume is mounted WHOLE, so a new key \
                     is a new file with no second edit; `items` would leave `agent-role` out",
                    v.line
                );
                named(&v).expect("a volume has a name")
            })
            .collect();
        for mount in walked
            .iter()
            .filter(|(k, _)| *k == "volumeMounts")
            .flat_map(|(_, n)| items(n))
            .filter(|m| named(m).is_some_and(|n| volumes.contains(&n)))
        {
            assert!(
                mount.get("subPath").is_none(),
                "{MANIFEST} line {}: the {CONFIG_MAP} mount takes no subPath",
                mount.line
            );
            mounts.push(
                mount
                    .get("mountPath")
                    .and_then(Node::str)
                    .unwrap_or_else(|| panic!("{MANIFEST} line {}: a mountPath", mount.line))
                    .to_string(),
            );
        }
    }
    assert!(
        !mounts.is_empty(),
        "{MANIFEST} mounts ConfigMap {CONFIG_MAP} into a container; if it does not, this pin is \
         reading the wrong manifest"
    );
    let file = boss_jobs::agents::ROW_ROLE_MODE_FILE;
    for dir in &mounts {
        assert_eq!(
            file,
            format!("{dir}/agent-role"),
            "the login door's default path is the `agent-role` key of ConfigMap {CONFIG_MAP}, \
             which {MANIFEST} mounts at {dir}"
        );
    }

    // ...and it is no sibling word's file, nor is its override theirs.
    let siblings = [
        (
            "the machine gate's mode",
            boss_core::machine_gate::DEFAULT_MODE_FILE,
            boss_core::machine_gate::MODE_FILE_ENV,
        ),
        (
            "the policy check's word",
            boss_policy::check_mode::DEFAULT_MODE_FILE,
            boss_policy::check_mode::MODE_FILE_ENV,
        ),
        (
            "the actor-role word",
            boss_policy_client::role_reader::ROLE_MODE_FILE,
            boss_policy_client::role_reader::ROLE_MODE_FILE_ENV,
        ),
    ];
    for (what, sibling_file, sibling_env) in siblings {
        assert_ne!(file, sibling_file, "the agent-role word is not {what}");
        assert_ne!(
            boss_jobs::agents::ROW_ROLE_MODE_FILE_ENV,
            sibling_env,
            "the agent-role word's override is not the one that moves {what}"
        );
        assert!(
            mounts
                .iter()
                .any(|dir| sibling_file.starts_with(&format!("{dir}/"))),
            "{what} ({sibling_file}) is a key of the same mounted ConfigMap; if it is not, the \
             sibling list above is stale"
        );
    }
    assert_eq!(
        boss_jobs::agents::ROW_ROLE_MODE_FILE_ENV,
        "BOSS_AGENT_ROLE_MODE_FILE"
    );
}

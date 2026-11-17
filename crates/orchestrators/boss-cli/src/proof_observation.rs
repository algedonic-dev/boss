//! Registered read observations never replace a car's proof or attempt.
use anyhow::{Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_OUTPUT: usize = 100_000;

// A Value map alone would silently keep the last duplicate key. Reject
// ambiguity at every depth before interpreting any observation binding.
struct Unique(Value);

impl<'de> serde::Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("one unambiguous JSON value")
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(Unique(v)) = seq.next_element::<Unique>()? {
                    values.push(v);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                    values.insert(key, map.next_value::<Unique>()?.0);
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        d.deserialize_any(Visitor)
    }
}

fn command(name: &str) -> Result<String> {
    if name.len() > 64
        || !name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || !name.rsplit_once("-v").is_some_and(|(_, version)| {
            version
                .parse::<u32>()
                .is_ok_and(|v| v > 0 && v.to_string() == version)
        })
    {
        bail!("one exact versioned registered observer name is required");
    }
    Ok(format!(
        "python3 -I -B infra/forge/proof-observation.py {name}"
    ))
}

fn validate(name: &str, raw: &str, exit: i32) -> Result<Value> {
    let body = serde_json::from_str::<Unique>(raw)?.0;
    let state = body.get("observation").and_then(Value::as_str);
    let expected = match state {
        Some("seen") => 0,
        Some("unseen") => 1,
        Some("unknown") => 75,
        _ => bail!("observer did not produce a declared observation"),
    };
    if body.get("schema").and_then(Value::as_u64) != Some(1)
        || body.get("kind").and_then(Value::as_str) != Some("registered-proof-observation")
        || exit != expected
    {
        bail!("observer schema, kind or process exit differs");
    }
    if exit != 75 {
        let contract = body
            .pointer("/contract/body")
            .ok_or_else(|| anyhow::anyhow!("registered contract absent"))?;
        let car = body
            .pointer("/original/car")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if body.pointer("/contract/name").and_then(Value::as_str) != Some(name)
            || uuid::Uuid::parse_str(car).is_err()
            || contract.get("car").and_then(Value::as_str) != Some(car)
            || contract.get("claim") != body.get("claim")
            || contract.get("branch") != body.pointer("/original/branch")
            || body
                .pointer("/original/proof_expect")
                .and_then(Value::as_str)
                .is_none_or(|v| v.trim().is_empty())
            || body
                .pointer("/original/converged_epoch")
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<u64>().ok())
                .is_none_or(|v| v == 0)
            || contract
                .get("claim")
                .and_then(Value::as_str)
                .is_none_or(|v| v.trim().is_empty())
            || contract
                .get("version")
                .and_then(Value::as_u64)
                .is_none_or(|v| !name.ends_with(&format!("-v{v}")))
            || [
                "/contract/sha256",
                "/source_head",
                "/original/merge_ref",
                "/original/whole_get_sha256",
                "/original/proof_history_sha256",
            ]
            .iter()
            .any(|p| {
                body.pointer(p).and_then(Value::as_str).is_none_or(|v| {
                    v.len()
                        != if *p == "/source_head" || *p == "/original/merge_ref" {
                            40
                        } else {
                            64
                        }
                        || !v
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            })
        {
            bail!("observation does not bind its exact contract, claim, original and artifact");
        }
        if exit == 0
            && body
                .get("witnesses")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        {
            bail!("seen observation lacks complete native witnesses");
        }
        if exit == 1 && !body.get("coverage").is_some_and(Value::is_object) {
            bail!("unseen observation lacks complete native coverage");
        }
    }
    Ok(body)
}

fn result(name: &str, outcome: &crate::prove::Outcome, context: Value) -> (i32, Value) {
    let judged = if outcome.missing_tools.is_empty() {
        validate(name, &outcome.stdout, outcome.exit).and_then(|body| {
            if outcome.exit != 75
                && (context
                    .get("user")
                    .and_then(Value::as_str)
                    .is_none_or(|v| v.is_empty() || v == "root")
                    || context
                        .get("uid")
                        .and_then(Value::as_str)
                        .and_then(|v| v.parse::<u32>().ok())
                        .is_none_or(|v| v == 0)
                    || context
                        .get("cwd")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    || context
                        .get("unreadable")
                        .and_then(Value::as_array)
                        .is_none_or(|v| !v.is_empty())
                    || context.get("tree_head") != body.get("source_head"))
            {
                bail!("native account, directory or artifact context is unavailable or differs");
            }
            Ok(body)
        })
    } else {
        Err(anyhow::anyhow!(
            "registered observer could not run every required tool"
        ))
    };
    let exit = if judged.is_ok() { outcome.exit } else { 75 };
    let body = json!({"schema":1,"kind":"protected-proof-observation","contract":name,
        "observation":judged.as_ref().ok().and_then(|v|v.get("observation")).cloned().unwrap_or(json!("unknown")),
        "result_accepted":judged.is_ok(),"diagnostic":judged.as_ref().err().map(|e|format!("{e:#}")),
        "raw_exit":outcome.exit,"stdout":outcome.stdout,"stderr":outcome.stderr,
        "missing_tools":outcome.missing_tools,"execution":context});
    if body.to_string().len() + 1 > MAX_OUTPUT {
        return (
            75,
            json!({"schema":1,"kind":"protected-proof-observation","observation":"unknown",
            "contract":name,"why":"whole output exceeds existing ops carrier; no full result is conserved",
            "stdout":{"complete":false,"bytes":outcome.stdout.len(),"sha256":hex::encode(Sha256::digest(outcome.stdout.as_bytes()))},
            "stderr":{"complete":false,"bytes":outcome.stderr.len(),"sha256":hex::encode(Sha256::digest(outcome.stderr.as_bytes()))}}),
        );
    }
    (exit, body)
}

pub(crate) fn run(name: &str) -> Result<()> {
    let probe = command(name)?;
    let base = crate::sor_client::resolve_jobs_base(None)?;
    let (shell, context, refused) =
        crate::prove::unattended_ground(crate::prove::Shell::unattended()?, None, &base);
    let execution = context.evidence();
    let answer = match refused {
        Some(why) => Err(anyhow::anyhow!(why)),
        None => crate::prove::execute_with(&probe, &shell),
    };
    let (exit, mut body) = match answer {
        Ok(outcome) => result(name, &outcome, execution),
        Err(error) => {
            let executed = error
                .downcast_ref::<crate::prove::EnvironmentRefusal>()
                .and_then(|r| r.executed());
            let mut body = match executed {
                Some(outcome) => result(name, outcome, execution).1,
                None => {
                    json!({"schema":1,"kind":"protected-proof-observation","contract":name,"execution":execution})
                }
            };
            body["observation"] = json!("unknown");
            body["result_accepted"] = json!(false);
            body["refusal"] = json!(format!("{error:#}"));
            (75, body)
        }
    };
    body["host"] = json!(crate::prove::host());
    if body.to_string().len() + 1 > MAX_OUTPUT {
        let raw = body.to_string();
        body = json!({"schema":1,"kind":"protected-proof-observation","observation":"unknown","contract":name,
            "why":"whole execution diagnostic exceeds existing ops carrier; complete result unavailable",
            "complete":false,"bytes":raw.len(),"sha256":hex::encode(Sha256::digest(raw.as_bytes()))});
        println!("{body}");
        std::process::exit(75);
    }
    println!("{body}");
    if exit != 0 {
        std::process::exit(exit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn registered_observation_is_a_distinct_one_argument_cli_door() {
        assert!(crate::Cli::try_parse_from(["boss", "observe-proof", "sponsorship-v1"]).is_ok());
        for extra in [
            "--probe",
            "--replace",
            "--record-probe",
            "--probe-anyway",
            "--expect",
        ] {
            assert!(
                crate::Cli::try_parse_from([
                    "boss",
                    "observe-proof",
                    "sponsorship-v1",
                    extra,
                    "true"
                ])
                .is_err()
            );
        }
    }

    #[test]
    fn arbitrary_script_names_and_unversioned_names_cannot_be_commands() {
        assert!(
            command("sponsorship-v1")
                .unwrap()
                .ends_with(" sponsorship-v1")
        );
        for name in [
            "",
            "../sponsorship-v1",
            "sponsorship-v1;true",
            "sponsorship",
            "sponsorship-v0",
            "sponsorship-v01",
            "SPONSORSHIP-v1",
        ] {
            assert!(command(name).is_err(), "{name}");
        }
    }

    fn positive() -> Value {
        json!({"schema":1,"kind":"registered-proof-observation","observation":"seen",
            "claim":"event only","contract":{"name":"control-v1","sha256":"a".repeat(64),
              "body":{"version":1,"car":"11111111-1111-4111-8111-111111111111","claim":"event only","branch":"feat/control"}},
            "original":{"car":"11111111-1111-4111-8111-111111111111","branch":"feat/control",
               "merge_ref":"a".repeat(40),"converged_epoch":"123","proof_expect":"original broader claim",
               "whole_get_sha256":"c".repeat(64),"proof_history_sha256":"d".repeat(64)},"source_head":"b".repeat(40),
            "witnesses":[{"id":"native"}]})
    }

    #[test]
    fn malformed_claim_identity_and_process_verdict_do_not_authorize_an_observation() {
        let body = positive();
        assert!(validate("control-v1", &body.to_string(), 0).is_ok());
        for (pointer, value) in [
            ("/schema", json!(true)),
            ("/observation", json!("proven")),
            ("/claim", json!("consent copied")),
            ("/contract/name", json!("other-v1")),
            ("/contract/sha256", json!("")),
            ("/source_head", json!("")),
            ("/original/car", json!("invalid")),
            ("/witnesses", json!([])),
        ] {
            let mut bad = body.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                validate("control-v1", &bad.to_string(), 0).is_err(),
                "{pointer}"
            );
        }
        for raw in ["", " \n", "{} {}", "true", "[]", "{", "null"] {
            assert!(validate("control-v1", raw, 0).is_err(), "{raw}");
        }
        assert!(validate("control-v1", &body.to_string(), 75).is_err());
    }

    #[test]
    fn failure_keeps_exact_streams_and_missing_tools_discard_even_a_seen_result() {
        let mut outcome = crate::prove::Outcome {
            exit: 0,
            stdout: positive().to_string() + "\n\n",
            stderr: "original stderr\n\n".into(),
            missing_tools: vec![],
            timing: None,
        };
        let context = json!({"user":"boss-probe","uid":"997","cwd":"/view","tree_head":"b".repeat(40),"unreadable":[]});
        let (exit, body) = result("control-v1", &outcome, context);
        assert_eq!(exit, 0);
        assert_eq!(body["stdout"], outcome.stdout);
        assert_eq!(body["stderr"], outcome.stderr);
        outcome.missing_tools.push("missing-tool".into());
        let (exit, body) = result("control-v1", &outcome, json!({}));
        assert_eq!(exit, 75);
        assert_eq!(body["observation"], "unknown");
        assert_eq!(body["stdout"], outcome.stdout);
        assert_eq!(body["stderr"], outcome.stderr);
        outcome.stdout = "x".repeat(MAX_OUTPUT);
        let (exit, body) = result("control-v1", &outcome, json!({}));
        assert_eq!(exit, 75);
        assert_eq!(body["stdout"]["complete"], false);
        assert!(body.to_string().len() < MAX_OUTPUT);
    }

    #[test]
    fn ambiguous_json_is_not_a_single_native_result() {
        let raw = positive().to_string();
        let duplicate = format!("{{\"schema\":0,{}", &raw[1..]);
        assert!(validate("control-v1", &duplicate, 0).is_err());
    }

    #[test]
    fn missing_or_foreign_execution_context_cannot_accept_a_seen_result() {
        let outcome = crate::prove::Outcome {
            exit: 0,
            stdout: positive().to_string(),
            stderr: String::new(),
            missing_tools: vec![],
            timing: None,
        };
        for context in [
            json!({}),
            json!({"user":"root","uid":"0","cwd":"/view","tree_head":"b".repeat(40),"unreadable":[]}),
            json!({"user":"boss-probe","uid":"997","cwd":"/view","tree_head":"c".repeat(40),"unreadable":[]}),
        ] {
            assert_eq!(result("control-v1", &outcome, context).0, 75);
        }
    }

    #[test]
    fn the_original_cut_and_complete_history_binding_are_required() {
        let body = positive();
        for (pointer, value) in [
            ("/original/converged_epoch", json!("0")),
            ("/original/merge_ref", json!("")),
            ("/original/proof_history_sha256", json!("")),
            ("/original/whole_get_sha256", json!("")),
            ("/original/branch", json!("foreign")),
            ("/original/proof_expect", json!(null)),
        ] {
            let mut bad = body.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                validate("control-v1", &bad.to_string(), 0).is_err(),
                "{pointer}"
            );
        }
    }
}

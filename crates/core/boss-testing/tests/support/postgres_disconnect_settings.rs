use boss_testing::repo_root;

const MANIFEST: &str = "infra/cluster/manifests/boss.yaml";

pub fn manifest_text() -> String {
    std::fs::read_to_string(repo_root().join(MANIFEST)).unwrap()
}

pub fn declared_settings() -> Vec<(String, String)> {
    settings_in(&manifest_text()).unwrap_or_else(|refusal| panic!("{MANIFEST}: {refusal}"))
}

/// A setting's name as PostgreSQL resolves it: names are case-insensitive,
/// and its start-up parser (ParseLongOption) reads `-` as `_`. Compared as
/// written, `-c FSYNC=off` passed a pin that refused `fsync` (1702a075).
fn normalised(name: &str) -> String {
    name.to_ascii_lowercase().replace('-', "_")
}

/// The jobs PostgreSQL StatefulSet's database container, the one place both
/// its start-up settings and the memory it requests are read from, so the
/// two cannot be read off different containers.
pub fn postgres_container(manifest: &str) -> Result<yaml_serde::Value, String> {
    let docs: Vec<yaml_serde::Value> = yaml_serde::Deserializer::from_str(manifest)
        .map(|doc| serde::Deserialize::deserialize(doc).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let postgres = docs
        .iter()
        .find(|doc| doc["kind"] == "StatefulSet" && doc["metadata"]["name"] == "postgres")
        .ok_or("no postgres StatefulSet")?;
    let containers = postgres["spec"]["template"]["spec"]["containers"]
        .as_sequence()
        .ok_or("the postgres StatefulSet declares no containers")?;
    match containers.as_slice() {
        [container] if container["name"] == "postgres" => Ok(container.clone()),
        _ => Err("the postgres StatefulSet must run exactly one container, postgres".into()),
    }
}

/// The `-c name=value` settings the jobs PostgreSQL StatefulSet starts with,
/// read from a manifest's text so a fixture is read exactly as the tree is.
/// Names come back normalised; a name declared twice in any spelling, or an
/// argument in any other shape (`-F`, `--name=value`, `-cname=value`), is
/// refused, because PostgreSQL would honour it where the pin cannot read it.
pub fn settings_in(manifest: &str) -> Result<Vec<(String, String)>, String> {
    let postgres = postgres_container(manifest)?;
    let args = postgres["args"]
        .as_sequence()
        .ok_or("the postgres container declares no args")?;
    if args.first().and_then(|arg| arg.as_str()) != Some("postgres") {
        return Err("the postgres container's args must start with postgres".into());
    }
    args[1..]
        .chunks(2)
        .try_fold(Vec::new(), |settings: Vec<(String, String)>, pair| {
            if pair.len() != 2 || pair[0].as_str() != Some("-c") {
                return Err(format!("{pair:?} is not a -c name=value pair"));
            }
            let setting = pair[1].as_str().ok_or("a setting must be a string")?;
            let (written, value) = setting
                .split_once('=')
                .ok_or_else(|| format!("{setting:?} has no value"))?;
            let key = normalised(written);
            if settings.iter().any(|(name, _)| *name == key) {
                return Err(format!("{key} is declared twice (again as {setting:?})"));
            }
            Ok(settings
                .into_iter()
                .chain([(key, value.to_owned())])
                .collect())
        })
}

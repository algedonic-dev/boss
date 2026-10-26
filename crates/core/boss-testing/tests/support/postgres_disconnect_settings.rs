use boss_testing::repo_root;

pub fn declared_settings() -> Vec<(String, String)> {
    let docs: Vec<yaml_serde::Value> = yaml_serde::Deserializer::from_str(
        &std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss.yaml")).unwrap(),
    )
    .map(|doc| serde::Deserialize::deserialize(doc).unwrap())
    .collect();
    let postgres = docs
        .iter()
        .find(|doc| doc["kind"] == "StatefulSet" && doc["metadata"]["name"] == "postgres")
        .unwrap();
    let args = postgres["spec"]["template"]["spec"]["containers"][0]["args"]
        .as_sequence()
        .unwrap();
    assert_eq!(args[0].as_str(), Some("postgres"));
    args[1..]
        .chunks(2)
        .map(|pair| {
            assert_eq!(pair.len(), 2);
            assert_eq!(pair[0].as_str(), Some("-c"));
            let (key, value) = pair[1].as_str().unwrap().split_once('=').unwrap();
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

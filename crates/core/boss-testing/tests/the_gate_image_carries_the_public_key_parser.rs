//! Gate 1e8 failed four deposit renderer positives because the CI image
//! omitted ssh-keygen while dev pod bootstrap had installed it.
//! tree-wide pin — the image and tool roster live outside this crate,
//! so their dependency contract must run in every scoped gate.
use boss_testing::repo_root;

#[test]
fn the_gate_image_declares_the_public_key_parser() {
    let root = repo_root();
    let tools =
        std::fs::read_to_string(root.join("infra/forge/boss-ci/required-tools.txt")).unwrap();
    assert!(
        tools.lines().any(|line| line == "ssh-keygen"),
        "the canonical gate tool roster must declare the public-key parser"
    );
    let image = std::fs::read_to_string(root.join("infra/forge/boss-ci/Dockerfile")).unwrap();
    let layer = image
        .split("\nRUN ")
        .find(|layer| layer.lines().any(|line| line.trim() == "openssh-client \\"))
        .expect("the gate image must install the public-key parser at build time");
    assert!(
        layer.contains("chmod u-s /usr/lib/openssh/ssh-keysign"),
        "installing the client must clear its unnecessary setuid helper in the same layer"
    );
}

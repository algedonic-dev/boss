//! The resident queue gate failed native API controls before sending a request:
//! the gate image had no real Kubernetes client. Check its image contract and
//! require image construction to execute its group cancellation control.
use boss_testing::repo_root;

const CLIENT_COPY: &str =
    "COPY --from=10.20.0.15:3000/david/alpine-k8s:1.33.3 /usr/bin/kubectl /usr/local/bin/kubectl";

#[test]
fn the_gate_image_declares_the_real_native_client() {
    let root = repo_root();
    let image = std::fs::read_to_string(root.join("infra/forge/boss-ci/Dockerfile")).unwrap();
    let tools =
        std::fs::read_to_string(root.join("infra/forge/boss-ci/required-tools.txt")).unwrap();
    assert!(
        image.lines().any(|line| line == CLIENT_COPY),
        "copy the existing mirrored real client into the gate image"
    );
    assert!(
        tools.lines().any(|line| line == "kubectl"),
        "locomotive must refuse a missing native client"
    );
    let mirror = std::fs::read_to_string(root.join("infra/forge/mirror-base-images.sh")).unwrap();
    assert!(mirror.contains("alpine/k8s:1.33.3|alpine-k8s:1.33.3"));
}

#[test]
fn the_gate_image_provides_external_group_cancellation() {
    let root = repo_root();
    let image = std::fs::read_to_string(root.join("infra/forge/boss-ci/Dockerfile")).unwrap();
    let tools =
        std::fs::read_to_string(root.join("infra/forge/boss-ci/required-tools.txt")).unwrap();
    assert!(
        image.contains("jq python3 procps"),
        "procps supplies external kill, independent of shell builtins"
    );
    assert!(tools.lines().any(|line| line == "kill"));
}

#[test]
fn the_image_build_checks_the_real_client_and_group_cancellation() {
    let root = repo_root();
    let image = std::fs::read_to_string(root.join("infra/forge/boss-ci/Dockerfile")).unwrap();
    let command = image
        .lines()
        .find_map(|line| line.strip_prefix("RUN kubectl version --client=true --output=json && "))
        .expect("the built image must execute its real client and cancellation control");
    assert!(command.starts_with("python3 -c '"));
    assert!(command.contains("/bin/kill"));
    assert!(command.contains("start_new_session=True"));
    assert!(command.contains("child.wait(timeout=5) == -15"));
    assert!(command.contains(
        "subprocess.run([\"/bin/kill\", \"-TERM\", \"--\", \"-\" + str(child.pid)], check=True)"
    ));
    // The source gate runs in the old image, which lacks both new tools.
    // Image construction owns execution of this command; the builder also
    // rehearses it locally, without claiming that an image has been built.
}

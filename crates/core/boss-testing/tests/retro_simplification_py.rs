use std::process::Command;

#[test]
fn the_retro_reads_individual_landed_cars_and_refuses_unknown_coverage() {
    let root = boss_testing::repo_root();
    let scratch = boss_testing::scratch_dir("retro-simplification");
    let out = Command::new("python3")
        .arg(root.join("infra/tests/retro_simplification_test.py"))
        .arg(root.join("infra/retro-simplification.py"))
        .arg(&scratch)
        .output()
        .expect("the Python fixture starts");
    println!("{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

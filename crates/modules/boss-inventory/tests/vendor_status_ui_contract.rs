use boss_inventory::types::{PoStatus, VendorInvoiceStatus};
use serde_json::json;
use std::process::{Command, Stdio};

// Discover the owning vocabulary rather than maintain another constant roster.
// A declaration this reader cannot interpret fails the pin, never omits a case.
fn native_statuses(kind: &str) -> Vec<String> {
    let source = include_str!("../src/types.rs");
    let start = format!("impl {kind} {{");
    let block = source
        .split_once(&start)
        .unwrap()
        .1
        .split_once("\n}\n")
        .unwrap()
        .0;
    let mut values: Vec<String> = block
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with("pub const ") {
                return None;
            }
            let value = line
                .split_once(" = \"")
                .unwrap()
                .1
                .strip_suffix("\";")
                .unwrap();
            assert!(
                !value.contains('\\'),
                "interpret changed native declaration before extending the pin"
            );
            Some(value.to_string())
        })
        .collect();
    assert!(!values.is_empty(), "the native vocabulary was not read");
    values.extend(["tenant-awaiting-import", "tenant-credit-review", ""].map(str::to_string));
    values
}

#[test]
fn vendor_ui_predicates_equal_the_owning_native_methods() {
    let pos = native_statuses("PoStatus");
    let bills = native_statuses("VendorInvoiceStatus");
    let input = json!({"pos":pos,"bills":bills});
    let expected = json!({
        "pos":pos.iter().map(|status| PoStatus::new(status).is_open()).collect::<Vec<_>>(),
        "bills":bills.iter().map(|status| VendorInvoiceStatus::new(status).is_paid()).collect::<Vec<_>>(),
    });
    let root = boss_testing::repo_root();
    let mut child = Command::new("bun")
        .arg(root.join("apps/web/tests/contracts/vendor-status-probe.ts"))
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    boss_testing::feed_stdin(&mut child, input.to_string().as_bytes());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "UI contract probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        actual, expected,
        "UI predicates drifted from actual native methods; input={input}"
    );
}

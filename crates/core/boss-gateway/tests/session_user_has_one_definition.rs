//! The downstream session user is one fact (cebf1588), not two copied
//! JSON builders that a new scope field can silently make disagree.

#[test]
fn downstream_session_scope_has_one_serialization_owner() {
    let owners = [
        ("session", include_str!("../src/session.rs")),
        ("role_headers", include_str!("../src/role_headers.rs")),
        ("passkey", include_str!("../src/passkey.rs")),
    ];
    for field in ["territory_account_ids", "direct_report_ids", "department"] {
        let key = format!("\"{field}\":");
        let copies: Vec<_> = owners
            .iter()
            .filter_map(|(name, source)| {
                let production = source.split("#[cfg(test)]").next().unwrap();
                let count = production.matches(&key).count();
                (count != 0).then_some((*name, count))
            })
            .collect();
        assert_eq!(
            copies,
            vec![("session", 1)],
            "the downstream {field} field must be owned once by Session, not copied by its consumers"
        );
    }
}

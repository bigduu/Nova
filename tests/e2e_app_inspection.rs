//! Explicit opt-in acceptance for an externally prepared, test-owned app.
//! Never invoke the default all-app discovery path or Nova's TCC bootstrap.
#![cfg(target_os = "macos")]

#[test]
#[ignore = "requires a running test-owned app and NOVA_TEST_APP_BUNDLE_ID"]
fn inspect_only_test_owned_application() {
    let Ok(bundle_id) = std::env::var("NOVA_TEST_APP_BUNDLE_ID") else {
        return;
    };
    assert!(
        bundle_id.starts_with("dev.nova.acceptance."),
        "only a dedicated Nova acceptance fixture is allowed"
    );
    let result = nova::app_inspection::inspect(Some(&bundle_id), true);
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    assert_eq!(result.apps.len(), 1, "test-owned app must be running");
    assert_eq!(
        result.apps[0].bundle_id.as_deref(),
        Some(bundle_id.as_str())
    );
    let details = serde_json::to_value(&result.apps[0].details).unwrap();
    assert!(
        !details["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue == "profile_limit"),
        "the fixture's one profile must be deduplicated across helpers"
    );
    for endpoint in details["endpoints"].as_array().unwrap() {
        let provenance = endpoint["provenance"].as_array().unwrap();
        let unique = provenance
            .iter()
            .map(|v| v.to_string())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), provenance.len());
    }
    if let Ok(expected) = std::env::var("NOVA_TEST_EXPECT_CDP") {
        match expected.as_str() {
            "1" => assert_eq!(result.apps[0].status, "browser_endpoint_available"),
            "0" => assert_eq!(result.apps[0].status, "no_endpoint_discovered"),
            _ => panic!("NOVA_TEST_EXPECT_CDP must be 0 or 1"),
        }
    }
}

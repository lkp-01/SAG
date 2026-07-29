use std::time::{Duration, Instant};

use serde_json::json;

#[tokio::test]
#[ignore = "requires two running sag-auth instances sharing isolated PostgreSQL and Redis"]
async fn both_instances_reject_a_token_after_remote_authorization_change() {
    let auth_a = std::env::var("SAG_TEST_AUTH_A_URL").expect("SAG_TEST_AUTH_A_URL is required");
    let auth_b = std::env::var("SAG_TEST_AUTH_B_URL").expect("SAG_TEST_AUTH_B_URL is required");
    let admin_token =
        std::env::var("SAG_TEST_AUTH_ADMIN_TOKEN").expect("SAG_TEST_AUTH_ADMIN_TOKEN is required");
    let username =
        std::env::var("SAG_TEST_AUTH_USERNAME").expect("SAG_TEST_AUTH_USERNAME is required");
    let password =
        std::env::var("SAG_TEST_AUTH_PASSWORD").expect("SAG_TEST_AUTH_PASSWORD is required");
    let client = reqwest::Client::new();

    let login: serde_json::Value = client
        .post(format!("{auth_a}/api/v1/auth/login"))
        .json(&json!({"username": username, "password": password}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let old_token = login["token"].as_str().unwrap().to_string();

    client
        .post(format!("{auth_b}/api/v1/users"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "username": username,
            "roles": ["ops"],
            "enabled": false
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut both_inactive = true;
        for base in [&auth_a, &auth_b] {
            let verification: serde_json::Value = client
                .post(format!("{base}/api/v1/auth/verify"))
                .json(&json!({"token": old_token}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            both_inactive &= verification["active"] == false;
        }
        if both_inactive {
            break;
        }
        assert!(Instant::now() < deadline, "revocation SLO exceeded");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    client
        .post(format!("{auth_b}/api/v1/users"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "username": username,
            "roles": ["ops"],
            "enabled": true
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let new_login: serde_json::Value = client
        .post(format!("{auth_b}/api/v1/auth/login"))
        .json(&json!({"username": username, "password": password}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(new_login["token"], old_token);
    assert_eq!(new_login["user"]["roles"], json!(["ops"]));
}

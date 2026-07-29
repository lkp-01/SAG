use anyhow::{bail, Context};
use reqwest::StatusCode;
use serde_json::json;

fn usage() -> &'static str {
    "usage:\n  reconcile_idempotency list [min_age_ms]\n  reconcile_idempotency show <scope_key>\n  reconcile_idempotency complete <scope_key> <version> <status> <reason> <result_body> --confirm COMPLETE\n  reconcile_idempotency release <scope_key> <version> <reason> --confirm RELEASE\n\nRequires SAG_RECONCILE_ADMIN_TOKEN; SAG_ADMIN_URL defaults to http://127.0.0.1:8090."
}

fn require_confirmation(args: &[String], expected: &str) -> anyhow::Result<()> {
    let confirmed = args
        .windows(2)
        .any(|pair| pair[0] == "--confirm" && pair[1] == expected);
    if !confirmed {
        bail!("explicit --confirm {expected} is required");
    }
    Ok(())
}

async fn print_response(response: reqwest::Response) -> anyhow::Result<()> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("Admin API returned {status}: {body}");
    }
    if status != StatusCode::NO_CONTENT {
        println!("{body}");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(action) = args.first().map(String::as_str) else {
        bail!(usage());
    };
    let base_url = std::env::var("SAG_ADMIN_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8090".into())
        .trim_end_matches('/')
        .to_string();
    let token = std::env::var("SAG_RECONCILE_ADMIN_TOKEN")
        .context("SAG_RECONCILE_ADMIN_TOKEN is required")?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let request = match action {
        "list" => {
            let min_age_ms = args.get(1).map(String::as_str).unwrap_or("300000");
            client.get(format!(
                "{base_url}/api/v1/idempotency/indeterminate?min_age_ms={min_age_ms}"
            ))
        }
        "show" => {
            let scope = args.get(1).context("show requires <scope_key>")?;
            client.get(format!("{base_url}/api/v1/idempotency/{scope}"))
        }
        "complete" => {
            if args.len() < 8 {
                bail!(usage());
            }
            require_confirmation(&args, "COMPLETE")?;
            let scope = &args[1];
            let version = args[2].parse::<i64>().context("invalid version")?;
            let status = args[3].parse::<u32>().context("invalid status")?;
            client
                .post(format!("{base_url}/api/v1/idempotency/{scope}/complete"))
                .json(&json!({
                    "expected_version": version,
                    "status_code": status,
                    "headers_json": "{}",
                    "reason": args[4],
                    "result_body": args[5],
                    "confirmation": "COMPLETE"
                }))
        }
        "release" => {
            if args.len() < 6 {
                bail!(usage());
            }
            require_confirmation(&args, "RELEASE")?;
            let scope = &args[1];
            let version = args[2].parse::<i64>().context("invalid version")?;
            client
                .post(format!("{base_url}/api/v1/idempotency/{scope}/release"))
                .json(&json!({
                    "expected_version": version,
                    "reason": args[3],
                    "confirmation": "RELEASE"
                }))
        }
        _ => bail!(usage()),
    };

    let response = request
        .bearer_auth(token)
        .send()
        .await
        .context("failed to call Admin reconciliation API")?;
    print_response(response).await
}

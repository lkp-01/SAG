use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use sag_tunnel_proto::{ForwardRequest, ForwardResponse};

#[derive(Clone)]
pub struct AppState {
    readiness: sag_service_health::Readiness,
}

#[derive(Debug)]
pub struct StubForwardError;

impl fmt::Display for StubForwardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("stub forward must not run in queue state tests")
    }
}

pub async fn forward_request(
    _state: &AppState,
    _request: ForwardRequest,
) -> Result<ForwardResponse, StubForwardError> {
    Err(StubForwardError)
}

#[path = "../src/queue.rs"]
#[allow(dead_code)]
mod queue;

use queue::{ClaimDecision, EnqueueError, QueueConfig, QueueRuntime};

fn config(redis_url: String, key_prefix: String) -> QueueConfig {
    QueueConfig {
        redis_url,
        sentinel_urls: Vec::new(),
        sentinel_service: None,
        redis_connect_timeout_ms: 500,
        redis_command_timeout_ms: 3_000,
        redis_reconnect_retries: 2,
        redis_reconnect_base_ms: 10,
        redis_reconnect_max_ms: 50,
        key_prefix,
        soft_inflight: 1,
        hard_inflight: 4,
        max_queue_len: 5,
        max_body_bytes: 1024,
        queue_ttl_sec: 60,
        worker_concurrency: 1,
        max_result_body_bytes: 1024,
        poll_min_interval_ms: 10,
        dedup_ttl_sec: 60,
        reclaim_idle_ms: 25,
        max_forward_deadline_ms: 10,
        reclaim_jitter_margin_ms: 10,
        max_attempts: 3,
    }
}

fn request(id: impl Into<String>, method: &str, idempotency_key: &str) -> ForwardRequest {
    ForwardRequest {
        request_id: id.into(),
        attempt_id: uuid::Uuid::new_v4().to_string(),
        deadline_unix_ms: 0,
        idempotency_key: idempotency_key.into(),
        app_id: "app-queue-test".into(),
        method: method.into(),
        path: "/mutation".into(),
        headers: HashMap::new(),
        body: Vec::new(),
        stream_epoch: String::new(),
    }
}

async fn connection(redis_url: &str) -> ConnectionManager {
    ConnectionManager::new(redis::Client::open(redis_url).unwrap())
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated Redis 7 at SAG_TEST_REDIS_URL"]
async fn redis_queue_kill_point_matrix() {
    let redis_url = std::env::var("SAG_TEST_REDIS_URL")
        .expect("SAG_TEST_REDIS_URL must point to an isolated Redis 7 database");
    let mut admin = connection(&redis_url).await;
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut admin)
        .await
        .unwrap();
    let prefix = format!("sag:test:queue:{}", uuid::Uuid::new_v4());
    let runtime = QueueRuntime::connect(config(redis_url.clone(), prefix.clone()))
        .await
        .unwrap();

    // 1. Capacity check and job creation are one atomic operation under concurrency.
    let mut enqueues = Vec::new();
    for index in 0..32 {
        let runtime = Arc::clone(&runtime);
        enqueues.push(tokio::spawn(async move {
            let request = request(
                format!("capacity-{index}"),
                "POST",
                &format!("idem-{index}"),
            );
            (request.request_id.clone(), runtime.enqueue(&request).await)
        }));
    }
    let mut accepted = Vec::new();
    let mut rejected = 0;
    for enqueue in enqueues {
        let (id, result) = enqueue.await.unwrap();
        match result {
            Ok(()) => accepted.push(id),
            Err(EnqueueError::OverCapacity) => rejected += 1,
            other => panic!("unexpected enqueue result: {other:?}"),
        }
    }
    assert_eq!(accepted.len(), 5);
    assert_eq!(rejected, 27);
    let length: usize = admin.xlen(runtime.stream_key()).await.unwrap();
    assert_eq!(length, 5);
    for id in &accepted {
        let exists: bool = admin.exists(runtime.job_key(id)).await.unwrap();
        assert!(exists);
    }

    // Clear the capacity fixture without sharing stream/group state with later cases.
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut admin)
        .await
        .unwrap();
    runtime.ensure_group().await.unwrap();

    // 2. A delivery abandoned after XREADGROUP is recovered by XAUTOCLAIM.
    let abandoned = request("abandoned", "POST", "idem-abandoned");
    runtime.enqueue(&abandoned).await.unwrap();
    let first_delivery = runtime.read_batch("killed-worker").await.unwrap();
    assert_eq!(first_delivery.len(), 1);
    tokio::time::sleep(Duration::from_millis(40)).await;
    let reclaimed = runtime.reclaim_batch("replacement-worker").await.unwrap();
    assert_eq!(reclaimed, first_delivery);

    // 3. A persisted terminal result is ACKed without a second dispatch.
    assert_eq!(
        runtime
            .prepare_delivery(&reclaimed[0].0, "abandoned", &reclaimed[0].1)
            .await
            .unwrap(),
        ClaimDecision::Process { attempt: 1 }
    );
    let attempt: i64 = admin
        .hget(runtime.job_key("abandoned"), "attempt")
        .await
        .unwrap();
    let claimed_at_ms: i64 = admin
        .hget(runtime.job_key("abandoned"), "claimed_at_ms")
        .await
        .unwrap();
    assert_eq!(attempt, 1);
    assert!(claimed_at_ms > 0);
    let _: () = admin
        .hset(runtime.job_key("abandoned"), "status", "done")
        .await
        .unwrap();
    assert_eq!(
        runtime
            .prepare_delivery(&reclaimed[0].0, "abandoned", &reclaimed[0].1)
            .await
            .unwrap(),
        ClaimDecision::Terminal
    );
    runtime
        .ack_terminal(&reclaimed[0].0, "abandoned")
        .await
        .unwrap();
    assert_eq!(runtime.pending_count().await.unwrap(), 0);

    // 4. If DLQ persistence fails, the original PEL entry must remain unacked.
    let failed = request("dlq-failure", "POST", "idem-dlq-failure");
    runtime.enqueue(&failed).await.unwrap();
    let delivery = runtime.read_batch("failure-worker").await.unwrap();
    assert_eq!(delivery.len(), 1);
    let _: () = admin.set(runtime.dlq_key(), "wrong-type").await.unwrap();
    assert!(runtime
        .complete_failure(
            &delivery[0].0,
            "dlq-failure",
            &delivery[0].1,
            "forced failure"
        )
        .await
        .is_err());
    assert_eq!(runtime.pending_count().await.unwrap(), 1);
    let _: () = admin.del(runtime.dlq_key()).await.unwrap();
    runtime
        .complete_failure(
            &delivery[0].0,
            "dlq-failure",
            &delivery[0].1,
            "forced failure",
        )
        .await
        .unwrap();
    assert_eq!(runtime.pending_count().await.unwrap(), 0);

    let exhausted = request("attempt-exhausted", "POST", "idem-attempt-exhausted");
    runtime.enqueue(&exhausted).await.unwrap();
    let delivery = runtime.read_batch("attempt-worker").await.unwrap();
    for expected_attempt in 1..=3 {
        assert_eq!(
            runtime
                .prepare_delivery(&delivery[0].0, "attempt-exhausted", &delivery[0].1)
                .await
                .unwrap(),
            ClaimDecision::Process {
                attempt: expected_attempt
            }
        );
    }
    assert_eq!(
        runtime
            .prepare_delivery(&delivery[0].0, "attempt-exhausted", &delivery[0].1)
            .await
            .unwrap(),
        ClaimDecision::DeadLettered
    );
    assert_eq!(runtime.pending_count().await.unwrap(), 0);
    let status: String = admin
        .hget(runtime.job_key("attempt-exhausted"), "status")
        .await
        .unwrap();
    assert_eq!(status, "dlq");

    // 5. Dedup dependency errors are errors, never implicit permission to dispatch.
    if redis_url.starts_with("redis://127.0.0.1:") && !redis_url.contains('@') {
        let restricted_user = format!("sag_dedup_{}", uuid::Uuid::new_v4().simple());
        redis::cmd("ACL")
            .arg("SETUSER")
            .arg(&restricted_user)
            .arg("on")
            .arg(">dedup-test-password")
            .arg(format!("~{prefix}:restricted:*"))
            .arg("+@all")
            .arg("-set")
            .query_async::<()>(&mut admin)
            .await
            .unwrap();
        let restricted_url = redis_url.replacen(
            "redis://",
            &format!("redis://{restricted_user}:dedup-test-password@"),
            1,
        );
        let restricted =
            QueueRuntime::connect(config(restricted_url, format!("{prefix}:restricted")))
                .await
                .unwrap();
        assert!(restricted
            .try_claim_dedup("mutation-scope", "request-id")
            .await
            .is_err());
        redis::cmd("ACL")
            .arg("DELUSER")
            .arg(&restricted_user)
            .query_async::<i64>(&mut admin)
            .await
            .unwrap();
    }

    // 6. Reclaim idle must exceed the complete forward deadline plus jitter.
    let mut invalid = config(redis_url.clone(), format!("{prefix}:invalid"));
    invalid.reclaim_idle_ms = invalid.max_forward_deadline_ms + invalid.reclaim_jitter_margin_ms;
    assert!(QueueRuntime::connect(invalid).await.is_err());

    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut admin)
        .await
        .unwrap();
}

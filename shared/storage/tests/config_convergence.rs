use std::path::{Path, PathBuf};

use shared_storage::{
    ensure_store_schema, AuditLogRecord, AuditLogsStore, ConfigSyncJobDraft, ConfigSyncOperation,
    ConfigSyncStatus, ConfigSyncStore, IntranetUpstreamRecord, PostgresPoolConfig, PostgresStore,
    RoutesStore, SecurityMutation, SqliteStore, StorageError, StorageStore, TunnelRouteRecord,
};

const SNAPSHOT_HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SNAPSHOT_HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn temp_db(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "sag-config-convergence-{name}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn sqlite_store(path: &Path) -> StorageStore {
    StorageStore::Sqlite(SqliteStore::new(path.to_string_lossy().to_string()))
}

fn delete_route_job(generation: i64, due_at_ms: i64) -> ConfigSyncJobDraft {
    ConfigSyncJobDraft {
        generation,
        target: "APISIX".into(),
        resource_type: "ROUTE".into(),
        resource_id: "app-001".into(),
        app_id: "app-001".into(),
        operation: ConfigSyncOperation::Delete,
        payload_json: None,
        next_attempt_at_ms: due_at_ms,
    }
}

fn mutation_audit(id: &str, app_id: &str) -> AuditLogRecord {
    let mut audit = AuditLogRecord::management(
        "config-convergence-test",
        "admin",
        app_id,
        "/api/v1/agent/routes",
        "PUT",
    );
    audit.id = id.into();
    audit
}

async fn apply_mutation(store: &StorageStore, id: &str, mutation: SecurityMutation) {
    let app_id = match &mutation {
        SecurityMutation::UpsertTunnelRoute(route) => route.app_id.as_str(),
        SecurityMutation::UpsertIntranetUpstream(upstream) => upstream.app_id.as_str(),
        _ => "",
    };
    AuditLogsStore::apply_security_mutation(store, &mutation, &mutation_audit(id, app_id))
        .await
        .unwrap();
}

fn route(host: &str, app_id: &str) -> TunnelRouteRecord {
    TunnelRouteRecord {
        host: host.into(),
        app_id: app_id.into(),
        connector_endpoint: "connector:stream".into(),
        require_healthy_tunnel: true,
    }
}

fn upstream(app_id: &str) -> IntranetUpstreamRecord {
    IntranetUpstreamRecord {
        app_id: app_id.into(),
        upstream: "apisix-upstream:8080".into(),
        scheme: "http".into(),
    }
}

#[tokio::test]
async fn schema_starts_at_generation_zero_with_an_empty_snapshot() {
    let path = temp_db("generation-zero");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    assert_eq!(
        ConfigSyncStore::current_generation(&store).await.unwrap(),
        0
    );
    let snapshot = ConfigSyncStore::load_route_snapshot(&store).await.unwrap();
    assert_eq!(snapshot.generation, 0);
    assert!(snapshot.routes.is_empty());

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn invalid_upstream_is_rejected_before_generation_audit_or_outbox_commit() {
    let path = temp_db("invalid-upstream");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();
    let mutation = SecurityMutation::UpsertIntranetUpstream(IntranetUpstreamRecord {
        app_id: "app-a".into(),
        upstream: "missing-port".into(),
        scheme: "ftp".into(),
    });
    let error = AuditLogsStore::apply_security_mutation(
        &store,
        &mutation,
        &mutation_audit("invalid-upstream-audit", "app-a"),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, StorageError::Validation(_)));
    assert_eq!(
        ConfigSyncStore::current_generation(&store).await.unwrap(),
        0
    );
    assert!(ConfigSyncStore::list_jobs(&store).await.unwrap().is_empty());
    assert!(RoutesStore::get_intranet_upstream(&store, "app-a")
        .await
        .unwrap()
        .is_none());

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn conflicting_connector_config_for_one_app_is_rejected_atomically() {
    let path = temp_db("connector-conflict");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();
    apply_mutation(
        &store,
        "first-route",
        SecurityMutation::UpsertTunnelRoute(route("one.internal", "app-a")),
    )
    .await;
    let conflict = SecurityMutation::UpsertTunnelRoute(TunnelRouteRecord {
        host: "two.internal".into(),
        app_id: "app-a".into(),
        connector_endpoint: "different-connector:stream".into(),
        require_healthy_tunnel: true,
    });
    let error = AuditLogsStore::apply_security_mutation(
        &store,
        &conflict,
        &mutation_audit("conflicting-route", "app-a"),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, StorageError::Conflict(_)));
    assert_eq!(
        ConfigSyncStore::current_generation(&store).await.unwrap(),
        1
    );
    assert_eq!(RoutesStore::load_all(&store).await.unwrap().len(), 1);
    assert_eq!(ConfigSyncStore::list_jobs(&store).await.unwrap().len(), 1);

    // Multiple hosts remain supported when their app-scoped routing fields
    // are identical.
    apply_mutation(
        &store,
        "consistent-route",
        SecurityMutation::UpsertTunnelRoute(route("two.internal", "app-a")),
    )
    .await;
    assert_eq!(
        ConfigSyncStore::current_generation(&store).await.unwrap(),
        2
    );
    assert_eq!(RoutesStore::load_all(&store).await.unwrap().len(), 2);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn delete_job_persists_and_claim_failure_retry_are_leased() {
    let path = temp_db("job-retry");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    let inserted = ConfigSyncStore::enqueue_job(&store, &delete_route_job(3, 100), 10)
        .await
        .unwrap();
    assert_eq!(inserted.operation, ConfigSyncOperation::Delete);
    assert_eq!(inserted.status, ConfigSyncStatus::Pending);
    assert!(inserted.payload_json.is_none());

    // Re-open through a new store value to prove the job is durable rather than
    // retained by an in-memory queue.
    let reopened = sqlite_store(&path);
    assert_eq!(
        ConfigSyncStore::list_jobs(&reopened).await.unwrap().len(),
        1
    );
    assert!(
        ConfigSyncStore::claim_due_jobs(&reopened, "worker-a", 99, 50, 10)
            .await
            .unwrap()
            .is_empty()
    );

    let first = ConfigSyncStore::claim_due_jobs(&reopened, "worker-a", 100, 50, 10)
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].attempt_count, 1);
    assert_eq!(first[0].lease_owner.as_deref(), Some("worker-a"));
    assert_eq!(first[0].lease_expires_at_ms, Some(150));
    assert!(
        ConfigSyncStore::claim_due_jobs(&reopened, "worker-b", 120, 50, 10)
            .await
            .unwrap()
            .is_empty()
    );

    assert!(!ConfigSyncStore::mark_failed(
        &reopened,
        &inserted.job_id,
        "wrong-worker",
        "APISIX 500",
        125,
        200,
    )
    .await
    .unwrap());
    assert!(ConfigSyncStore::mark_failed(
        &reopened,
        &inserted.job_id,
        "worker-a",
        "APISIX 500",
        125,
        200,
    )
    .await
    .unwrap());
    let failed = ConfigSyncStore::get_job(&reopened, &inserted.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, ConfigSyncStatus::Failed);
    assert_eq!(failed.last_error.as_deref(), Some("APISIX 500"));
    assert!(failed.lease_owner.is_none());
    assert!(
        ConfigSyncStore::claim_due_jobs(&reopened, "worker-b", 199, 50, 10)
            .await
            .unwrap()
            .is_empty()
    );

    let retry = ConfigSyncStore::claim_due_jobs(&reopened, "worker-b", 200, 50, 10)
        .await
        .unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].attempt_count, 2);
    assert!(
        ConfigSyncStore::mark_applied(&reopened, &inserted.job_id, "worker-b", 210)
            .await
            .unwrap()
    );
    let applied = ConfigSyncStore::get_job(&reopened, &inserted.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(applied.status, ConfigSyncStatus::Applied);
    assert_eq!(applied.applied_at_ms, Some(210));
    assert!(
        ConfigSyncStore::claim_due_jobs(&reopened, "worker-c", 1_000, 50, 10)
            .await
            .unwrap()
            .is_empty()
    );

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn unknown_external_failure_retains_lease_until_quarantine_expires() {
    let path = temp_db("unknown-outcome-quarantine");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();
    let job = ConfigSyncStore::enqueue_job(&store, &delete_route_job(1, 10), 10)
        .await
        .unwrap();
    ConfigSyncStore::claim_due_jobs(&store, "worker-a", 10, 100, 1)
        .await
        .unwrap();

    assert!(ConfigSyncStore::mark_failed_retaining_lease(
        &store,
        &job.job_id,
        "worker-a",
        "  APISIX outcome unknown\nupstream closed  ",
        20,
        30,
    )
    .await
    .unwrap());
    let quarantined = ConfigSyncStore::get_job(&store, &job.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(quarantined.status, ConfigSyncStatus::Failed);
    assert_eq!(
        quarantined.last_error.as_deref(),
        Some("APISIX outcome unknown upstream closed")
    );
    assert_eq!(quarantined.lease_owner.as_deref(), Some("worker-a"));
    assert_eq!(quarantined.lease_expires_at_ms, Some(110));
    assert!(
        ConfigSyncStore::claim_due_jobs(&store, "worker-b", 109, 100, 1)
            .await
            .unwrap()
            .is_empty()
    );
    let retry = ConfigSyncStore::claim_due_jobs(&store, "worker-b", 110, 100, 1)
        .await
        .unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].job_id, job.job_id);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn newer_work_supersedes_older_non_terminal_work_for_the_resource() {
    let path = temp_db("job-supersede");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    let upsert = ConfigSyncJobDraft {
        generation: 1,
        target: "APISIX".into(),
        resource_type: "ROUTE".into(),
        resource_id: "app-001".into(),
        app_id: "app-001".into(),
        operation: ConfigSyncOperation::Upsert,
        payload_json: Some("{\"id\":\"sag-route-app-001\"}".into()),
        next_attempt_at_ms: 10,
    };
    let old = ConfigSyncStore::enqueue_job(&store, &upsert, 10)
        .await
        .unwrap();
    let delete = ConfigSyncStore::enqueue_job(&store, &delete_route_job(2, 20), 20)
        .await
        .unwrap();

    let rows = ConfigSyncStore::list_jobs(&store).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].job_id, old.job_id);
    assert_eq!(rows[0].superseded_by_generation, Some(2));
    assert_eq!(rows[1].job_id, delete.job_id);
    assert_eq!(rows[1].operation, ConfigSyncOperation::Delete);

    let claimed = ConfigSyncStore::claim_due_jobs(&store, "worker", 20, 50, 10)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].job_id, delete.job_id);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn superseded_inflight_work_serializes_newer_resource_io_until_release() {
    let path = temp_db("resource-lease-serialization");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    let old = ConfigSyncStore::enqueue_job(&store, &delete_route_job(1, 10), 10)
        .await
        .unwrap();
    let claimed = ConfigSyncStore::claim_due_jobs(&store, "old-worker", 10, 100, 10)
        .await
        .unwrap();
    assert_eq!(claimed[0].job_id, old.job_id);

    let newer = ConfigSyncStore::enqueue_job(&store, &delete_route_job(2, 20), 20)
        .await
        .unwrap();
    let old_after_supersede = ConfigSyncStore::get_job(&store, &old.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old_after_supersede.superseded_by_generation, Some(2));
    assert_eq!(
        old_after_supersede.lease_owner.as_deref(),
        Some("old-worker")
    );
    assert!(
        ConfigSyncStore::claim_due_jobs(&store, "new-worker", 30, 100, 10)
            .await
            .unwrap()
            .is_empty()
    );

    assert!(
        ConfigSyncStore::release_lease(&store, &old.job_id, "old-worker", 40)
            .await
            .unwrap()
    );
    let next = ConfigSyncStore::claim_due_jobs(&store, "new-worker", 40, 100, 10)
        .await
        .unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].job_id, newer.job_id);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn route_and_route_id_jobs_for_one_app_share_a_single_io_lease() {
    let path = temp_db("app-scoped-lease");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    let route = ConfigSyncStore::enqueue_job(&store, &delete_route_job(1, 10), 10)
        .await
        .unwrap();
    let route_id = ConfigSyncStore::enqueue_job(
        &store,
        &ConfigSyncJobDraft {
            generation: 1,
            target: "APISIX".into(),
            resource_type: "ROUTE_ID".into(),
            resource_id: "sag-route-legacy-app-001".into(),
            app_id: "app-001".into(),
            operation: ConfigSyncOperation::Delete,
            payload_json: None,
            next_attempt_at_ms: 11,
        },
        11,
    )
    .await
    .unwrap();

    let first = ConfigSyncStore::claim_due_jobs(&store, "worker-a", 20, 100, 10)
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].job_id, route.job_id);
    assert!(
        ConfigSyncStore::claim_due_jobs(&store, "worker-b", 20, 100, 10)
            .await
            .unwrap()
            .is_empty()
    );

    assert!(
        ConfigSyncStore::mark_applied(&store, &route.job_id, "worker-a", 21)
            .await
            .unwrap()
    );
    let second = ConfigSyncStore::claim_due_jobs(&store, "worker-b", 21, 100, 10)
        .await
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].job_id, route_id.job_id);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn newer_generation_supersedes_applied_history_and_is_the_only_current_job() {
    let path = temp_db("one-current-generation");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    let old = ConfigSyncStore::enqueue_job(&store, &delete_route_job(1, 10), 10)
        .await
        .unwrap();
    ConfigSyncStore::claim_due_jobs(&store, "worker", 10, 50, 1)
        .await
        .unwrap();
    assert!(
        ConfigSyncStore::mark_applied(&store, &old.job_id, "worker", 11)
            .await
            .unwrap()
    );

    let current = ConfigSyncStore::enqueue_job(&store, &delete_route_job(2, 20), 20)
        .await
        .unwrap();
    let jobs = ConfigSyncStore::list_jobs(&store).await.unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].superseded_by_generation, Some(2));
    assert_eq!(jobs[1].job_id, current.job_id);
    assert!(jobs[1].superseded_by_generation.is_none());

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn reconciliation_requeues_an_applied_job_in_the_same_generation() {
    let path = temp_db("requeue-applied");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    let draft = delete_route_job(1, 10);
    let job = ConfigSyncStore::enqueue_job(&store, &draft, 10)
        .await
        .unwrap();
    ConfigSyncStore::claim_due_jobs(&store, "worker", 10, 50, 1)
        .await
        .unwrap();
    assert!(
        ConfigSyncStore::mark_applied(&store, &job.job_id, "worker", 20)
            .await
            .unwrap()
    );

    let mut repair = draft;
    repair.next_attempt_at_ms = 30;
    assert!(ConfigSyncStore::requeue_job(&store, &repair, 30)
        .await
        .unwrap());
    let repaired = ConfigSyncStore::get_job(&store, &job.job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(repaired.status, ConfigSyncStatus::Pending);
    assert!(repaired.applied_at_ms.is_none());
    assert_eq!(repaired.next_attempt_at_ms, 30);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn agent_ack_never_moves_applied_generation_backwards() {
    let path = temp_db("monotonic-ack");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    ConfigSyncStore::ack_agent_generation(
        &store,
        "edge-agent-a",
        7,
        Some(SNAPSHOT_HASH_A),
        700,
        710,
    )
    .await
    .unwrap();
    ConfigSyncStore::ack_agent_generation(
        &store,
        "edge-agent-a",
        6,
        Some(SNAPSHOT_HASH_B),
        900,
        910,
    )
    .await
    .unwrap();
    let after_old = ConfigSyncStore::get_agent_apply(&store, "edge-agent-a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_old.applied_generation, 7);
    assert_eq!(after_old.snapshot_hash.as_deref(), Some(SNAPSHOT_HASH_A));
    assert_eq!(after_old.applied_at_ms, 700);
    assert_eq!(after_old.reported_at_ms, 710);

    ConfigSyncStore::ack_agent_generation(
        &store,
        "edge-agent-a",
        8,
        Some(SNAPSHOT_HASH_B),
        800,
        920,
    )
    .await
    .unwrap();
    let rows = ConfigSyncStore::list_agent_applies(&store).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].applied_generation, 8);
    assert_eq!(rows[0].snapshot_hash.as_deref(), Some(SNAPSHOT_HASH_B));
    assert_eq!(rows[0].applied_at_ms, 800);
    assert_eq!(rows[0].reported_at_ms, 920);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn agent_ack_rejects_conflicting_hash_for_the_same_generation() {
    let path = temp_db("ack-hash-conflict");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    ConfigSyncStore::ack_agent_generation(
        &store,
        "edge-agent-conflict",
        4,
        Some(SNAPSHOT_HASH_A),
        400,
        410,
    )
    .await
    .unwrap();
    let refreshed = ConfigSyncStore::ack_agent_generation(
        &store,
        "edge-agent-conflict",
        4,
        Some(SNAPSHOT_HASH_A),
        999,
        420,
    )
    .await
    .unwrap();
    assert_eq!(refreshed.applied_at_ms, 400);
    assert_eq!(refreshed.reported_at_ms, 420);

    let error = ConfigSyncStore::ack_agent_generation(
        &store,
        "edge-agent-conflict",
        4,
        Some(SNAPSHOT_HASH_B),
        500,
        430,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, StorageError::Conflict(_)));

    let persisted = ConfigSyncStore::get_agent_apply(&store, "edge-agent-conflict")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.snapshot_hash.as_deref(), Some(SNAPSHOT_HASH_A));
    assert_eq!(persisted.reported_at_ms, 420);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn hashless_agent_ack_rows_remain_compatible_during_upgrade() {
    let path = temp_db("ack-hashless-compatibility");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    ConfigSyncStore::ack_agent_generation(&store, "legacy-agent", 3, None, 300, 310)
        .await
        .unwrap();
    let upgraded = ConfigSyncStore::ack_agent_generation(
        &store,
        "legacy-agent",
        3,
        Some(SNAPSHOT_HASH_A),
        999,
        320,
    )
    .await
    .unwrap();
    assert_eq!(upgraded.snapshot_hash.as_deref(), Some(SNAPSHOT_HASH_A));
    assert_eq!(upgraded.applied_at_ms, 300);

    let hashless_refresh =
        ConfigSyncStore::ack_agent_generation(&store, "legacy-agent", 3, None, 999, 330)
            .await
            .unwrap();
    assert_eq!(
        hashless_refresh.snapshot_hash.as_deref(),
        Some(SNAPSHOT_HASH_A)
    );

    let newer_hashless =
        ConfigSyncStore::ack_agent_generation(&store, "legacy-agent", 4, None, 400, 410)
            .await
            .unwrap();
    assert_eq!(newer_hashless.applied_generation, 4);
    assert!(newer_hashless.snapshot_hash.is_none());

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn agent_ack_rejects_malformed_snapshot_hashes() {
    let path = temp_db("ack-invalid-hash");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    for invalid_hash in ["abc".to_owned(), SNAPSHOT_HASH_A.to_uppercase()] {
        let error = ConfigSyncStore::ack_agent_generation(
            &store,
            "invalid-hash-agent",
            1,
            Some(&invalid_hash),
            100,
            110,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, StorageError::Invariant(_)));
    }
    assert!(
        ConfigSyncStore::get_agent_apply(&store, "invalid-hash-agent")
            .await
            .unwrap()
            .is_none()
    );

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn sqlite_schema_upgrade_adds_nullable_snapshot_hash_to_legacy_ack_table() {
    let path = temp_db("ack-schema-upgrade");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE agent_config_applies ( \
                   agent_id TEXT PRIMARY KEY, \
                   applied_generation INTEGER NOT NULL, \
                   applied_at_ms INTEGER NOT NULL, \
                   reported_at_ms INTEGER NOT NULL \
                 ); \
                 INSERT INTO agent_config_applies VALUES ('legacy-agent', 9, 900, 910);",
            )
            .unwrap();
    }

    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();
    let legacy = ConfigSyncStore::get_agent_apply(&store, "legacy-agent")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(legacy.applied_generation, 9);
    assert!(legacy.snapshot_hash.is_none());

    let upgraded = ConfigSyncStore::ack_agent_generation(
        &store,
        "legacy-agent",
        9,
        Some(SNAPSHOT_HASH_A),
        999,
        920,
    )
    .await
    .unwrap();
    assert_eq!(upgraded.snapshot_hash.as_deref(), Some(SNAPSHOT_HASH_A));

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn stale_agent_ack_rows_are_pruned_by_reported_time() {
    let path = temp_db("ack-retention");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    ConfigSyncStore::ack_agent_generation(&store, "expired-agent", 1, None, 100, 100)
        .await
        .unwrap();
    ConfigSyncStore::ack_agent_generation(&store, "active-agent", 2, None, 200, 200)
        .await
        .unwrap();
    ConfigSyncStore::ack_agent_generation(
        &store,
        "durable-agent",
        3,
        Some(SNAPSHOT_HASH_A),
        50,
        50,
    )
    .await
    .unwrap();
    assert_eq!(
        ConfigSyncStore::prune_agent_applies_before(&store, 150)
            .await
            .unwrap(),
        1
    );
    let rows = ConfigSyncStore::list_agent_applies(&store).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].agent_id, "active-agent");
    assert_eq!(rows[1].agent_id, "durable-agent");

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn route_snapshot_returns_routes_and_generation_from_committed_state() {
    let path = temp_db("route-snapshot");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    {
        let mut connection = rusqlite::Connection::open(&path).unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO tunnel_routes \
                 (host, app_id, connector_endpoint, require_healthy_tunnel) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params!["app.internal", "app-001", "connector:stream", 1],
            )
            .unwrap();
        transaction
            .execute(
                "UPDATE config_state SET generation = 1, updated_at_ms = 100 WHERE id = 1",
                [],
            )
            .unwrap();
        transaction.commit().unwrap();
    }

    let snapshot = ConfigSyncStore::load_route_snapshot(&store).await.unwrap();
    assert_eq!(snapshot.generation, 1);
    assert_eq!(snapshot.routes.len(), 1);
    assert_eq!(snapshot.routes[0].host, "app.internal");
    assert_eq!(snapshot.routes[0].app_id, "app-001");

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn duplicate_audit_rolls_back_route_generation_and_outbox_together() {
    let path = temp_db("atomic-rollback");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    let duplicate = mutation_audit("duplicate-audit", "app-001");
    AuditLogsStore::insert(&store, &duplicate).await.unwrap();
    let result = AuditLogsStore::apply_security_mutation(
        &store,
        &SecurityMutation::UpsertTunnelRoute(route("one.internal", "app-001")),
        &duplicate,
    )
    .await;

    assert!(result.is_err());
    let snapshot = ConfigSyncStore::load_route_snapshot(&store).await.unwrap();
    assert_eq!(snapshot.generation, 0);
    assert!(snapshot.routes.is_empty());
    assert!(ConfigSyncStore::list_jobs(&store).await.unwrap().is_empty());

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn deleting_the_last_route_creates_a_durable_delete_tombstone() {
    let path = temp_db("last-route-delete");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    apply_mutation(
        &store,
        "upstream-app-001",
        SecurityMutation::UpsertIntranetUpstream(upstream("app-001")),
    )
    .await;
    apply_mutation(
        &store,
        "route-app-001",
        SecurityMutation::UpsertTunnelRoute(route("one.internal", "app-001")),
    )
    .await;
    apply_mutation(
        &store,
        "delete-app-001",
        SecurityMutation::DeleteTunnelRoute("one.internal".into()),
    )
    .await;

    let snapshot = ConfigSyncStore::load_route_snapshot(&store).await.unwrap();
    assert_eq!(snapshot.generation, 3);
    assert!(snapshot.routes.is_empty());
    let latest = ConfigSyncStore::list_jobs(&store)
        .await
        .unwrap()
        .into_iter()
        .find(|job| job.generation == 3)
        .unwrap();
    assert_eq!(latest.resource_id, "app-001");
    assert_eq!(latest.operation, ConfigSyncOperation::Delete);
    assert_eq!(latest.status, ConfigSyncStatus::Pending);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn deleting_one_of_multiple_routes_keeps_the_app_upsert_desired() {
    let path = temp_db("multi-route-delete");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    apply_mutation(
        &store,
        "upstream-app-001",
        SecurityMutation::UpsertIntranetUpstream(upstream("app-001")),
    )
    .await;
    apply_mutation(
        &store,
        "route-one",
        SecurityMutation::UpsertTunnelRoute(route("one.internal", "app-001")),
    )
    .await;
    apply_mutation(
        &store,
        "route-two",
        SecurityMutation::UpsertTunnelRoute(route("two.internal", "app-001")),
    )
    .await;
    apply_mutation(
        &store,
        "delete-one",
        SecurityMutation::DeleteTunnelRoute("one.internal".into()),
    )
    .await;

    let snapshot = ConfigSyncStore::load_route_snapshot(&store).await.unwrap();
    assert_eq!(snapshot.generation, 4);
    assert_eq!(snapshot.routes.len(), 1);
    assert_eq!(snapshot.routes[0].host, "two.internal");
    let latest = ConfigSyncStore::list_jobs(&store)
        .await
        .unwrap()
        .into_iter()
        .find(|job| job.generation == 4)
        .unwrap();
    assert_eq!(latest.operation, ConfigSyncOperation::Upsert);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn moving_a_host_enqueues_new_app_upsert_and_old_app_delete_once() {
    let path = temp_db("move-host");
    let store = sqlite_store(&path);
    ensure_store_schema(&store).await.unwrap();

    apply_mutation(
        &store,
        "upstream-old",
        SecurityMutation::UpsertIntranetUpstream(upstream("app-old")),
    )
    .await;
    apply_mutation(
        &store,
        "upstream-new",
        SecurityMutation::UpsertIntranetUpstream(upstream("app-new")),
    )
    .await;
    apply_mutation(
        &store,
        "route-old",
        SecurityMutation::UpsertTunnelRoute(route("move.internal", "app-old")),
    )
    .await;
    apply_mutation(
        &store,
        "route-new",
        SecurityMutation::UpsertTunnelRoute(route("move.internal", "app-new")),
    )
    .await;

    let snapshot = ConfigSyncStore::load_route_snapshot(&store).await.unwrap();
    assert_eq!(snapshot.generation, 4);
    assert_eq!(snapshot.routes.len(), 1);
    assert_eq!(snapshot.routes[0].app_id, "app-new");
    let mut latest = ConfigSyncStore::list_jobs(&store)
        .await
        .unwrap()
        .into_iter()
        .filter(|job| job.generation == 4)
        .collect::<Vec<_>>();
    latest.sort_by(|left, right| left.app_id.cmp(&right.app_id));
    assert_eq!(latest.len(), 2);
    assert_eq!(latest[0].app_id, "app-new");
    assert_eq!(latest[0].operation, ConfigSyncOperation::Upsert);
    assert_eq!(latest[1].app_id, "app-old");
    assert_eq!(latest[1].operation, ConfigSyncOperation::Delete);

    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL at SAG_TEST_POSTGRES_DSN"]
async fn postgres_schema_and_config_convergence_contract() {
    let dsn = std::env::var("SAG_TEST_POSTGRES_DSN")
        .expect("SAG_TEST_POSTGRES_DSN must point to an isolated PostgreSQL test database");
    let store = StorageStore::Postgres(
        PostgresStore::with_config(
            dsn,
            PostgresPoolConfig {
                max_size: 4,
                acquire_timeout: std::time::Duration::from_secs(2),
                connect_timeout: std::time::Duration::from_secs(2),
                query_timeout: std::time::Duration::from_secs(5),
            },
        )
        .unwrap(),
    );
    ensure_store_schema(&store).await.unwrap();
    assert_eq!(
        ConfigSyncStore::current_generation(&store).await.unwrap(),
        0
    );

    let manual = ConfigSyncJobDraft {
        generation: 1,
        target: "APISIX".into(),
        resource_type: "ROUTE".into(),
        resource_id: "manual-job".into(),
        app_id: "manual-job".into(),
        operation: ConfigSyncOperation::Delete,
        payload_json: None,
        next_attempt_at_ms: 10,
    };
    let job = ConfigSyncStore::enqueue_job(&store, &manual, 10)
        .await
        .unwrap();
    let claimed = ConfigSyncStore::claim_due_jobs(&store, "pg-worker", 100, 50, 10)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].job_id, job.job_id);
    assert!(
        ConfigSyncStore::mark_applied(&store, &job.job_id, "pg-worker", 110)
            .await
            .unwrap()
    );

    ConfigSyncStore::ack_agent_generation(&store, "pg-agent", 2, Some(SNAPSHOT_HASH_A), 200, 210)
        .await
        .unwrap();
    ConfigSyncStore::ack_agent_generation(&store, "pg-agent", 1, Some(SNAPSHOT_HASH_B), 300, 310)
        .await
        .unwrap();
    let pg_apply = ConfigSyncStore::get_agent_apply(&store, "pg-agent")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pg_apply.applied_generation, 2);
    assert_eq!(pg_apply.snapshot_hash.as_deref(), Some(SNAPSHOT_HASH_A));
    let hash_conflict = ConfigSyncStore::ack_agent_generation(
        &store,
        "pg-agent",
        2,
        Some(SNAPSHOT_HASH_B),
        400,
        410,
    )
    .await
    .unwrap_err();
    assert!(matches!(hash_conflict, StorageError::Conflict(_)));

    apply_mutation(
        &store,
        "pg-upstream",
        SecurityMutation::UpsertIntranetUpstream(upstream("pg-app")),
    )
    .await;
    apply_mutation(
        &store,
        "pg-route",
        SecurityMutation::UpsertTunnelRoute(route("pg.internal", "pg-app")),
    )
    .await;
    apply_mutation(
        &store,
        "pg-delete",
        SecurityMutation::DeleteTunnelRoute("pg.internal".into()),
    )
    .await;
    assert_eq!(
        ConfigSyncStore::current_generation(&store).await.unwrap(),
        3
    );
    let tombstone = ConfigSyncStore::list_jobs(&store)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.generation == 3 && row.app_id == "pg-app")
        .unwrap();
    assert_eq!(tombstone.operation, ConfigSyncOperation::Delete);

    // Two replicas racing different resource rows for one app may acquire at
    // most one app-scoped external-I/O lease.
    let pg = match &store {
        StorageStore::Postgres(pg) => pg,
        StorageStore::Sqlite(_) => unreachable!(),
    };
    pg.client()
        .await
        .unwrap()
        .execute("DELETE FROM config_sync_jobs", &[])
        .await
        .unwrap();
    let route_job = ConfigSyncStore::enqueue_job(
        &store,
        &ConfigSyncJobDraft {
            generation: 10,
            target: "APISIX".into(),
            resource_type: "ROUTE".into(),
            resource_id: "pg-lease-app".into(),
            app_id: "pg-lease-app".into(),
            operation: ConfigSyncOperation::Delete,
            payload_json: None,
            next_attempt_at_ms: 0,
        },
        0,
    )
    .await
    .unwrap();
    let route_id_job = ConfigSyncStore::enqueue_job(
        &store,
        &ConfigSyncJobDraft {
            generation: 10,
            target: "APISIX".into(),
            resource_type: "ROUTE_ID".into(),
            resource_id: "sag-route-pg-legacy".into(),
            app_id: "pg-lease-app".into(),
            operation: ConfigSyncOperation::Delete,
            payload_json: None,
            next_attempt_at_ms: 0,
        },
        0,
    )
    .await
    .unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let store_a = store.clone();
    let barrier_a = barrier.clone();
    let worker_a = tokio::spawn(async move {
        barrier_a.wait().await;
        ConfigSyncStore::claim_due_jobs(&store_a, "pg-worker-a", 0, 5_000, 1)
            .await
            .unwrap()
    });
    let store_b = store.clone();
    let barrier_b = barrier.clone();
    let worker_b = tokio::spawn(async move {
        barrier_b.wait().await;
        ConfigSyncStore::claim_due_jobs(&store_b, "pg-worker-b", 0, 5_000, 1)
            .await
            .unwrap()
    });
    let (claimed_a, claimed_b) = tokio::join!(worker_a, worker_b);
    let claimed_a = claimed_a.unwrap();
    let claimed_b = claimed_b.unwrap();
    assert_eq!(claimed_a.len() + claimed_b.len(), 1);
    let (claimed, owner) = if claimed_a.is_empty() {
        (&claimed_b[0], "pg-worker-b")
    } else {
        (&claimed_a[0], "pg-worker-a")
    };
    assert!(claimed.job_id == route_job.job_id || claimed.job_id == route_id_job.job_id);
    assert!(
        ConfigSyncStore::mark_applied(&store, &claimed.job_id, owner, 1)
            .await
            .unwrap()
    );
    let remaining = ConfigSyncStore::claim_due_jobs(&store, "pg-worker-c", 0, 5_000, 1)
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_ne!(remaining[0].job_id, claimed.job_id);
    assert!(
        ConfigSyncStore::mark_applied(&store, &remaining[0].job_id, "pg-worker-c", 2)
            .await
            .unwrap()
    );

    // Incompatible same-app routes submitted by two control-plane replicas
    // serialize on generation/app locks: exactly one commits.
    let generation_before = ConfigSyncStore::current_generation(&store).await.unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mutation_a = SecurityMutation::UpsertTunnelRoute(TunnelRouteRecord {
        host: "pg-concurrent-a.internal".into(),
        app_id: "pg-concurrent-app".into(),
        connector_endpoint: "connector-a:stream".into(),
        require_healthy_tunnel: true,
    });
    let mutation_b = SecurityMutation::UpsertTunnelRoute(TunnelRouteRecord {
        host: "pg-concurrent-b.internal".into(),
        app_id: "pg-concurrent-app".into(),
        connector_endpoint: "connector-b:stream".into(),
        require_healthy_tunnel: true,
    });
    let store_a = store.clone();
    let barrier_a = barrier.clone();
    let task_a = tokio::spawn(async move {
        barrier_a.wait().await;
        AuditLogsStore::apply_security_mutation(
            &store_a,
            &mutation_a,
            &mutation_audit("pg-concurrent-a", "pg-concurrent-app"),
        )
        .await
    });
    let store_b = store.clone();
    let barrier_b = barrier.clone();
    let task_b = tokio::spawn(async move {
        barrier_b.wait().await;
        AuditLogsStore::apply_security_mutation(
            &store_b,
            &mutation_b,
            &mutation_audit("pg-concurrent-b", "pg-concurrent-app"),
        )
        .await
    });
    let (result_a, result_b) = tokio::join!(task_a, task_b);
    let results = [result_a.unwrap(), result_b.unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(StorageError::Conflict(_))))
            .count(),
        1
    );
    assert_eq!(
        ConfigSyncStore::current_generation(&store).await.unwrap(),
        generation_before + 1
    );
    assert_eq!(
        RoutesStore::load_all(&store)
            .await
            .unwrap()
            .into_iter()
            .filter(|route| route.app_id == "pg-concurrent-app")
            .count(),
        1
    );

    // A reconciliation requeue at generation G racing mutation G+1 cannot
    // leave two current rows or resurrect G after G+1 commits.
    apply_mutation(
        &store,
        "pg-race-upstream",
        SecurityMutation::UpsertIntranetUpstream(upstream("pg-race-app")),
    )
    .await;
    apply_mutation(
        &store,
        "pg-race-route",
        SecurityMutation::UpsertTunnelRoute(route("pg-race.internal", "pg-race-app")),
    )
    .await;
    let race_generation = ConfigSyncStore::current_generation(&store).await.unwrap();
    let repair = ConfigSyncJobDraft {
        generation: race_generation,
        target: "APISIX".into(),
        resource_type: "ROUTE".into(),
        resource_id: "pg-race-app".into(),
        app_id: "pg-race-app".into(),
        operation: ConfigSyncOperation::Upsert,
        payload_json: None,
        next_attempt_at_ms: 0,
    };
    let update = SecurityMutation::UpsertTunnelRoute(TunnelRouteRecord {
        host: "pg-race.internal".into(),
        app_id: "pg-race-app".into(),
        connector_endpoint: "connector-new:stream".into(),
        require_healthy_tunnel: true,
    });
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let store_reconcile = store.clone();
    let barrier_reconcile = barrier.clone();
    let reconcile = tokio::spawn(async move {
        barrier_reconcile.wait().await;
        ConfigSyncStore::requeue_job(&store_reconcile, &repair, 0).await
    });
    let store_mutation = store.clone();
    let barrier_mutation = barrier.clone();
    let mutation = tokio::spawn(async move {
        barrier_mutation.wait().await;
        AuditLogsStore::apply_security_mutation(
            &store_mutation,
            &update,
            &mutation_audit("pg-race-update", "pg-race-app"),
        )
        .await
    });
    let (reconcile_result, mutation_result) = tokio::join!(reconcile, mutation);
    mutation_result.unwrap().unwrap();
    if let Err(error) = reconcile_result.unwrap() {
        assert!(matches!(error, StorageError::Invariant(_)));
    }
    let final_generation = ConfigSyncStore::current_generation(&store).await.unwrap();
    assert_eq!(final_generation, race_generation + 1);
    let current_race_jobs = ConfigSyncStore::list_jobs(&store)
        .await
        .unwrap()
        .into_iter()
        .filter(|job| {
            job.target == "APISIX"
                && job.resource_type == "ROUTE"
                && job.resource_id == "pg-race-app"
                && job.superseded_by_generation.is_none()
        })
        .collect::<Vec<_>>();
    assert_eq!(current_race_jobs.len(), 1);
    assert_eq!(current_race_jobs[0].generation, final_generation);

    // The partial unique upgrade invariant is enforced by PostgreSQL itself.
    let duplicate = pg
        .client()
        .await
        .unwrap()
        .execute(
            "INSERT INTO config_sync_jobs ( \
               job_id, generation, target, resource_type, resource_id, app_id, operation, \
               status, attempt_count, next_attempt_at_ms, created_at_ms, updated_at_ms \
             ) VALUES ($1, $2, 'APISIX', 'ROUTE', 'pg-race-app', 'pg-race-app', 'UPSERT', \
               'PENDING', 0, 0, 0, 0)",
            &[&uuid::Uuid::new_v4().to_string(), &(final_generation + 1)],
        )
        .await
        .unwrap_err();
    assert_eq!(
        duplicate.code(),
        Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
    );
}

use std::time::{Duration, Instant};

use shared_storage::{
    ensure_store_schema, AppRecord, AuditLogRecord, AuditLogsStore, PostgresPoolConfig,
    PostgresStore, SecurityMutation, StorageError, StorageStore,
};
use tokio_postgres::error::SqlState;

fn test_dsn(application_name: &str) -> String {
    let base = std::env::var("SAG_TEST_POSTGRES_DSN")
        .expect("SAG_TEST_POSTGRES_DSN must point to an isolated PostgreSQL test database");
    let separator = if base.contains('?') { '&' } else { '?' };
    format!("{base}{separator}application_name={application_name}")
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL at SAG_TEST_POSTGRES_DSN"]
async fn pool_caps_connections_times_out_and_recovers() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let application_name = format!("sag_pool_test_{nonce}");
    let config = PostgresPoolConfig {
        max_size: 2,
        acquire_timeout: Duration::from_millis(150),
        connect_timeout: Duration::from_secs(2),
        query_timeout: Duration::from_millis(100),
    };
    let store = PostgresStore::with_config(test_dsn(&application_name), config).unwrap();

    let first = store.client().await.unwrap();
    let second = store.client().await.unwrap();
    let status = store.pool_status();
    assert_eq!(status.max_size, 2);
    assert_eq!(status.size, 2);
    assert_eq!(status.available, 0);

    let count: i64 = first
        .query_one(
            "SELECT COUNT(*) FROM pg_stat_activity WHERE application_name = $1",
            &[&application_name],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        count <= config.max_size as i64,
        "active connections={count}"
    );

    let wait_started = Instant::now();
    let error = store
        .client()
        .await
        .expect_err("pool must not exceed max_size");
    assert!(matches!(
        error,
        StorageError::PostgresPoolAcquireTimeout { .. }
    ));
    assert!(wait_started.elapsed() >= config.acquire_timeout);
    assert!(wait_started.elapsed() < Duration::from_secs(1));

    drop(second);
    let query_timeout = first
        .query_one("SELECT pg_sleep(0.5)", &[])
        .await
        .unwrap_err();
    assert_eq!(query_timeout.code(), Some(&SqlState::QUERY_CANCELED));

    let victim_pid: i32 = first
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let killer = store.client().await.unwrap();
    let terminated: bool = killer
        .query_one("SELECT pg_terminate_backend($1)", &[&victim_pid])
        .await
        .unwrap()
        .get(0);
    assert!(terminated);
    drop(first);
    drop(killer);

    let recovered = store.client().await.unwrap();
    let value: i32 = recovered.query_one("SELECT 1", &[]).await.unwrap().get(0);
    assert_eq!(value, 1);
    assert!(store.pool_status().size <= config.max_size);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL at SAG_TEST_POSTGRES_DSN"]
async fn postgres_security_mutation_rolls_back_with_duplicate_audit() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let app_id = format!("audit-rollback-{nonce}");
    let audit = AuditLogRecord::management(
        "postgres-test",
        "admin",
        app_id.clone(),
        format!("/api/v1/apps/{app_id}"),
        "PUT",
    );
    let store = StorageStore::Postgres(
        PostgresStore::with_config(
            test_dsn(&format!("sag_audit_tx_test_{nonce}")),
            PostgresPoolConfig {
                max_size: 2,
                acquire_timeout: Duration::from_secs(1),
                connect_timeout: Duration::from_secs(2),
                query_timeout: Duration::from_secs(2),
            },
        )
        .unwrap(),
    );
    ensure_store_schema(&store).await.unwrap();
    AuditLogsStore::insert(&store, &audit).await.unwrap();

    let result = AuditLogsStore::apply_security_mutation(
        &store,
        &SecurityMutation::UpsertApp(AppRecord {
            app_id: app_id.clone(),
            display_name: "must roll back".into(),
            description: String::new(),
            enabled: true,
        }),
        &audit,
    )
    .await;
    assert!(result.is_err());

    let StorageStore::Postgres(postgres) = &store else {
        unreachable!()
    };
    let client = postgres.client().await.unwrap();
    let app_count: i64 = client
        .query_one("SELECT COUNT(*) FROM apps WHERE app_id=$1", &[&app_id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(app_count, 0);
    client
        .execute("DELETE FROM audit_logs WHERE id=$1", &[&audit.id])
        .await
        .unwrap();
}

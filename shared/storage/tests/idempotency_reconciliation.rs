use shared_storage::{
    ensure_store_schema, IdempotencyClaim, IdempotencyState, IdempotencyStore, SqliteStore,
    StorageStore,
};

fn test_store(name: &str) -> (StorageStore, String) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir()
        .join(format!(
            "sag-idempotency-reconcile-{name}-{}-{nonce}.db",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned();
    (StorageStore::Sqlite(SqliteStore::new(path.clone())), path)
}

#[tokio::test]
async fn only_the_declared_state_machine_transitions_are_accepted() {
    let (store, path) = test_store("transitions");
    ensure_store_schema(&store).await.unwrap();

    let IdempotencyClaim::Claimed { state_version } =
        IdempotencyStore::claim(&store, "scope", "hash", "attempt", 10, 10_000)
            .await
            .unwrap()
    else {
        panic!("first attempt must own the claim");
    };
    assert_eq!(state_version, 1);

    let dispatched_version =
        IdempotencyStore::mark_dispatched(&store, "scope", "hash", "attempt", state_version, 20)
            .await
            .unwrap()
            .expect("claimed -> dispatched must succeed");

    assert!(
        !IdempotencyStore::release_undispatched(
            &store,
            "scope",
            "hash",
            "attempt",
            dispatched_version,
        )
        .await
        .unwrap(),
        "a dispatched mutation must never be automatically released"
    );

    let indeterminate_version = IdempotencyStore::mark_indeterminate(
        &store,
        "scope",
        "hash",
        "attempt",
        dispatched_version,
        30,
    )
    .await
    .unwrap()
    .expect("dispatched -> indeterminate must succeed");

    assert!(
        !IdempotencyStore::complete(
            &store,
            "scope",
            "hash",
            "attempt",
            indeterminate_version,
            200,
            "{}",
            b"late transport response",
            40,
        )
        .await
        .unwrap(),
        "a late transport completion must not race past reconciliation"
    );

    assert!(IdempotencyStore::complete_by_operator(
        &store,
        "scope",
        indeterminate_version,
        201,
        "{}",
        b"operator verified result",
        "admin-1",
        "verified the upstream transaction receipt",
        50,
        "audit-complete-1",
    )
    .await
    .unwrap());

    let record = IdempotencyStore::get(&store, "scope")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, IdempotencyState::CompletedByOperator);
    assert_eq!(record.reconciled_by.as_deref(), Some("admin-1"));
    assert!(record.result_hash.is_some());

    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn concurrent_operator_decisions_use_compare_and_set() {
    let (store, path) = test_store("operator-cas");
    ensure_store_schema(&store).await.unwrap();
    let IdempotencyClaim::Claimed { state_version } =
        IdempotencyStore::claim(&store, "scope", "hash", "attempt", 10, 10_000)
            .await
            .unwrap()
    else {
        panic!("first attempt must own the claim");
    };
    let dispatched_version =
        IdempotencyStore::mark_dispatched(&store, "scope", "hash", "attempt", state_version, 20)
            .await
            .unwrap()
            .unwrap();
    let indeterminate_version = IdempotencyStore::mark_indeterminate(
        &store,
        "scope",
        "hash",
        "attempt",
        dispatched_version,
        30,
    )
    .await
    .unwrap()
    .unwrap();

    let complete_store = store.clone();
    let release_store = store.clone();
    let (completed, released) = tokio::join!(
        IdempotencyStore::complete_by_operator(
            &complete_store,
            "scope",
            indeterminate_version,
            200,
            "{}",
            b"result",
            "admin-a",
            "receipt found",
            40,
            "audit-complete-cas",
        ),
        IdempotencyStore::release_by_operator(
            &release_store,
            "scope",
            indeterminate_version,
            "admin-b",
            "verified no upstream execution",
            40,
            "audit-release-cas",
        )
    );
    assert_eq!(completed.unwrap() as u8 + released.unwrap() as u8, 1);

    let final_state = IdempotencyStore::get(&store, "scope")
        .await
        .unwrap()
        .unwrap()
        .state;
    assert!(matches!(
        final_state,
        IdempotencyState::CompletedByOperator | IdempotencyState::ReleasedByOperator
    ));

    drop(store);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn legacy_pending_rows_migrate_conservatively_to_indeterminate() {
    let (store, path) = test_store("legacy-pending");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE idempotency_records (
                    scope_key TEXT PRIMARY KEY,
                    request_hash TEXT NOT NULL,
                    state TEXT NOT NULL CHECK (state IN ('pending', 'completed')),
                    owner_attempt_id TEXT NOT NULL,
                    status_code INTEGER NOT NULL DEFAULT 0,
                    headers_json TEXT NOT NULL DEFAULT '{}',
                    body BLOB NOT NULL DEFAULT X'',
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    expires_at_ms INTEGER NOT NULL
                );
                INSERT INTO idempotency_records
                    (scope_key, request_hash, state, owner_attempt_id,
                     created_at_ms, updated_at_ms, expires_at_ms)
                VALUES ('legacy', 'hash', 'pending', 'old-attempt', 10, 11, 10000);
                "#,
            )
            .unwrap();
    }

    ensure_store_schema(&store).await.unwrap();
    let record = IdempotencyStore::get(&store, "legacy")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, IdempotencyState::Indeterminate);
    assert!(!IdempotencyStore::release_undispatched(
        &store,
        "legacy",
        "hash",
        "old-attempt",
        record.state_version,
    )
    .await
    .unwrap());

    drop(store);
    let _ = std::fs::remove_file(path);
}

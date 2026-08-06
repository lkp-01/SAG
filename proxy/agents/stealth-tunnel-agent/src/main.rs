mod config;
mod connector_registry;
mod degrade_redis;
mod grpc_server;
mod manager;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use degrade_redis::AgentDegradeRedis;
use grpc_server::StealthTunnelGrpcService;
use moka::future::Cache;
use sag_service_health::Readiness;
use sag_tunnel_proto::tunnel_service_server::TunnelServiceServer;
use shared_storage::{build_store_from_env, ensure_store_schema, AuditWriter, ConfigSyncStore};
use tokio::sync::{RwLock, Semaphore};
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tracing::{info, warn};

use crate::config::StealthTunnelConfig;
use crate::connector_registry::{ConnectorRegistry, ProbeOutcome, ProbePolicy};
use crate::manager::{agent_instance_id, sync_routes_loop, TunnelManager};

#[derive(Clone)]
struct AgentHealthState {
    readiness: Readiness,
    manager: TunnelManager,
    connector_registry: ConnectorRegistry,
    minimum_connector_sessions: usize,
    tunnel_healthy_window: std::time::Duration,
    max_route_sync_age: std::time::Duration,
}

async fn live() -> StatusCode {
    StatusCode::OK
}

async fn ready(State(state): State<AgentHealthState>) -> StatusCode {
    let timeout = std::time::Duration::from_millis(
        std::env::var("SAG_READINESS_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1_000)
            .max(1),
    );
    let manager = state.manager.clone();
    let registry = state.connector_registry.clone();
    let minimum = state.minimum_connector_sessions;
    let healthy_window = state.tunnel_healthy_window;
    let max_route_sync_age = state.max_route_sync_age;
    let observed = state
        .readiness
        .probe(timeout, async move {
            manager.initial_sync_succeeded()
                && manager.route_sync_age_seconds() <= max_route_sync_age.as_secs_f64()
                && registry.healthy_session_count(healthy_window) >= minimum
        })
        .await;
    if observed == sag_service_health::ReadyState::Ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

fn debug_admin_enabled() -> bool {
    matches!(
        std::env::var("SAG_AGENT_DEBUG_ADMIN").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    )
}

async fn clear_ephemeral_caches_handler(
    axum::extract::State(svc): axum::extract::State<StealthTunnelGrpcService>,
) -> axum::http::StatusCode {
    svc.clear_ephemeral_caches().await;
    axum::http::StatusCode::NO_CONTENT
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let metrics_addr =
        std::env::var("SAG_METRICS_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:9104".to_string());
    // IMPORTANT: `install_recorder()` only installs the recorder; it does NOT start an HTTP listener.
    // We need `build()` + spawn the exporter future to actually serve `/metrics`.
    let metrics_addr = metrics_addr.parse::<std::net::SocketAddr>()?;
    let (recorder, exporter) = metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_http_listener(metrics_addr)
        .build()
        .map_err(|e| anyhow::anyhow!("build prometheus exporter failed: {e}"))?;
    metrics::set_global_recorder(recorder)
        .map_err(|e| anyhow::anyhow!("set global recorder failed: {e}"))?;
    tokio::spawn(exporter);
    // Ensure /metrics is never an empty scrape when no traffic has hit instrumentation yet.
    metrics::describe_counter!(
        "sag_stealth_tunnel_agent_info",
        metrics::Unit::Count,
        "Process is up and Prometheus recorder is wired (always 1)."
    );
    metrics::counter!(
        "sag_stealth_tunnel_agent_info",
        "version" => env!("CARGO_PKG_VERSION")
    )
    .increment(1);
    metrics::gauge!("agent_pending_waiters").set(0.0);
    metrics::gauge!("agent_connector_sessions").set(0.0);
    info!(%metrics_addr, "metrics listening (/metrics)");

    let cfg0 = StealthTunnelConfig::from_env()?;
    info!(
        max_pending_waiters = cfg0.max_pending_waiters,
        stream_buffer = cfg0.stream_buffer,
        max_request_body_bytes = cfg0.max_request_body_bytes,
        max_response_body_bytes = cfg0.max_response_body_bytes,
        memory_required_bytes = cfg0.memory_required_bytes,
        memory_allowed_bytes = cfg0.memory_allowed_bytes,
        connector_probe_enabled = cfg0.connector_probe_enabled,
        connector_probe_interval_ms = cfg0.connector_probe_interval_ms,
        connector_probe_timeout_ms = cfg0.connector_probe_timeout_ms,
        connector_probe_freshness_ms = cfg0.connector_probe_freshness_ms,
        connector_probe_startup_grace_ms = cfg0.connector_probe_startup_grace_ms,
        connector_probe_failure_threshold = cfg0.connector_probe_failure_threshold,
        "stealth-tunnel-agent bounded data-plane memory budget enabled"
    );
    let listen: SocketAddr = cfg0.listen_addr.parse()?;
    let sync_eps = cfg0.control_plane_sync_endpoints.clone();
    let sync_interval = std::time::Duration::from_millis(cfg0.sync_interval_ms);
    let sync_token = std::env::var("SAG_AGENT_SYNC_TOKEN")
        .ok()
        .filter(|v| !v.is_empty());

    info!(
        ?sync_eps,
        "control-plane route sync endpoints (fetched concurrently; highest generation wins)"
    );

    let probe_policy = ProbePolicy {
        enabled: cfg0.connector_probe_enabled,
        freshness: std::time::Duration::from_millis(cfg0.connector_probe_freshness_ms),
        startup_grace: std::time::Duration::from_millis(cfg0.connector_probe_startup_grace_ms),
        failure_threshold: cfg0.connector_probe_failure_threshold,
    };
    let probe_interval = std::time::Duration::from_millis(cfg0.connector_probe_interval_ms);
    let probe_timeout = std::time::Duration::from_millis(cfg0.connector_probe_timeout_ms);
    let cfg = Arc::new(RwLock::new(cfg0));
    let connector_registry = ConnectorRegistry::with_probe_policy(probe_policy);
    let reaper_registry = connector_registry.clone();
    let reaper_config = cfg.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            let max_age = std::time::Duration::from_secs(
                reaper_config.read().await.tunnel_healthy_window_sec.max(1),
            );
            for expired in reaper_registry.expire_stale(max_age) {
                warn!(
                    connector_id = %expired.connector_id,
                    endpoint = %expired.endpoint,
                    generation = expired.generation,
                    max_age_sec = max_age.as_secs(),
                    "Connector heartbeat lease expired"
                );
            }
        }
    });
    if probe_policy.enabled {
        let probe_registry = connector_registry.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(probe_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let probes = probe_registry.probe_targets().into_iter().map(|target| {
                    let registry = probe_registry.clone();
                    async move {
                        let outcome = registry.probe_session(target.clone(), probe_timeout).await;
                        (target, outcome)
                    }
                });
                for (target, outcome) in futures::future::join_all(probes).await {
                    if outcome == ProbeOutcome::Revoked {
                        warn!(
                            endpoint = %target.endpoint,
                            generation = target.generation,
                            timeout_ms = probe_timeout.as_millis(),
                            "Connector real-path health probe failed; session revoked"
                        );
                    }
                }
            }
        });
    }

    let manager = TunnelManager::new();
    let readiness = Readiness::new(
        std::env::var("SAG_READINESS_SUCCESS_THRESHOLD")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(2)
            .max(1),
    );
    let policy_limit = cfg.read().await.policy_inflight_limit.max(1);
    let auth_limit = cfg.read().await.auth_inflight_limit.max(1);
    let degrade = AgentDegradeRedis::connect_from_env().await;

    let store = build_store_from_env();
    ensure_store_schema(&store).await?;
    let agent_id = agent_instance_id();
    if let Some(previous_apply) = ConfigSyncStore::get_agent_apply(&store, &agent_id).await? {
        if let Some(snapshot_hash) = previous_apply.snapshot_hash {
            let generation = u64::try_from(previous_apply.applied_generation).map_err(|_| {
                anyhow::anyhow!("persisted applied generation for Agent {agent_id} is negative")
            })?;
            manager
                .restore_generation_floor(generation, snapshot_hash, previous_apply.applied_at_ms)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "restore persisted route generation fence for Agent {agent_id}: {error}"
                    )
                })?;
            info!(
                %agent_id,
                applied_generation = generation,
                "restored durable route generation fence; waiting for matching or newer snapshot"
            );
        } else {
            warn!(
                %agent_id,
                applied_generation = previous_apply.applied_generation,
                "legacy Agent ACK has no snapshot fingerprint; restart rollback fence starts after the next successful sync"
            );
        }
    }
    let audit_writer = AuditWriter::from_env(store.clone())?;
    let svc = StealthTunnelGrpcService {
        manager,
        connector_registry,
        config: cfg.clone(),
        http_client: reqwest::Client::new(),
        policy_semaphore: Arc::new(Semaphore::new(policy_limit)),
        auth_semaphore: Arc::new(Semaphore::new(auth_limit)),
        pending_semaphore: Arc::new(Semaphore::new(cfg.read().await.max_pending_waiters.max(1))),
        store,
        audit_writer: audit_writer.clone(),
        policy_eval_cache: Arc::new(
            Cache::builder()
                .time_to_live(std::time::Duration::from_secs(
                    std::env::var("SAG_POLICY_DECISION_CACHE_TTL_SEC")
                        .ok()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(10),
                ))
                .build(),
        ),
        negative_cache: Arc::new(
            Cache::builder()
                .time_to_live(std::time::Duration::from_secs(
                    cfg.read().await.negative_cache_ttl_sec.max(1),
                ))
                .build(),
        ),
        negative_cache_enabled: cfg.read().await.negative_cache_enabled,
        readiness: readiness.clone(),
        degrade,
    };

    tokio::spawn(sync_routes_loop(
        sync_eps,
        svc.manager.clone(),
        sync_interval,
        sync_token,
    ));

    let health_addr: SocketAddr = std::env::var("SAG_HEALTH_LISTEN_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:9105".to_string())
        .parse()?;
    let health_state = AgentHealthState {
        readiness: readiness.clone(),
        manager: svc.manager.clone(),
        connector_registry: svc.connector_registry.clone(),
        minimum_connector_sessions: std::env::var("SAG_AGENT_MIN_CONNECTOR_SESSIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1),
        tunnel_healthy_window: std::time::Duration::from_secs(
            cfg.read().await.tunnel_healthy_window_sec.max(1),
        ),
        max_route_sync_age: std::time::Duration::from_secs(
            std::env::var("SAG_AGENT_MAX_ROUTE_SYNC_AGE_SEC")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(60)
                .clamp(5, 86_400),
        ),
    };
    let health_listener = tokio::net::TcpListener::bind(health_addr).await?;
    let health_app = Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/health", get(ready))
        .with_state(health_state);
    let mut health_task =
        tokio::spawn(async move { axum::serve(health_listener, health_app).await });
    info!(%health_addr, "agent health listening (/live, /ready)");

    if debug_admin_enabled() {
        let admin_listen = std::env::var("SAG_AGENT_DEBUG_LISTEN")
            .unwrap_or_else(|_| "127.0.0.1:19104".to_string());
        let admin_addr: SocketAddr = admin_listen
            .parse()
            .unwrap_or_else(|_| "127.0.0.1:19104".parse().expect("fallback admin addr"));
        let svc_admin = svc.clone();
        tokio::spawn(async move {
            let app = Router::new()
                .route(
                    "/debug/clear-ephemeral-caches",
                    post(clear_ephemeral_caches_handler),
                )
                .with_state(svc_admin);
            match tokio::net::TcpListener::bind(admin_addr).await {
                Ok(listener) => {
                    info!(
                        %admin_addr,
                        "debug admin: POST /debug/clear-ephemeral-caches (policy/negative only; not tunnels or idempotency ledger)"
                    );
                    if let Err(e) = axum::serve(listener, app).await {
                        tracing::error!(?e, "debug admin server exited");
                    }
                }
                Err(e) => tracing::error!(?e, %admin_addr, "debug admin bind failed"),
            }
        });
    }

    let (grpc_tls_enabled, cert_path, key_path, ca_path) = {
        let c = cfg.read().await;
        if c.grpc_tls_enabled {
            (
                true,
                c.grpc_tls_cert
                    .clone()
                    .filter(|path| !path.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("SAG_GRPC_TLS_CERT is required when mTLS is enabled")
                    })?,
                c.grpc_tls_key
                    .clone()
                    .filter(|path| !path.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("SAG_GRPC_TLS_KEY is required when mTLS is enabled")
                    })?,
                c.grpc_tls_client_ca
                    .clone()
                    .filter(|path| !path.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("SAG_GRPC_TLS_CLIENT_CA is required when mTLS is enabled")
                    })?,
            )
        } else {
            (false, String::new(), String::new(), String::new())
        }
    };
    let grpc_keepalive_interval = std::time::Duration::from_millis(
        std::env::var("SAG_GRPC_KEEPALIVE_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(5_000)
            .max(1_000),
    );
    let grpc_keepalive_timeout = std::time::Duration::from_millis(
        std::env::var("SAG_GRPC_KEEPALIVE_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(5_000)
            .max(1_000),
    );

    let tls_material = if grpc_tls_enabled {
        let cert = tokio::fs::read(&cert_path).await.map_err(|error| {
            anyhow::anyhow!("read Agent TLS certificate {cert_path:?} failed: {error}")
        })?;
        let key = tokio::fs::read(&key_path).await.map_err(|error| {
            anyhow::anyhow!("read Agent TLS private key {key_path:?} failed: {error}")
        })?;
        let ca = tokio::fs::read(&ca_path).await.map_err(|error| {
            anyhow::anyhow!(
                "read Agent mTLS client CA {ca_path:?} failed; refusing server-only TLS: {error}"
            )
        })?;
        Some(
            ServerTlsConfig::new()
                .identity(Identity::from_pem(cert, key))
                .client_ca_root(tonic::transport::Certificate::from_pem(ca)),
        )
    } else {
        None
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let mut grpc_task = tokio::spawn(async move {
        let mut builder = Server::builder()
            .http2_keepalive_interval(Some(grpc_keepalive_interval))
            .http2_keepalive_timeout(Some(grpc_keepalive_timeout));
        if let Some(tls) = tls_material {
            info!(%listen, "stealth-tunnel-agent listening (mTLS)");
            builder
                .tls_config(tls)?
                .add_service(TunnelServiceServer::new(svc))
                .serve_with_shutdown(listen, async move {
                    let _ = shutdown_rx.await;
                })
                .await?;
        } else {
            info!(%listen, "stealth-tunnel-agent listening (plaintext gRPC)");
            builder
                .add_service(TunnelServiceServer::new(svc))
                .serve_with_shutdown(listen, async move {
                    let _ = shutdown_rx.await;
                })
                .await?;
        }
        Ok::<(), anyhow::Error>(())
    });

    sag_service_health::shutdown_signal().await;
    readiness.begin_draining();
    let _ = shutdown_tx.send(());
    let drain_timeout = std::time::Duration::from_millis(
        std::env::var("SAG_DRAIN_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30_000)
            .max(1),
    );
    let drain_report = readiness.wait_for_drain(drain_timeout).await;
    if drain_report.timed_out {
        metrics::counter!("shutdown_drain_timeout_total").increment(1);
        warn!(
            remaining = drain_report.remaining,
            "agent request drain deadline expired"
        );
    }
    match tokio::time::timeout(drain_timeout, &mut grpc_task).await {
        Ok(result) => result??,
        Err(_) => {
            grpc_task.abort();
            metrics::counter!("shutdown_server_abort_total").increment(1);
            warn!(
                remaining = readiness.active(),
                "agent gRPC drain forced to abort"
            );
        }
    }
    health_task.abort();
    let _ = (&mut health_task).await;

    let audit_report = audit_writer.shutdown().await;
    if audit_report.dropped > 0 {
        warn!(
            dropped = audit_report.dropped,
            timed_out = audit_report.timed_out,
            "audit writer did not drain completely during shutdown"
        );
    }
    Ok(())
}

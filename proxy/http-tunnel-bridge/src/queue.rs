//! Redis Stream queue for dataplane overload shedding (bounded queue + 202 poll).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use redis::aio::{
    ConnectionLike, ConnectionManager, ConnectionManagerConfig, MultiplexedConnection,
};
use redis::sentinel::{SentinelClient, SentinelNodeConnectionInfo, SentinelServerType};
use redis::streams::{
    StreamAutoClaimOptions, StreamAutoClaimReply, StreamPendingCountReply, StreamPendingReply,
    StreamReadOptions, StreamReadReply,
};
use redis::{AsyncCommands, Cmd, ConnectionAddr, FromRedisValue, RedisFuture, TlsMode, Value};
use sag_tunnel_proto::{ForwardRequest, ForwardResponse};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

pub const GROUP_NAME: &str = "bridge-workers";

const ENQUEUE_SCRIPT_V1: &str = r#"
-- sag queue enqueue v1
local stream_type = redis.call('TYPE', KEYS[1])['ok']
local job_type = redis.call('TYPE', KEYS[2])['ok']
if stream_type ~= 'none' and stream_type ~= 'stream' then
  return redis.error_reply('queue stream key has wrong type')
end
if job_type ~= 'none' and job_type ~= 'hash' then
  return redis.error_reply('queue job key has wrong type')
end
if job_type == 'hash' then
  local existing_id = redis.call('HGET', KEYS[2], 'stream_id') or ''
  return {2, existing_id, redis.call('XLEN', KEYS[1])}
end
local length = redis.call('XLEN', KEYS[1])
if length >= tonumber(ARGV[1]) then
  return {0, '', length}
end
local stream_id = redis.call(
  'XADD', KEYS[1], '*',
  'payload', ARGV[2],
  'attempt', '0',
  'claimed_at_ms', '0'
)
redis.call(
  'HSET', KEYS[2],
  'status', 'pending',
  'stream_id', stream_id,
  'enqueued_at_ms', ARGV[3],
  'attempt', '0',
  'claimed_at_ms', '0'
)
redis.call('EXPIRE', KEYS[2], tonumber(ARGV[4]))
return {1, stream_id, length + 1}
"#;

const PREPARE_DELIVERY_SCRIPT_V1: &str = r#"
-- sag queue prepare delivery v1
local stream_type = redis.call('TYPE', KEYS[1])['ok']
local job_type = redis.call('TYPE', KEYS[2])['ok']
local dlq_type = redis.call('TYPE', KEYS[3])['ok']
if stream_type ~= 'stream' then return redis.error_reply('queue stream missing or wrong type') end
if job_type ~= 'hash' then return redis.error_reply('queue job missing or wrong type') end
if dlq_type ~= 'none' and dlq_type ~= 'stream' then return redis.error_reply('queue dlq key has wrong type') end
local status = redis.call('HGET', KEYS[2], 'status') or ''
if status == 'done' or status == 'failed' or status == 'dlq' then
  return {2, tonumber(redis.call('HGET', KEYS[2], 'attempt') or '0')}
end
local attempt = redis.call('HINCRBY', KEYS[2], 'attempt', 1)
redis.call('HSET', KEYS[2], 'claimed_at_ms', ARGV[3], 'status', 'running')
redis.call('EXPIRE', KEYS[2], tonumber(ARGV[5]))
if attempt > tonumber(ARGV[4]) then
  redis.call('XADD', KEYS[3], '*', 'stream_id', ARGV[2], 'error', 'max attempts exceeded', 'payload', ARGV[6])
  redis.call('HSET', KEYS[2], 'status', 'dlq', 'error', 'max attempts exceeded')
  redis.call('XACK', KEYS[1], ARGV[1], ARGV[2])
  redis.call('XDEL', KEYS[1], ARGV[2])
  return {3, attempt}
end
return {1, attempt}
"#;

const COMPLETE_SUCCESS_SCRIPT_V1: &str = r#"
-- sag queue complete success v1
local stream_type = redis.call('TYPE', KEYS[1])['ok']
local job_type = redis.call('TYPE', KEYS[2])['ok']
if stream_type ~= 'stream' then return redis.error_reply('queue stream missing or wrong type') end
if job_type ~= 'hash' then return redis.error_reply('queue job missing or wrong type') end
redis.call(
  'HSET', KEYS[2],
  'status', 'done',
  'http_status', ARGV[3],
  'headers_json', ARGV[4],
  'body_b64', ARGV[5],
  'body_truncated', ARGV[6],
  'error', ''
)
redis.call('EXPIRE', KEYS[2], tonumber(ARGV[7]))
redis.call('XACK', KEYS[1], ARGV[1], ARGV[2])
redis.call('XDEL', KEYS[1], ARGV[2])
return 1
"#;

const COMPLETE_FAILURE_SCRIPT_V1: &str = r#"
-- sag queue complete failure v1
local stream_type = redis.call('TYPE', KEYS[1])['ok']
local job_type = redis.call('TYPE', KEYS[2])['ok']
local dlq_type = redis.call('TYPE', KEYS[3])['ok']
if stream_type ~= 'stream' then return redis.error_reply('queue stream missing or wrong type') end
if job_type ~= 'hash' then return redis.error_reply('queue job missing or wrong type') end
if dlq_type ~= 'none' and dlq_type ~= 'stream' then return redis.error_reply('queue dlq key has wrong type') end
redis.call('XADD', KEYS[3], '*', 'stream_id', ARGV[2], 'error', ARGV[3], 'payload', ARGV[4])
redis.call(
  'HSET', KEYS[2],
  'status', 'failed',
  'http_status', '0',
  'headers_json', '{}',
  'body_b64', '',
  'error', ARGV[3]
)
redis.call('EXPIRE', KEYS[2], tonumber(ARGV[5]))
redis.call('XACK', KEYS[1], ARGV[1], ARGV[2])
redis.call('XDEL', KEYS[1], ARGV[2])
return 1
"#;

const DLQ_UNPARSEABLE_SCRIPT_V1: &str = r#"
-- sag queue unparseable payload v1
local stream_type = redis.call('TYPE', KEYS[1])['ok']
local dlq_type = redis.call('TYPE', KEYS[2])['ok']
if stream_type ~= 'stream' then return redis.error_reply('queue stream missing or wrong type') end
if dlq_type ~= 'none' and dlq_type ~= 'stream' then return redis.error_reply('queue dlq key has wrong type') end
redis.call('XADD', KEYS[2], '*', 'stream_id', ARGV[2], 'error', ARGV[3], 'payload', ARGV[4])
redis.call('XACK', KEYS[1], ARGV[1], ARGV[2])
redis.call('XDEL', KEYS[1], ARGV[2])
return 1
"#;

const ACK_TERMINAL_SCRIPT_V1: &str = r#"
-- sag queue ack terminal v1
local job_type = redis.call('TYPE', KEYS[2])['ok']
if job_type ~= 'hash' then return redis.error_reply('queue job missing or wrong type') end
local status = redis.call('HGET', KEYS[2], 'status') or ''
if status ~= 'done' and status ~= 'failed' and status ~= 'dlq' then
  return redis.error_reply('refusing to ack non-terminal queue job')
end
redis.call('XACK', KEYS[1], ARGV[1], ARGV[2])
redis.call('XDEL', KEYS[1], ARGV[2])
return 1
"#;

const DEDUP_SCRIPT_V1: &str = r#"
-- sag queue dedup v1
local key_type = redis.call('TYPE', KEYS[1])['ok']
if key_type ~= 'none' and key_type ~= 'string' then
  return redis.error_reply('queue dedup key has wrong type')
end
local existing = redis.call('GET', KEYS[1])
if not existing then
  redis.call('SET', KEYS[1], ARGV[1], 'EX', tonumber(ARGV[2]))
  return 1
end
if existing == ARGV[1] then return 1 end
return 0
"#;

#[derive(Clone, Debug)]
pub struct QueueConfig {
    pub redis_url: String,
    pub sentinel_urls: Vec<String>,
    pub sentinel_service: Option<String>,
    pub redis_connect_timeout_ms: u64,
    pub redis_command_timeout_ms: u64,
    pub redis_reconnect_retries: usize,
    pub redis_reconnect_base_ms: u64,
    pub redis_reconnect_max_ms: u64,
    pub key_prefix: String,
    pub soft_inflight: usize,
    pub hard_inflight: usize,
    pub max_queue_len: usize,
    pub max_body_bytes: usize,
    pub queue_ttl_sec: u64,
    pub worker_concurrency: usize,
    pub max_result_body_bytes: usize,
    pub poll_min_interval_ms: u64,
    pub dedup_ttl_sec: u64,
    pub reclaim_idle_ms: u64,
    pub max_forward_deadline_ms: u64,
    pub reclaim_jitter_margin_ms: u64,
    pub max_attempts: u32,
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

impl QueueConfig {
    pub fn from_env() -> Option<Self> {
        let redis_url = std::env::var("SAG_BRIDGE_REDIS_URL")
            .ok()?
            .trim()
            .to_string();
        if redis_url.is_empty() {
            return None;
        }
        Some(Self {
            redis_url,
            sentinel_urls: std::env::var("SAG_BRIDGE_REDIS_SENTINELS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect(),
            sentinel_service: std::env::var("SAG_BRIDGE_REDIS_SENTINEL_SERVICE")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            redis_connect_timeout_ms: env_u64("SAG_BRIDGE_REDIS_CONNECT_TIMEOUT_MS", 2_000),
            redis_command_timeout_ms: env_u64("SAG_BRIDGE_REDIS_COMMAND_TIMEOUT_MS", 5_000),
            redis_reconnect_retries: env_usize("SAG_BRIDGE_REDIS_RECONNECT_RETRIES", 6),
            redis_reconnect_base_ms: env_u64("SAG_BRIDGE_REDIS_RECONNECT_BASE_MS", 100),
            redis_reconnect_max_ms: env_u64("SAG_BRIDGE_REDIS_RECONNECT_MAX_MS", 2_000),
            key_prefix: std::env::var("SAG_BRIDGE_QUEUE_KEY_PREFIX")
                .unwrap_or_else(|_| "sag:dataplane".into()),
            soft_inflight: env_usize("SAG_BRIDGE_SOFT_INFLIGHT", 24),
            hard_inflight: env_usize("SAG_BRIDGE_HARD_INFLIGHT", 128),
            max_queue_len: env_usize("SAG_BRIDGE_QUEUE_MAX_LEN", 5000),
            max_body_bytes: env_usize("SAG_BRIDGE_QUEUE_MAX_BODY_BYTES", 262_144),
            queue_ttl_sec: env_u64("SAG_BRIDGE_QUEUE_TTL_SEC", 600),
            worker_concurrency: env_usize("SAG_BRIDGE_WORKER_CONCURRENCY", 16).max(1),
            max_result_body_bytes: env_usize("SAG_BRIDGE_QUEUE_MAX_RESULT_BODY_BYTES", 65_536),
            poll_min_interval_ms: env_u64("SAG_BRIDGE_POLL_MIN_INTERVAL_MS", 100),
            dedup_ttl_sec: env_u64("SAG_BRIDGE_DEDUP_TTL_SEC", 600),
            reclaim_idle_ms: env_u64("SAG_BRIDGE_QUEUE_RECLAIM_IDLE_MS", 70_000),
            max_forward_deadline_ms: env_u64("SAG_BRIDGE_FORWARD_TIMEOUT_MS", 60_000),
            reclaim_jitter_margin_ms: env_u64("SAG_BRIDGE_QUEUE_RECLAIM_JITTER_MARGIN_MS", 5_000),
            max_attempts: env_usize("SAG_BRIDGE_QUEUE_MAX_ATTEMPTS", 3)
                .try_into()
                .unwrap_or(u32::MAX),
        })
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.key_prefix.trim().is_empty()
            || self.soft_inflight == 0
            || self.hard_inflight == 0
            || self.max_queue_len == 0
            || self.queue_ttl_sec == 0
            || self.dedup_ttl_sec == 0
            || self.max_attempts == 0
        {
            anyhow::bail!("queue key prefix and all queue bounds must be non-zero");
        }
        if self.soft_inflight > self.hard_inflight {
            anyhow::bail!(
                "SAG_BRIDGE_SOFT_INFLIGHT ({}) must not exceed SAG_BRIDGE_HARD_INFLIGHT ({})",
                self.soft_inflight,
                self.hard_inflight
            );
        }
        let minimum_reclaim = self
            .max_forward_deadline_ms
            .checked_add(self.reclaim_jitter_margin_ms)
            .ok_or_else(|| anyhow::anyhow!("queue reclaim deadline configuration overflow"))?;
        if self.reclaim_idle_ms <= minimum_reclaim {
            anyhow::bail!(
                "SAG_BRIDGE_QUEUE_RECLAIM_IDLE_MS ({}) must be greater than forward deadline + jitter margin ({minimum_reclaim})",
                self.reclaim_idle_ms
            );
        }
        redis::Client::open(self.redis_url.as_str())?;
        if self.sentinel_urls.is_empty() != self.sentinel_service.is_none() {
            anyhow::bail!(
                "SAG_BRIDGE_REDIS_SENTINELS and SAG_BRIDGE_REDIS_SENTINEL_SERVICE must be configured together"
            );
        }
        for sentinel_url in &self.sentinel_urls {
            redis::Client::open(sentinel_url.as_str())?;
        }
        if self.redis_connect_timeout_ms == 0
            || self.redis_command_timeout_ms <= 2_000
            || self.redis_reconnect_retries == 0
            || self.redis_reconnect_base_ms == 0
            || self.redis_reconnect_max_ms < self.redis_reconnect_base_ms
        {
            anyhow::bail!(
                "Redis connect/reconnect bounds must be non-zero, command timeout must exceed the 2000ms blocking read, and reconnect max must be >= base"
            );
        }
        Ok(())
    }

    pub fn deployment_mode(&self) -> RedisDeploymentMode {
        if !self.sentinel_urls.is_empty() {
            RedisDeploymentMode::Sentinel
        } else if self.redis_url.starts_with("rediss://") {
            RedisDeploymentMode::DirectTls
        } else {
            RedisDeploymentMode::Direct
        }
    }

    /// A credential-free endpoint description suitable for startup logs.
    pub fn safe_endpoint(&self) -> String {
        if !self.sentinel_urls.is_empty() {
            return format!(
                "sentinel://{} nodes/{}",
                self.sentinel_urls.len(),
                self.sentinel_service
                    .as_deref()
                    .unwrap_or("<missing-service>")
            );
        }
        let Ok(client) = redis::Client::open(self.redis_url.as_str()) else {
            return "<invalid-redis-endpoint>".into();
        };
        let info = client.get_connection_info();
        let scheme = match info.addr {
            ConnectionAddr::TcpTls { .. } => "rediss",
            _ => "redis",
        };
        format!("{scheme}://{}/{}", info.addr, info.redis.db)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedisDeploymentMode {
    Direct,
    DirectTls,
    Sentinel,
}

#[derive(Debug)]
pub enum EnqueueError {
    OverCapacity,
    BodyTooLarge,
    Serialization,
    Redis(redis::RedisError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimDecision {
    Process { attempt: u32 },
    Terminal,
    DeadLettered,
}

#[derive(Serialize, Deserialize)]
struct QueuedPayload {
    request_id: String,
    #[serde(default)]
    attempt_id: String,
    #[serde(default)]
    deadline_unix_ms: i64,
    #[serde(default)]
    idempotency_key: String,
    app_id: String,
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body_b64: String,
}

fn whitelist_headers(src: &HashMap<String, String>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (k, v) in src {
        let lk = k.to_ascii_lowercase();
        let keep = lk.starts_with("x-sag-")
            || matches!(
                lk.as_str(),
                "content-type"
                    | "accept"
                    | "authorization"
                    | "x-request-id"
                    | "x-trace-id"
                    | "idempotency-key"
                    | "x-idempotency-key"
                    | "user-agent"
            );
        if keep {
            out.insert(lk, v.clone());
        }
    }
    out
}

impl QueuedPayload {
    fn from_forward(fr: &ForwardRequest) -> Self {
        Self {
            request_id: fr.request_id.clone(),
            attempt_id: fr.attempt_id.clone(),
            deadline_unix_ms: fr.deadline_unix_ms,
            idempotency_key: fr.idempotency_key.clone(),
            app_id: fr.app_id.clone(),
            method: fr.method.clone(),
            path: fr.path.clone(),
            headers: whitelist_headers(&fr.headers),
            body_b64: B64.encode(&fr.body),
        }
    }

    fn into_forward(self) -> anyhow::Result<ForwardRequest> {
        let body = B64
            .decode(self.body_b64.as_bytes())
            .map_err(|e| anyhow::anyhow!("body b64: {e}"))?;
        Ok(ForwardRequest {
            request_id: self.request_id,
            attempt_id: self.attempt_id,
            deadline_unix_ms: self.deadline_unix_ms,
            idempotency_key: self.idempotency_key,
            app_id: self.app_id,
            method: self.method,
            path: self.path,
            headers: self.headers,
            body,
            stream_epoch: String::new(),
        })
    }
}

#[derive(Clone)]
enum QueueRedis {
    Direct(Box<ConnectionManager>),
    Sentinel(Arc<SentinelRedis>),
}

struct SentinelRedis {
    client: tokio::sync::Mutex<SentinelClient>,
    cached: tokio::sync::Mutex<Option<(u64, MultiplexedConnection)>>,
    next_generation: AtomicU64,
    connect_timeout: Duration,
    command_timeout: Duration,
    reconnect_retries: usize,
    reconnect_base_ms: u64,
    reconnect_max_ms: u64,
}

enum QueueConnection {
    Direct(Box<ConnectionManager>),
    Sentinel {
        connection: MultiplexedConnection,
        backend: Arc<SentinelRedis>,
        generation: u64,
    },
}

impl SentinelRedis {
    async fn connection(self: &Arc<Self>) -> redis::RedisResult<QueueConnection> {
        let mut cached = self.cached.lock().await;
        if let Some((generation, connection)) = cached.as_ref() {
            return Ok(QueueConnection::Sentinel {
                connection: connection.clone(),
                backend: self.clone(),
                generation: *generation,
            });
        }

        let async_config = redis::AsyncConnectionConfig::new()
            .set_connection_timeout(self.connect_timeout)
            .set_response_timeout(self.command_timeout);
        let mut delay_ms = self.reconnect_base_ms;
        let mut attempt = 0usize;
        let connection = loop {
            let result = self
                .client
                .lock()
                .await
                .get_async_connection_with_config(&async_config)
                .await;
            match result {
                Ok(connection) => break connection,
                Err(error) if attempt < self.reconnect_retries => {
                    attempt += 1;
                    metrics::counter!("bridge_queue_redis_reconnect_total", "mode" => "sentinel")
                        .increment(1);
                    warn!(
                        attempt,
                        delay_ms,
                        error_kind = ?error.kind(),
                        "Redis Sentinel master connection failed; retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms = delay_ms.saturating_mul(2).min(self.reconnect_max_ms);
                }
                Err(error) => return Err(error),
            }
        };
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        *cached = Some((generation, connection.clone()));
        Ok(QueueConnection::Sentinel {
            connection,
            backend: self.clone(),
            generation,
        })
    }

    async fn invalidate(&self, generation: u64) {
        let mut cached = self.cached.lock().await;
        if cached
            .as_ref()
            .map(|(current, _)| *current == generation)
            .unwrap_or(false)
        {
            cached.take();
            metrics::counter!("bridge_queue_redis_connection_invalidated_total", "mode" => "sentinel")
                .increment(1);
        }
    }
}

impl QueueRedis {
    async fn connection(&self) -> redis::RedisResult<QueueConnection> {
        match self {
            Self::Direct(connection) => Ok(QueueConnection::Direct(connection.clone())),
            Self::Sentinel(backend) => backend.connection().await,
        }
    }
}

impl ConnectionLike for QueueConnection {
    fn req_packed_command<'a>(&'a mut self, command: &'a Cmd) -> RedisFuture<'a, Value> {
        match self {
            Self::Direct(connection) => connection.req_packed_command(command),
            Self::Sentinel {
                connection,
                backend,
                generation,
            } => Box::pin(async move {
                let result = connection.req_packed_command(command).await;
                if let Err(error) = &result {
                    if error.is_unrecoverable_error() || error.is_timeout() {
                        backend.invalidate(*generation).await;
                    }
                }
                result
            }),
        }
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        pipeline: &'a redis::Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        match self {
            Self::Direct(connection) => connection.req_packed_commands(pipeline, offset, count),
            Self::Sentinel {
                connection,
                backend,
                generation,
            } => Box::pin(async move {
                let result = connection
                    .req_packed_commands(pipeline, offset, count)
                    .await;
                if let Err(error) = &result {
                    if error.is_unrecoverable_error() || error.is_timeout() {
                        backend.invalidate(*generation).await;
                    }
                }
                result
            }),
        }
    }

    fn get_db(&self) -> i64 {
        match self {
            Self::Direct(connection) => connection.get_db(),
            Self::Sentinel { connection, .. } => connection.get_db(),
        }
    }
}

pub struct QueueRuntime {
    pub cfg: QueueConfig,
    redis: QueueRedis,
    poll_times: Arc<tokio::sync::Mutex<HashMap<String, Instant>>>,
}

impl QueueRuntime {
    pub async fn connect(cfg: QueueConfig) -> anyhow::Result<Arc<Self>> {
        cfg.validate()?;
        let client = redis::Client::open(cfg.redis_url.as_str())?;
        let redis = if cfg.sentinel_urls.is_empty() {
            let manager_config = ConnectionManagerConfig::new()
                .set_connection_timeout(Duration::from_millis(cfg.redis_connect_timeout_ms))
                .set_response_timeout(Duration::from_millis(cfg.redis_command_timeout_ms))
                .set_number_of_retries(cfg.redis_reconnect_retries)
                .set_factor(cfg.redis_reconnect_base_ms)
                .set_exponent_base(2)
                .set_max_delay(cfg.redis_reconnect_max_ms);
            QueueRedis::Direct(Box::new(
                ConnectionManager::new_with_config(client, manager_config).await?,
            ))
        } else {
            let connection_info = client.get_connection_info();
            let tls_mode = match &connection_info.addr {
                ConnectionAddr::TcpTls {
                    insecure: false, ..
                } => Some(TlsMode::Secure),
                ConnectionAddr::TcpTls { insecure: true, .. } => Some(TlsMode::Insecure),
                _ => None,
            };
            let sentinel = SentinelClient::build(
                cfg.sentinel_urls.clone(),
                cfg.sentinel_service
                    .clone()
                    .expect("validated sentinel service"),
                Some(SentinelNodeConnectionInfo {
                    tls_mode,
                    redis_connection_info: Some(connection_info.redis.clone()),
                }),
                SentinelServerType::Master,
            )?;
            QueueRedis::Sentinel(Arc::new(SentinelRedis {
                client: tokio::sync::Mutex::new(sentinel),
                cached: tokio::sync::Mutex::new(None),
                next_generation: AtomicU64::new(1),
                connect_timeout: Duration::from_millis(cfg.redis_connect_timeout_ms),
                command_timeout: Duration::from_millis(cfg.redis_command_timeout_ms),
                reconnect_retries: cfg.redis_reconnect_retries,
                reconnect_base_ms: cfg.redis_reconnect_base_ms,
                reconnect_max_ms: cfg.redis_reconnect_max_ms,
            }))
        };
        let qr = Arc::new(Self {
            cfg,
            redis,
            poll_times: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        });
        qr.ensure_group().await?;
        Ok(qr)
    }

    pub fn stream_key(&self) -> String {
        format!("{}:queue", self.cfg.key_prefix)
    }

    pub fn dlq_key(&self) -> String {
        format!("{}:dlq", self.cfg.key_prefix)
    }

    pub fn job_key(&self, queue_id: &str) -> String {
        format!("{}:job:{queue_id}", self.cfg.key_prefix)
    }

    pub fn dedup_key(&self, scope_key: &str) -> String {
        format!("{}:dedup:{scope_key}", self.cfg.key_prefix)
    }

    pub async fn ensure_group(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut conn = self.redis.connection().await?;
        let stream_key = self.stream_key();
        let r: Result<String, redis::RedisError> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(&stream_key)
            .arg(GROUP_NAME)
            .arg("0")
            .arg("MKSTREAM")
            .query_async(&mut conn)
            .await;
        match r {
            Ok(_) => {}
            Err(e) if e.to_string().contains("BUSYGROUP") => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    pub async fn health_check(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut connection = self.redis.connection().await?;
        let value: i64 = redis::cmd("EVAL")
            .arg("return 1")
            .arg(0)
            .query_async(&mut connection)
            .await?;
        anyhow::ensure!(
            value == 1,
            "Redis readiness script returned an unexpected value"
        );
        Ok(())
    }

    pub async fn enqueue(self: &Arc<Self>, fr: &ForwardRequest) -> Result<(), EnqueueError> {
        if fr.body.len() > self.cfg.max_body_bytes {
            metrics::counter!("bridge_queue_reject_total", "reason" => "body_too_large")
                .increment(1);
            return Err(EnqueueError::BodyTooLarge);
        }

        let payload = serde_json::to_string(&QueuedPayload::from_forward(fr))
            .map_err(|_| EnqueueError::Serialization)?;
        let mut conn = self.redis.connection().await.map_err(EnqueueError::Redis)?;
        let result: (i64, String, i64) = redis::Script::new(ENQUEUE_SCRIPT_V1)
            .key(self.stream_key())
            .key(self.job_key(&fr.request_id))
            .arg(self.cfg.max_queue_len)
            .arg(&payload)
            .arg(now_ms())
            .arg(self.cfg.queue_ttl_sec)
            .invoke_async(&mut conn)
            .await
            .map_err(EnqueueError::Redis)?;
        if result.0 == 0 {
            metrics::counter!("bridge_queue_reject_total", "reason" => "queue_full").increment(1);
            return Err(EnqueueError::OverCapacity);
        }

        metrics::counter!("bridge_queue_enqueue_total").increment(1);
        metrics::gauge!("bridge_queue_depth").set(result.2 as f64);
        Ok(())
    }

    /// Returns `Err` if polling too fast for this id.
    pub async fn throttle_poll(self: &Arc<Self>, queue_id: &str) -> Result<(), ()> {
        let min = Duration::from_millis(self.cfg.poll_min_interval_ms.max(10));
        let mut map = self.poll_times.lock().await;
        if map.len() > 10_000 {
            map.retain(|_, t| t.elapsed() < Duration::from_secs(120));
        }
        let now = Instant::now();
        if let Some(last) = map.get(queue_id) {
            if now.duration_since(*last) < min {
                return Err(());
            }
        }
        map.insert(queue_id.to_string(), now);
        Ok(())
    }

    pub async fn read_job(self: &Arc<Self>, queue_id: &str) -> anyhow::Result<Option<JobRecord>> {
        let mut conn = self.redis.connection().await?;
        let jk = self.job_key(queue_id);
        let m: HashMap<String, String> = conn.hgetall(&jk).await?;
        if m.is_empty() {
            return Ok(None);
        }
        Ok(Some(JobRecord::from_hash(m)))
    }

    pub async fn prepare_delivery(
        self: &Arc<Self>,
        stream_id: &str,
        queue_id: &str,
        payload: &str,
    ) -> anyhow::Result<ClaimDecision> {
        let mut conn = self.redis.connection().await?;
        let result: (i64, i64) = redis::Script::new(PREPARE_DELIVERY_SCRIPT_V1)
            .key(self.stream_key())
            .key(self.job_key(queue_id))
            .key(self.dlq_key())
            .arg(GROUP_NAME)
            .arg(stream_id)
            .arg(now_ms())
            .arg(self.cfg.max_attempts)
            .arg(self.cfg.queue_ttl_sec)
            .arg(payload.chars().take(16_384).collect::<String>())
            .invoke_async(&mut conn)
            .await?;
        match result.0 {
            1 => Ok(ClaimDecision::Process {
                attempt: result.1.try_into().unwrap_or(u32::MAX),
            }),
            2 => Ok(ClaimDecision::Terminal),
            3 => {
                metrics::counter!("bridge_queue_dlq_total").increment(1);
                Ok(ClaimDecision::DeadLettered)
            }
            other => anyhow::bail!("unknown queue claim decision {other}"),
        }
    }

    pub async fn complete_success(
        self: &Arc<Self>,
        stream_id: &str,
        queue_id: &str,
        tun: &ForwardResponse,
    ) -> anyhow::Result<()> {
        let cap = self.cfg.max_result_body_bytes;
        let body = if tun.body.len() > cap {
            tun.body[..cap].to_vec()
        } else {
            tun.body.clone()
        };
        let truncated = tun.body.len() > cap;
        let headers_json = if tun.header_values.is_empty() {
            serde_json::to_string(&tun.headers)?
        } else {
            serde_json::to_string(
                &tun.header_values
                    .iter()
                    .map(|header| {
                        serde_json::json!({
                            "name": header.name,
                            "value_b64": B64.encode(&header.value),
                        })
                    })
                    .collect::<Vec<_>>(),
            )?
        };
        let mut conn = self.redis.connection().await?;
        redis::Script::new(COMPLETE_SUCCESS_SCRIPT_V1)
            .key(self.stream_key())
            .key(self.job_key(queue_id))
            .arg(GROUP_NAME)
            .arg(stream_id)
            .arg(tun.status_code)
            .arg(headers_json)
            .arg(B64.encode(&body))
            .arg(if truncated { 1 } else { 0 })
            .arg(self.cfg.queue_ttl_sec)
            .invoke_async::<i64>(&mut conn)
            .await?;
        Ok(())
    }

    pub async fn complete_failure(
        self: &Arc<Self>,
        stream_id: &str,
        queue_id: &str,
        payload: &str,
        err: &str,
    ) -> anyhow::Result<()> {
        let mut conn = self.redis.connection().await?;
        let e = err.chars().take(1024).collect::<String>();
        redis::Script::new(COMPLETE_FAILURE_SCRIPT_V1)
            .key(self.stream_key())
            .key(self.job_key(queue_id))
            .key(self.dlq_key())
            .arg(GROUP_NAME)
            .arg(stream_id)
            .arg(e)
            .arg(payload.chars().take(16_384).collect::<String>())
            .arg(self.cfg.queue_ttl_sec)
            .invoke_async::<i64>(&mut conn)
            .await?;
        metrics::counter!("bridge_queue_dlq_total").increment(1);
        Ok(())
    }

    pub async fn dead_letter_unparseable(
        self: &Arc<Self>,
        stream_id: &str,
        payload: &str,
        err: &str,
    ) -> anyhow::Result<()> {
        let mut conn = self.redis.connection().await?;
        redis::Script::new(DLQ_UNPARSEABLE_SCRIPT_V1)
            .key(self.stream_key())
            .key(self.dlq_key())
            .arg(GROUP_NAME)
            .arg(stream_id)
            .arg(err.chars().take(2_048).collect::<String>())
            .arg(payload.chars().take(16_384).collect::<String>())
            .invoke_async::<i64>(&mut conn)
            .await?;
        metrics::counter!("bridge_queue_dlq_total").increment(1);
        Ok(())
    }

    pub async fn try_claim_dedup(
        self: &Arc<Self>,
        scope_key: &str,
        queue_id: &str,
    ) -> anyhow::Result<bool> {
        let mut conn = self.redis.connection().await?;
        let decision: i64 = redis::Script::new(DEDUP_SCRIPT_V1)
            .key(self.dedup_key(scope_key))
            .arg(queue_id)
            .arg(self.cfg.dedup_ttl_sec)
            .invoke_async(&mut conn)
            .await?;
        Ok(decision == 1)
    }

    pub async fn ack_terminal(
        self: &Arc<Self>,
        stream_id: &str,
        queue_id: &str,
    ) -> anyhow::Result<()> {
        let mut conn = self.redis.connection().await?;
        redis::Script::new(ACK_TERMINAL_SCRIPT_V1)
            .key(self.stream_key())
            .key(self.job_key(queue_id))
            .arg(GROUP_NAME)
            .arg(stream_id)
            .invoke_async::<i64>(&mut conn)
            .await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn pending_count(self: &Arc<Self>) -> anyhow::Result<usize> {
        let mut conn = self.redis.connection().await?;
        let reply: StreamPendingReply = conn.xpending(self.stream_key(), GROUP_NAME).await?;
        Ok(reply.count())
    }

    pub async fn pending_oldest_idle_seconds(self: &Arc<Self>) -> anyhow::Result<f64> {
        let mut conn = self.redis.connection().await?;
        let reply: StreamPendingCountReply = conn
            .xpending_count(self.stream_key(), GROUP_NAME, "-", "+", 1)
            .await?;
        Ok(reply
            .ids
            .first()
            .map_or(0.0, |entry| entry.last_delivered_ms as f64 / 1_000.0))
    }

    pub async fn read_batch(
        self: &Arc<Self>,
        consumer_name: &str,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut conn = self.redis.connection().await?;
        let opts = StreamReadOptions::default()
            .group(GROUP_NAME, consumer_name)
            .count(32)
            .block(2000);
        let stream_key = self.stream_key();
        let reply: StreamReadReply = conn.xread_options(&[&stream_key], &[">"], &opts).await?;
        Ok(payloads_from_stream_ids(
            reply.keys.into_iter().flat_map(|key| key.ids),
        ))
    }

    pub async fn reclaim_batch(
        self: &Arc<Self>,
        consumer_name: &str,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut conn = self.redis.connection().await?;
        let reply: StreamAutoClaimReply = conn
            .xautoclaim_options(
                self.stream_key(),
                GROUP_NAME,
                consumer_name,
                self.cfg.reclaim_idle_ms as usize,
                "0-0",
                StreamAutoClaimOptions::default().count(32),
            )
            .await?;
        if !reply.deleted_ids.is_empty() {
            metrics::counter!("bridge_queue_reclaim_deleted_total")
                .increment(reply.deleted_ids.len() as u64);
        }
        let reclaimed = payloads_from_stream_ids(reply.claimed);
        if !reclaimed.is_empty() {
            metrics::counter!("bridge_queue_reclaimed_total").increment(reclaimed.len() as u64);
        }
        Ok(reclaimed)
    }
}

fn payloads_from_stream_ids(
    ids: impl IntoIterator<Item = redis::streams::StreamId>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for sid in ids {
        if let Some(value) = sid.map.get("payload") {
            if let Ok(payload) = String::from_redis_value(value) {
                out.push((sid.id, payload));
            }
        }
    }
    out
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize)]
pub struct JobRecord {
    pub status: String,
    pub http_status: Option<u32>,
    pub headers_json: Option<String>,
    pub body_b64: Option<String>,
    pub body_truncated: bool,
    pub error: Option<String>,
}

impl JobRecord {
    fn from_hash(m: HashMap<String, String>) -> Self {
        let status = m.get("status").cloned().unwrap_or_else(|| "unknown".into());
        let http_status = m
            .get("http_status")
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|&c| c > 0);
        let headers_json = m.get("headers_json").cloned().filter(|s| !s.is_empty());
        let body_b64 = m.get("body_b64").cloned().filter(|s| !s.is_empty());
        let body_truncated = m.get("body_truncated").map(|s| s == "1").unwrap_or(false);
        let error = m.get("error").cloned().filter(|s| !s.is_empty());
        Self {
            status,
            http_status,
            headers_json,
            body_b64,
            body_truncated,
            error,
        }
    }
}

pub fn parse_queued_payload(json: &str) -> anyhow::Result<ForwardRequest> {
    let p: QueuedPayload = serde_json::from_str(json)?;
    p.into_forward()
}

/// One worker loop: read stream, forward, ack or DLQ.
pub async fn worker_loop(
    state: crate::AppState,
    qr: Arc<QueueRuntime>,
    sem: Arc<tokio::sync::Semaphore>,
    consumer_name: String,
) {
    loop {
        if state.readiness.is_draining() {
            return;
        }
        match qr.pending_oldest_idle_seconds().await {
            Ok(age) => metrics::gauge!("bridge_queue_pel_oldest_age_seconds").set(age),
            Err(error) => debug!(?error, "queue XPENDING age probe failed"),
        }
        let reclaimed = match qr.reclaim_batch(&consumer_name).await {
            Ok(batch) => batch,
            Err(e) => {
                error!(?e, "queue xautoclaim");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let batch = if reclaimed.is_empty() {
            match qr.read_batch(&consumer_name).await {
                Ok(batch) => batch,
                Err(e) => {
                    error!(?e, "queue xreadgroup");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            }
        } else {
            reclaimed
        };
        if batch.is_empty() {
            continue;
        }
        for (stream_id, payload_json) in batch {
            let permit = match sem.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => return,
            };
            let st = state.clone();
            let qr2 = qr.clone();
            let Some(active_request) = st.readiness.try_admit() else {
                return;
            };
            tokio::spawn(async move {
                let _permit = permit;
                let _active_request = active_request;
                process_one(st, qr2, stream_id, payload_json).await;
            });
        }
    }
}

async fn process_one(
    state: crate::AppState,
    qr: Arc<QueueRuntime>,
    stream_id: String,
    payload_json: String,
) {
    let fr = match parse_queued_payload(&payload_json) {
        Ok(f) => f,
        Err(e) => {
            warn!(?e, %stream_id, "bad queue payload");
            if let Err(dlq_error) = qr
                .dead_letter_unparseable(&stream_id, &payload_json, &format!("parse: {e}"))
                .await
            {
                error!(?dlq_error, %stream_id, "failed to persist unparseable payload to DLQ; leaving pending");
            }
            return;
        }
    };
    let qid = fr.request_id.clone();

    match qr.prepare_delivery(&stream_id, &qid, &payload_json).await {
        Ok(ClaimDecision::Process { .. }) => {}
        Ok(ClaimDecision::Terminal) => {
            if let Err(error) = qr.ack_terminal(&stream_id, &qid).await {
                warn!(?error, %qid, %stream_id, "failed to ack terminal queue replay");
            }
            return;
        }
        Ok(ClaimDecision::DeadLettered) => return,
        Err(error) => {
            metrics::counter!("queue_dependency_unavailable_total", "operation" => "prepare_delivery")
                .increment(1);
            warn!(?error, %qid, %stream_id, "cannot prepare queue delivery; leaving pending");
            return;
        }
    }

    if fr.deadline_unix_ms > 0 && fr.deadline_unix_ms <= now_ms() {
        metrics::counter!("bridge_queue_expired_total").increment(1);
        if let Err(error) = qr
            .complete_failure(
                &stream_id,
                &qid,
                &payload_json,
                "request deadline expired before worker dispatch",
            )
            .await
        {
            warn!(?error, %qid, %stream_id, "failed to persist expired queue job; leaving pending");
        }
        warn!(%qid, attempt_id = %fr.attempt_id, "expired queue job dropped before forwarding");
        return;
    }

    let dedup_scope = if fr.idempotency_key.is_empty() {
        fr.request_id.clone()
    } else {
        format!("{}:{}", fr.app_id, fr.idempotency_key)
    };
    match qr.try_claim_dedup(&dedup_scope, &qid).await {
        Ok(true) => {}
        Ok(false) => {
            if let Err(error) = qr
                .complete_failure(
                    &stream_id,
                    &qid,
                    &payload_json,
                    "duplicate idempotency scope owned by another queue job",
                )
                .await
            {
                warn!(?error, %qid, %stream_id, "failed to persist duplicate queue job; leaving pending");
            }
            return;
        }
        Err(error) => {
            metrics::counter!("queue_dependency_unavailable_total", "operation" => "dedup")
                .increment(1);
            warn!(?error, %qid, %stream_id, "dedup unavailable; refusing dispatch and leaving pending");
            return;
        }
    }

    let t0 = Instant::now();
    let tun = match crate::forward_request(&state, fr).await {
        Ok(t) => t,
        Err(e) => {
            let err_s = e.to_string();
            warn!(%qid, err=%err_s, "worker forward failed");
            if let Err(error) = qr
                .complete_failure(&stream_id, &qid, &payload_json, err_s.as_str())
                .await
            {
                error!(?error, %qid, %stream_id, "failed to persist queue failure; leaving pending");
            }
            metrics::counter!("bridge_worker_forward_total", "result" => "error").increment(1);
            return;
        }
    };

    if let Err(error) = qr.complete_success(&stream_id, &qid, &tun).await {
        error!(?error, %qid, %stream_id, "failed atomic queue completion; leaving pending");
        return;
    }
    metrics::counter!("bridge_worker_forward_total", "result" => "ok").increment(1);
    metrics::histogram!(
        "bridge_worker_latency_seconds",
        "service" => "http-tunnel-bridge"
    )
    .record(t0.elapsed().as_secs_f64());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(redis_url: String) -> QueueConfig {
        QueueConfig {
            redis_url,
            sentinel_urls: Vec::new(),
            sentinel_service: None,
            redis_connect_timeout_ms: 500,
            redis_command_timeout_ms: 3_000,
            redis_reconnect_retries: 2,
            redis_reconnect_base_ms: 10,
            redis_reconnect_max_ms: 50,
            key_prefix: "sag:dataplane".into(),
            soft_inflight: 1,
            hard_inflight: 2,
            max_queue_len: 1,
            max_body_bytes: 1024,
            queue_ttl_sec: 60,
            worker_concurrency: 1,
            max_result_body_bytes: 1024,
            poll_min_interval_ms: 10,
            dedup_ttl_sec: 60,
            reclaim_idle_ms: 30,
            max_forward_deadline_ms: 10,
            reclaim_jitter_margin_ms: 10,
            max_attempts: 3,
        }
    }

    #[test]
    fn reclaim_idle_must_exceed_forward_deadline_and_jitter() {
        let mut config = test_config("redis://127.0.0.1:6379/15".into());
        assert!(config.validate().is_ok());
        config.reclaim_idle_ms = 20;
        assert!(config.validate().is_err());
        config.reclaim_idle_ms = 19;
        assert!(config.validate().is_err());
    }

    #[test]
    fn admission_sync_limit_must_not_exceed_hard_ingress_limit() {
        let mut config = test_config("redis://127.0.0.1:6379/15".into());
        config.soft_inflight = config.hard_inflight + 1;
        assert!(config.validate().is_err());
        config.soft_inflight = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn redis_connection_contract_supports_tls_and_sentinel_without_secret_disclosure() {
        let mut direct =
            test_config("rediss://queue-user:super-secret@managed-redis.example:6380/2".into());
        assert_eq!(direct.deployment_mode(), RedisDeploymentMode::DirectTls);
        let safe = direct.safe_endpoint();
        assert!(safe.contains("managed-redis.example:6380/2"));
        assert!(!safe.contains("queue-user"));
        assert!(!safe.contains("super-secret"));

        direct.sentinel_urls = vec![
            "rediss://sentinel-1.example:26379".into(),
            "rediss://sentinel-2.example:26379".into(),
            "rediss://sentinel-3.example:26379".into(),
        ];
        direct.sentinel_service = Some("sag-queue-primary".into());
        assert_eq!(direct.deployment_mode(), RedisDeploymentMode::Sentinel);
        assert!(direct.validate().is_ok());

        direct.sentinel_service = None;
        assert!(direct.validate().is_err());
    }

    #[tokio::test]
    #[ignore = "requires Redis 7 at SAG_TEST_REDIS_URL"]
    async fn acknowledging_a_job_releases_stream_capacity() {
        let redis_url = std::env::var("SAG_TEST_REDIS_URL")
            .expect("set SAG_TEST_REDIS_URL to an isolated Redis database");
        let client = redis::Client::open(redis_url.as_str()).unwrap();
        let mut conn = ConnectionManager::new(client).await.unwrap();
        redis::cmd("FLUSHDB")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();

        let runtime = QueueRuntime::connect(test_config(redis_url)).await.unwrap();

        let request = ForwardRequest {
            request_id: "capacity-release-test".to_string(),
            app_id: "app-test".to_string(),
            method: "GET".to_string(),
            path: "/health".to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
            ..Default::default()
        };
        runtime.enqueue(&request).await.unwrap();
        let batch = runtime.read_batch("capacity-test-consumer").await.unwrap();
        assert_eq!(batch.len(), 1);

        runtime
            .complete_success(
                &batch[0].0,
                &request.request_id,
                &ForwardResponse {
                    request_id: request.request_id.clone(),
                    attempt_id: request.attempt_id.clone(),
                    status_code: 200,
                    headers: HashMap::new(),
                    body: b"ok".to_vec(),
                    header_values: Vec::new(),
                    stream_epoch: String::new(),
                },
            )
            .await
            .unwrap();

        let remaining: u64 = conn.xlen(runtime.stream_key()).await.unwrap();
        assert_eq!(
            remaining, 0,
            "acknowledged entries must not consume queue capacity"
        );
    }
}

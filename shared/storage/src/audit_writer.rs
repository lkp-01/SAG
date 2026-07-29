use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};

use crate::{AuditLogRecord, AuditLogsStore, StorageError, StorageStore};

type AuditSinkFuture<'a> = Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send + 'a>>;

trait AuditSink: Send + Sync + 'static {
    fn write_batch<'a>(&'a self, records: &'a [AuditLogRecord]) -> AuditSinkFuture<'a>;
}

struct StorageAuditSink {
    store: StorageStore,
}

impl AuditSink for StorageAuditSink {
    fn write_batch<'a>(&'a self, records: &'a [AuditLogRecord]) -> AuditSinkFuture<'a> {
        Box::pin(AuditLogsStore::insert_batch(&self.store, records))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AuditWriterConfig {
    pub capacity: usize,
    pub batch_size: usize,
    pub flush_interval: Duration,
    pub drain_timeout: Duration,
}

impl Default for AuditWriterConfig {
    fn default() -> Self {
        Self {
            capacity: 4_096,
            batch_size: 100,
            flush_interval: Duration::from_millis(250),
            drain_timeout: Duration::from_secs(5),
        }
    }
}

impl AuditWriterConfig {
    pub fn from_env() -> Result<Self, StorageError> {
        fn usize_value(name: &str, default: usize) -> Result<usize, StorageError> {
            std::env::var(name)
                .ok()
                .map(|raw| {
                    raw.parse::<usize>().map_err(|_| {
                        StorageError::Configuration(format!("{name} must be a positive integer"))
                    })
                })
                .transpose()
                .map(|value| value.unwrap_or(default))
        }

        fn duration_value(name: &str, default_ms: u64) -> Result<Duration, StorageError> {
            std::env::var(name)
                .ok()
                .map(|raw| {
                    raw.parse::<u64>().map(Duration::from_millis).map_err(|_| {
                        StorageError::Configuration(format!(
                            "{name} must be a positive integer number of milliseconds"
                        ))
                    })
                })
                .transpose()
                .map(|value| value.unwrap_or_else(|| Duration::from_millis(default_ms)))
        }

        let config = Self {
            capacity: usize_value("SAG_AUDIT_QUEUE_CAPACITY", 4_096)?,
            batch_size: usize_value("SAG_AUDIT_BATCH_SIZE", 100)?,
            flush_interval: duration_value("SAG_AUDIT_FLUSH_INTERVAL_MS", 250)?,
            drain_timeout: duration_value("SAG_AUDIT_DRAIN_TIMEOUT_MS", 5_000)?,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), StorageError> {
        if self.capacity == 0
            || self.batch_size == 0
            || self.flush_interval.is_zero()
            || self.drain_timeout.is_zero()
        {
            return Err(StorageError::Configuration(
                "audit writer capacity, batch size, flush interval, and drain timeout must all be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEnqueueError {
    Full,
    Closed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuditShutdownReport {
    pub dropped: usize,
    pub timed_out: bool,
}

struct QueuedAudit {
    record: AuditLogRecord,
    enqueued_at: Instant,
}

#[derive(Default)]
struct WorkerReport {
    dropped: usize,
}

struct AuditWriterInner {
    sender: Mutex<Option<mpsc::Sender<QueuedAudit>>>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    worker: Mutex<Option<tokio::task::JoinHandle<WorkerReport>>>,
    queue_depth: Arc<AtomicUsize>,
    drain_timeout: Duration,
}

#[derive(Clone)]
pub struct AuditWriter {
    inner: Arc<AuditWriterInner>,
}

impl AuditWriter {
    pub fn new(store: StorageStore, config: AuditWriterConfig) -> Result<Self, StorageError> {
        Self::with_sink(config, Arc::new(StorageAuditSink { store }))
    }

    pub fn from_env(store: StorageStore) -> Result<Self, StorageError> {
        Self::new(store, AuditWriterConfig::from_env()?)
    }

    fn with_sink(
        config: AuditWriterConfig,
        sink: Arc<dyn AuditSink>,
    ) -> Result<Self, StorageError> {
        config.validate()?;
        let (sender, receiver) = mpsc::channel(config.capacity);
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let queue_depth = Arc::new(AtomicUsize::new(0));
        let worker_depth = Arc::clone(&queue_depth);
        let worker = tokio::spawn(run_worker(
            receiver,
            shutdown_receiver,
            sink,
            config,
            worker_depth,
        ));

        Ok(Self {
            inner: Arc::new(AuditWriterInner {
                sender: Mutex::new(Some(sender)),
                shutdown: Mutex::new(Some(shutdown_sender)),
                worker: Mutex::new(Some(worker)),
                queue_depth,
                drain_timeout: config.drain_timeout,
            }),
        })
    }

    pub fn try_record(&self, record: AuditLogRecord) -> Result<(), AuditEnqueueError> {
        let guard = self.inner.sender.lock().expect("audit sender poisoned");
        let Some(sender) = guard.as_ref() else {
            metrics::counter!("audit_dropped_total", "reason" => "closed").increment(1);
            return Err(AuditEnqueueError::Closed);
        };
        let queued = QueuedAudit {
            record,
            enqueued_at: Instant::now(),
        };
        // Account before publishing to the channel: the worker can receive and
        // flush immediately on another executor thread.
        let depth = self.inner.queue_depth.fetch_add(1, Ordering::AcqRel) + 1;
        match sender.try_send(queued) {
            Ok(()) => {
                metrics::gauge!("audit_queue_depth").set(depth as f64);
                metrics::counter!("audit_enqueued_total").increment(1);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                let depth = self.inner.queue_depth.fetch_sub(1, Ordering::AcqRel) - 1;
                metrics::gauge!("audit_queue_depth").set(depth as f64);
                metrics::counter!("audit_dropped_total", "reason" => "full").increment(1);
                Err(AuditEnqueueError::Full)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                let depth = self.inner.queue_depth.fetch_sub(1, Ordering::AcqRel) - 1;
                metrics::gauge!("audit_queue_depth").set(depth as f64);
                metrics::counter!("audit_dropped_total", "reason" => "closed").increment(1);
                Err(AuditEnqueueError::Closed)
            }
        }
    }

    pub fn queue_depth(&self) -> usize {
        self.inner.queue_depth.load(Ordering::Acquire)
    }

    pub async fn shutdown(&self) -> AuditShutdownReport {
        self.inner
            .sender
            .lock()
            .expect("audit sender poisoned")
            .take();
        if let Some(shutdown) = self
            .inner
            .shutdown
            .lock()
            .expect("audit shutdown sender poisoned")
            .take()
        {
            let _ = shutdown.send(());
        }
        let worker = self
            .inner
            .worker
            .lock()
            .expect("audit worker handle poisoned")
            .take();
        let Some(mut worker) = worker else {
            return AuditShutdownReport::default();
        };

        match tokio::time::timeout(self.inner.drain_timeout, &mut worker).await {
            Ok(Ok(report)) => AuditShutdownReport {
                dropped: report.dropped,
                timed_out: false,
            },
            Ok(Err(_)) => {
                let dropped = self.inner.queue_depth.swap(0, Ordering::AcqRel);
                metrics::gauge!("audit_queue_depth").set(0.0);
                metrics::counter!("audit_dropped_total", "reason" => "worker_join_error")
                    .increment(dropped as u64);
                AuditShutdownReport {
                    dropped,
                    timed_out: false,
                }
            }
            Err(_) => {
                worker.abort();
                let _ = worker.await;
                let dropped = self.inner.queue_depth.swap(0, Ordering::AcqRel);
                metrics::gauge!("audit_queue_depth").set(0.0);
                metrics::counter!("audit_dropped_total", "reason" => "shutdown_timeout")
                    .increment(dropped as u64);
                AuditShutdownReport {
                    dropped,
                    timed_out: true,
                }
            }
        }
    }
}

async fn flush_batch(
    sink: &dyn AuditSink,
    batch: &mut Vec<QueuedAudit>,
    queue_depth: &AtomicUsize,
) -> usize {
    if batch.is_empty() {
        return 0;
    }
    metrics::gauge!("audit_oldest_buffered_seconds")
        .set(batch[0].enqueued_at.elapsed().as_secs_f64());
    let records = batch
        .iter()
        .map(|queued| queued.record.clone())
        .collect::<Vec<_>>();
    let count = records.len();
    let dropped = match sink.write_batch(&records).await {
        Ok(()) => {
            metrics::counter!("audit_batch_write_total").increment(1);
            0
        }
        Err(_) => {
            metrics::counter!("audit_write_failed_total").increment(1);
            metrics::counter!("audit_dropped_total", "reason" => "sink_error")
                .increment(count as u64);
            count
        }
    };
    batch.clear();
    let remaining = queue_depth.fetch_sub(count, Ordering::AcqRel) - count;
    metrics::gauge!("audit_queue_depth").set(remaining as f64);
    metrics::gauge!("audit_oldest_buffered_seconds").set(0.0);
    dropped
}

async fn run_worker(
    mut receiver: mpsc::Receiver<QueuedAudit>,
    mut shutdown: oneshot::Receiver<()>,
    sink: Arc<dyn AuditSink>,
    config: AuditWriterConfig,
    queue_depth: Arc<AtomicUsize>,
) -> WorkerReport {
    let mut batch = Vec::with_capacity(config.batch_size);
    let mut dropped = 0;
    let timer = tokio::time::sleep(config.flush_interval);
    tokio::pin!(timer);

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                receiver.close();
                while let Some(queued) = receiver.recv().await {
                    batch.push(queued);
                    if batch.len() >= config.batch_size {
                        dropped += flush_batch(sink.as_ref(), &mut batch, queue_depth.as_ref()).await;
                    }
                }
                dropped += flush_batch(sink.as_ref(), &mut batch, queue_depth.as_ref()).await;
                break;
            }
            item = receiver.recv() => {
                match item {
                    Some(queued) => {
                        batch.push(queued);
                        if batch.len() >= config.batch_size {
                            dropped += flush_batch(sink.as_ref(), &mut batch, queue_depth.as_ref()).await;
                            timer.as_mut().reset(tokio::time::Instant::now() + config.flush_interval);
                        }
                    }
                    None => {
                        dropped += flush_batch(sink.as_ref(), &mut batch, queue_depth.as_ref()).await;
                        break;
                    }
                }
            }
            _ = &mut timer, if !batch.is_empty() => {
                dropped += flush_batch(sink.as_ref(), &mut batch, queue_depth.as_ref()).await;
                timer.as_mut().reset(tokio::time::Instant::now() + config.flush_interval);
            }
        }
    }

    WorkerReport { dropped }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::sync::Notify;

    use super::*;
    use crate::{AuditLogRecord, StorageError};

    #[derive(Default)]
    struct FakeSink {
        batches: Mutex<Vec<Vec<AuditLogRecord>>>,
        entered: Notify,
        release: Notify,
        paused: bool,
        fail: bool,
    }

    impl FakeSink {
        fn paused() -> Self {
            Self {
                paused: true,
                ..Self::default()
            }
        }

        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::default()
            }
        }
    }

    impl AuditSink for FakeSink {
        fn write_batch<'a>(&'a self, records: &'a [AuditLogRecord]) -> AuditSinkFuture<'a> {
            Box::pin(async move {
                self.entered.notify_waiters();
                if self.paused {
                    self.release.notified().await;
                }
                if self.fail {
                    return Err(StorageError::Task("fake audit sink failure".into()));
                }
                self.batches.lock().unwrap().push(records.to_vec());
                Ok(())
            })
        }
    }

    fn record(index: usize) -> AuditLogRecord {
        AuditLogRecord {
            id: format!("00000000-0000-4000-8000-{index:012}"),
            ts_ms: index as i64,
            service: "test".into(),
            user_id: "user".into(),
            app_id: "app".into(),
            path: "/test".into(),
            method: "GET".into(),
            latency_ms: 1,
            decision: "ALLOW".into(),
            result: "200".into(),
            trace_id: format!("trace-{index}"),
            extra_json: "{}".into(),
        }
    }

    fn config() -> AuditWriterConfig {
        AuditWriterConfig {
            capacity: 1,
            batch_size: 1,
            flush_interval: Duration::from_secs(60),
            drain_timeout: Duration::from_millis(100),
        }
    }

    #[tokio::test]
    async fn full_queue_returns_immediately_without_spawning() {
        let sink = Arc::new(FakeSink::paused());
        let writer = AuditWriter::with_sink(config(), sink.clone()).unwrap();

        writer.try_record(record(1)).unwrap();
        sink.entered.notified().await;
        writer.try_record(record(2)).unwrap();

        let started = std::time::Instant::now();
        assert_eq!(writer.try_record(record(3)), Err(AuditEnqueueError::Full));
        assert!(started.elapsed() < Duration::from_millis(20));
        sink.release.notify_waiters();
        writer.shutdown().await;
    }

    #[tokio::test]
    async fn single_worker_flushes_on_batch_size_and_interval() {
        let sink = Arc::new(FakeSink::default());
        let writer = AuditWriter::with_sink(
            AuditWriterConfig {
                capacity: 4,
                batch_size: 2,
                flush_interval: Duration::from_millis(20),
                drain_timeout: Duration::from_secs(1),
            },
            sink.clone(),
        )
        .unwrap();

        writer.try_record(record(1)).unwrap();
        writer.try_record(record(2)).unwrap();
        tokio::time::timeout(Duration::from_secs(1), sink.entered.notified())
            .await
            .unwrap();
        writer.try_record(record(3)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let report = writer.shutdown().await;

        let batches = sink.batches.lock().unwrap();
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(report.dropped, 0);
    }

    #[tokio::test]
    async fn shutdown_drain_timeout_counts_dropped_records() {
        let sink = Arc::new(FakeSink::paused());
        let writer = AuditWriter::with_sink(
            AuditWriterConfig {
                capacity: 2,
                batch_size: 1,
                flush_interval: Duration::from_secs(60),
                drain_timeout: Duration::from_millis(30),
            },
            sink.clone(),
        )
        .unwrap();
        writer.try_record(record(1)).unwrap();
        sink.entered.notified().await;
        writer.try_record(record(2)).unwrap();

        let report = writer.shutdown().await;
        assert!(report.timed_out);
        assert_eq!(report.dropped, 2);
    }

    #[tokio::test]
    async fn sink_failure_keeps_memory_bounded() {
        let sink = Arc::new(FakeSink::failing());
        let writer = AuditWriter::with_sink(
            AuditWriterConfig {
                capacity: 2,
                batch_size: 1,
                flush_interval: Duration::from_millis(5),
                drain_timeout: Duration::from_secs(1),
            },
            sink,
        )
        .unwrap();

        let mut full = 0;
        for index in 0..1000 {
            if writer.try_record(record(index)) == Err(AuditEnqueueError::Full) {
                full += 1;
            }
        }
        assert!(full > 0);
        assert!(writer.queue_depth() <= 2);
        writer.shutdown().await;
    }
}

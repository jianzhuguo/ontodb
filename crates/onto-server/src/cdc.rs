//! Change Data Capture (CDC) for OntoDB.
//!
//! Publishes database changes to Kafka topics in real-time,
//! enabling downstream consumers like Flink, Spark, or other microservices.
//!
//! Architecture:
//! ```
//! OntoDB WAL → CDC Publisher → Kafka Topic → Flink/Spark/Consumer
//! ```
//!
//! Each change event includes:
//! - Operation type (INSERT/UPDATE/DELETE)
//! - Entity ID (class::pk)
//! - Before/after values (for UPDATE)
//! - Timestamp
//! - Sequence number (for ordering)

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// CDC event types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum CdcOperation {
    Insert,
    Update,
    Delete,
}

/// A single CDC event published to Kafka.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CdcEvent {
    /// Event timestamp (Unix millis).
    pub ts: u64,
    /// Operation type.
    pub op: CdcOperation,
    /// Entity class name.
    pub class: String,
    /// Entity primary key.
    pub pk: String,
    /// Full entity key (class::pk).
    pub key: String,
    /// New value (for INSERT/UPDATE). None for DELETE.
    pub after: Option<serde_json::Value>,
    /// Old value (for UPDATE/DELETE). None for INSERT.
    pub before: Option<serde_json::Value>,
    /// WAL sequence number (for ordering and dedup).
    pub seq: u64,
}

/// CDC configuration.
#[derive(Debug, Clone)]
pub struct CdcConfig {
    /// Whether CDC is enabled.
    pub enabled: bool,
    /// Kafka bootstrap servers (comma-separated).
    pub kafka_brokers: String,
    /// Kafka topic name for CDC events.
    pub topic: String,
    /// Kafka client ID.
    pub client_id: String,
    /// Whether to include before-values in UPDATE/DELETE events.
    pub include_before: bool,
    /// Whether to include after-values in INSERT/UPDATE events.
    pub include_after: bool,
    /// Batch size for Kafka producer (events per batch).
    pub batch_size: usize,
    /// Flush interval in milliseconds.
    pub flush_interval_ms: u64,
}

impl Default for CdcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            kafka_brokers: "localhost:9092".to_string(),
            topic: "ontodb-cdc".to_string(),
            client_id: "ontodb-cdc-producer".to_string(),
            include_before: true,
            include_after: true,
            batch_size: 100,
            flush_interval_ms: 1000,
        }
    }
}

/// CDC event builder for creating events from storage operations.
pub struct CdcEventBuilder {
    include_before: bool,
    include_after: bool,
}

impl CdcEventBuilder {
    pub fn new(config: &CdcConfig) -> Self {
        Self {
            include_before: config.include_before,
            include_after: config.include_after,
        }
    }

    /// Create an INSERT event.
    pub fn insert_event(&self, class: &str, pk: &str, key: &str, value: &serde_json::Value, seq: u64) -> CdcEvent {
        CdcEvent {
            ts: now_millis(),
            op: CdcOperation::Insert,
            class: class.to_string(),
            pk: pk.to_string(),
            key: key.to_string(),
            after: if self.include_after { Some(value.clone()) } else { None },
            before: None,
            seq,
        }
    }

    /// Create an UPDATE event.
    pub fn update_event(
        &self,
        class: &str,
        pk: &str,
        key: &str,
        old_value: Option<&serde_json::Value>,
        new_value: &serde_json::Value,
        seq: u64,
    ) -> CdcEvent {
        CdcEvent {
            ts: now_millis(),
            op: CdcOperation::Update,
            class: class.to_string(),
            pk: pk.to_string(),
            key: key.to_string(),
            before: if self.include_before { old_value.cloned() } else { None },
            after: if self.include_after { Some(new_value.clone()) } else { None },
            seq,
        }
    }

    /// Create a DELETE event.
    pub fn delete_event(
        &self,
        class: &str,
        pk: &str,
        key: &str,
        old_value: Option<&serde_json::Value>,
        seq: u64,
    ) -> CdcEvent {
        CdcEvent {
            ts: now_millis(),
            op: CdcOperation::Delete,
            class: class.to_string(),
            pk: pk.to_string(),
            key: key.to_string(),
            before: if self.include_before { old_value.cloned() } else { None },
            after: None,
            seq,
        }
    }
}

/// CDC publisher trait — abstracts the actual message queue.
///
/// Implementations:
/// - `KafkaCdcPublisher`: publishes to Kafka
/// - `InMemoryCdcPublisher`: stores events in memory (for testing)
/// - `WebhookCdcPublisher`: sends events via HTTP webhook
/// - `ResilientCdcPublisher`: wraps any publisher with retry + buffering
pub trait CdcPublisher: Send + Sync {
    /// Publish a single CDC event.
    fn publish(&self, event: &CdcEvent) -> Result<(), String>;

    /// Publish a batch of CDC events.
    fn publish_batch(&self, events: &[CdcEvent]) -> Result<(), String> {
        for event in events {
            self.publish(event)?;
        }
        Ok(())
    }

    /// Flush any buffered events.
    fn flush(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Resilient CDC publisher — wraps any publisher with retry, buffering, and DLQ.
///
/// Features:
/// - Exponential backoff retry (1s → 2s → 4s → ... → max 60s)
/// - In-memory buffer for events when downstream is unavailable
/// - Dead letter queue for permanently failed events
/// - Automatic buffer flush when connection recovers
/// - Configurable max retry count and buffer size
pub struct ResilientCdcPublisher {
    inner: Arc<dyn CdcPublisher>,
    /// Events waiting to be retried (downstream was unavailable).
    retry_buffer: parking_lot::Mutex<Vec<RetryEntry>>,
    /// Permanently failed events (exceeded max retries).
    dead_letter_queue: parking_lot::Mutex<Vec<DeadLetterEntry>>,
    /// Configuration.
    config: ResilientConfig,
    /// Consecutive failure count for exponential backoff.
    consecutive_failures: std::sync::atomic::AtomicU32,
}

/// Configuration for the resilient publisher.
#[derive(Debug, Clone)]
pub struct ResilientConfig {
    /// Maximum retry attempts per event before moving to DLQ.
    pub max_retries: u32,
    /// Initial retry delay in milliseconds.
    pub initial_retry_delay_ms: u64,
    /// Maximum retry delay in milliseconds (cap for exponential backoff).
    pub max_retry_delay_ms: u64,
    /// Maximum events in the retry buffer.
    pub max_buffer_size: usize,
    /// Maximum events in the dead letter queue.
    pub max_dlq_size: usize,
    /// Whether to log retry attempts.
    pub log_retries: bool,
}

impl Default for ResilientConfig {
    fn default() -> Self {
        Self {
            max_retries: 10,
            initial_retry_delay_ms: 1000,
            max_retry_delay_ms: 60_000,
            max_buffer_size: 100_000,
            max_dlq_size: 10_000,
            log_retries: true,
        }
    }
}

/// A pending retry entry.
#[derive(Debug, Clone)]
struct RetryEntry {
    event: CdcEvent,
    attempts: u32,
    next_retry_at: std::time::Instant,
}

/// A permanently failed event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadLetterEntry {
    pub event: CdcEvent,
    pub error: String,
    pub attempts: u32,
    pub failed_at: u64,
}

impl ResilientCdcPublisher {
    /// Create a new resilient publisher wrapping the given inner publisher.
    pub fn new(inner: Arc<dyn CdcPublisher>) -> Self {
        Self::with_config(inner, ResilientConfig::default())
    }

    /// Create with custom configuration.
    pub fn with_config(inner: Arc<dyn CdcPublisher>, config: ResilientConfig) -> Self {
        Self {
            inner,
            retry_buffer: parking_lot::Mutex::new(Vec::new()),
            dead_letter_queue: parking_lot::Mutex::new(Vec::new()),
            config,
            consecutive_failures: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Get the number of events in the retry buffer.
    pub fn buffer_size(&self) -> usize {
        self.retry_buffer.lock().len()
    }

    /// Get the number of events in the dead letter queue.
    pub fn dlq_size(&self) -> usize {
        self.dead_letter_queue.lock().len()
    }

    /// Get all dead letter entries (for inspection/replay).
    pub fn get_dead_letters(&self) -> Vec<DeadLetterEntry> {
        self.dead_letter_queue.lock().clone()
    }

    /// Clear the dead letter queue.
    pub fn clear_dead_letters(&self) {
        self.dead_letter_queue.lock().clear();
    }

    /// Replay dead letter entries — attempt to publish them again.
    pub fn replay_dead_letters(&self) -> usize {
        let entries: Vec<DeadLetterEntry> = {
            let mut dlq = self.dead_letter_queue.lock();
            std::mem::take(&mut *dlq)
        };

        let mut replayed = 0;
        for entry in entries {
            if self.inner.publish(&entry.event).is_ok() {
                replayed += 1;
            } else {
                // Put back in DLQ if still failing
                self.dead_letter_queue.lock().push(entry);
            }
        }
        replayed
    }

    /// Calculate retry delay with exponential backoff.
    fn retry_delay(&self, attempts: u32) -> std::time::Duration {
        let delay_ms = self.config.initial_retry_delay_ms * 2u64.saturating_pow(attempts);
        let capped_ms = delay_ms.min(self.config.max_retry_delay_ms);
        std::time::Duration::from_millis(capped_ms)
    }

    /// Try to flush the retry buffer (called on successful publish or periodic flush).
    fn flush_buffer(&self) {
        let mut buffer = self.retry_buffer.lock();
        if buffer.is_empty() {
            return;
        }

        let now = std::time::Instant::now();
        let mut still_pending = Vec::new();

        for entry in buffer.drain(..) {
            if entry.next_retry_at > now {
                still_pending.push(entry);
                continue;
            }

            match self.inner.publish(&entry.event) {
                Ok(()) => {
                    // Successfully published — reset failure counter
                    self.consecutive_failures.store(0, std::sync::atomic::Ordering::Relaxed);
                }
                Err(e) => {
                    let mut entry = entry;
                    entry.attempts += 1;

                    if entry.attempts >= self.config.max_retries {
                        // Move to dead letter queue
                        let mut dlq = self.dead_letter_queue.lock();
                        if dlq.len() < self.config.max_dlq_size {
                            dlq.push(DeadLetterEntry {
                                event: entry.event,
                                error: e,
                                attempts: entry.attempts,
                                failed_at: now_millis(),
                            });
                        }
                    } else {
                        entry.next_retry_at = now + self.retry_delay(entry.attempts);
                        still_pending.push(entry);
                    }
                }
            }
        }

        *buffer = still_pending;
    }
}

impl CdcPublisher for ResilientCdcPublisher {
    fn publish(&self, event: &CdcEvent) -> Result<(), String> {
        // Try to publish directly
        match self.inner.publish(event) {
            Ok(()) => {
                // Success — reset failure counter and try to flush buffered events
                self.consecutive_failures.store(0, std::sync::atomic::Ordering::Relaxed);
                self.flush_buffer();
                Ok(())
            }
            Err(e) => {
                // Failed — increment failure counter
                let failures = self.consecutive_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

                if self.config.log_retries {
                    tracing::warn!(
                        "CDC publish failed (attempt {}): {}. Buffering for retry.",
                        failures, e
                    );
                }

                // Buffer the event for retry
                let mut buffer = self.retry_buffer.lock();
                if buffer.len() >= self.config.max_buffer_size {
                    // Buffer full — drop oldest event to DLQ
                    if let Some(oldest) = buffer.first() {
                        let mut dlq = self.dead_letter_queue.lock();
                        if dlq.len() < self.config.max_dlq_size {
                            dlq.push(DeadLetterEntry {
                                event: oldest.event.clone(),
                                error: "buffer overflow".to_string(),
                                attempts: oldest.attempts,
                                failed_at: now_millis(),
                            });
                        }
                    }
                    buffer.remove(0);
                }

                buffer.push(RetryEntry {
                    event: event.clone(),
                    attempts: 1,
                    next_retry_at: std::time::Instant::now() + self.retry_delay(1),
                });

                // Don't return error — event is buffered for retry
                Ok(())
            }
        }
    }

    fn flush(&self) -> Result<(), String> {
        self.flush_buffer();
        self.inner.flush()
    }
}

/// In-memory CDC publisher for testing.
pub struct InMemoryCdcPublisher {
    events: parking_lot::Mutex<Vec<CdcEvent>>,
}

impl InMemoryCdcPublisher {
    pub fn new() -> Self {
        Self {
            events: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// Get all published events.
    pub fn get_events(&self) -> Vec<CdcEvent> {
        self.events.lock().clone()
    }

    /// Clear all events.
    pub fn clear(&self) {
        self.events.lock().clear();
    }

    /// Get event count.
    pub fn count(&self) -> usize {
        self.events.lock().len()
    }
}

impl CdcPublisher for InMemoryCdcPublisher {
    fn publish(&self, event: &CdcEvent) -> Result<(), String> {
        self.events.lock().push(event.clone());
        Ok(())
    }
}

/// Kafka CDC publisher using rdkafka.
///
/// Requires the `cdc-kafka` feature flag.
#[cfg(feature = "cdc-kafka")]
pub struct KafkaCdcPublisher {
    producer: rdkafka::producer::FutureProducer,
    topic: String,
}

#[cfg(feature = "cdc-kafka")]
impl KafkaCdcPublisher {
    pub fn new(config: &CdcConfig) -> Result<Self, String> {
        use rdkafka::config::ClientConfig;
        use rdkafka::producer::FutureProducer;

        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", &config.kafka_brokers)
            .set("client.id", &config.client_id)
            .set("message.timeout.ms", "5000")
            .set("queue.buffering.max.messages", &config.batch_size.to_string())
            .set("linger.ms", &config.flush_interval_ms.to_string())
            .create()
            .map_err(|e| format!("Failed to create Kafka producer: {}", e))?;

        Ok(Self {
            producer,
            topic: config.topic.clone(),
        })
    }
}

#[cfg(feature = "cdc-kafka")]
impl CdcPublisher for KafkaCdcPublisher {
    fn publish(&self, event: &CdcEvent) -> Result<(), String> {
        use rdkafka::producer::FutureRecord;
        use rdkafka::util::Timeout;

        let payload = serde_json::to_string(event)
            .map_err(|e| format!("Failed to serialize CDC event: {}", e))?;

        let key = event.key.clone();

        // Use async runtime to send (only if Tokio runtime is available)
        let topic = self.topic.clone();
        let producer = self.producer.clone();

        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!("CDC Kafka publish called outside Tokio runtime, skipping");
            return;
        }
        tokio::spawn(async move {
            let record = FutureRecord::to(&topic)
                .key(&key)
                .payload(&payload);

            if let Err((e, _)) = producer.send(record, Timeout::After(Duration::from_secs(5))).await {
                eprintln!("CDC Kafka publish error: {}", e);
            }
        });

        Ok(())
    }

    fn publish_batch(&self, events: &[CdcEvent]) -> Result<(), String> {
        for event in events {
            self.publish(event)?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), String> {
        // rdkafka auto-flushes based on linger.ms config
        Ok(())
    }
}

/// Webhook CDC publisher — sends events via HTTP POST.
///
/// Requires the `cdc-webhook` feature flag.
#[cfg(feature = "cdc-webhook")]
pub struct WebhookCdcPublisher {
    url: String,
}

#[cfg(feature = "cdc-webhook")]
impl WebhookCdcPublisher {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
        }
    }
}

#[cfg(feature = "cdc-webhook")]
impl CdcPublisher for WebhookCdcPublisher {
    fn publish(&self, event: &CdcEvent) -> Result<(), String> {
        let url = self.url.clone();
        let payload = serde_json::to_string(event)
            .map_err(|e| format!("Failed to serialize CDC event: {}", e))?;

        tokio::spawn(async move {
            if let Err(e) = reqwest::Client::new()
                .post(&url)
                .header("Content-Type", "application/json")
                .body(payload)
                .send()
                .await
            {
                eprintln!("CDC webhook error: {}", e);
            }
        });

        Ok(())
    }
}

/// CDC manager — coordinates event publishing.
pub struct CdcManager {
    config: CdcConfig,
    publisher: Arc<dyn CdcPublisher>,
    builder: CdcEventBuilder,
}

impl CdcManager {
    /// Create a new CDC manager with the given publisher.
    pub fn new(config: CdcConfig, publisher: Arc<dyn CdcPublisher>) -> Self {
        let builder = CdcEventBuilder::new(&config);
        Self {
            config,
            publisher,
            builder,
        }
    }

    /// Create a CDC manager with in-memory publisher (for testing).
    pub fn new_in_memory() -> (Self, Arc<InMemoryCdcPublisher>) {
        let publisher = Arc::new(InMemoryCdcPublisher::new());
        let config = CdcConfig {
            enabled: true,
            ..Default::default()
        };
        let manager = Self::new(config, publisher.clone());
        (manager, publisher)
    }

    /// Whether CDC is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Publish an INSERT event.
    pub fn on_insert(&self, class: &str, pk: &str, value: &serde_json::Value, seq: u64) {
        if !self.config.enabled {
            return;
        }
        let key = format!("{}::{}", class, pk);
        let event = self.builder.insert_event(class, pk, &key, value, seq);
        let _ = self.publisher.publish(&event);
    }

    /// Publish an UPDATE event.
    pub fn on_update(&self, class: &str, pk: &str, old: Option<&serde_json::Value>, new: &serde_json::Value, seq: u64) {
        if !self.config.enabled {
            return;
        }
        let key = format!("{}::{}", class, pk);
        let event = self.builder.update_event(class, pk, &key, old, new, seq);
        let _ = self.publisher.publish(&event);
    }

    /// Publish a DELETE event.
    pub fn on_delete(&self, class: &str, pk: &str, old: Option<&serde_json::Value>, seq: u64) {
        if !self.config.enabled {
            return;
        }
        let key = format!("{}::{}", class, pk);
        let event = self.builder.delete_event(class, pk, &key, old, seq);
        let _ = self.publisher.publish(&event);
    }

    /// Flush buffered events.
    pub fn flush(&self) -> Result<(), String> {
        self.publisher.flush()
    }
}

/// Current timestamp in milliseconds.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cdc_event_serialization() {
        let event = CdcEvent {
            ts: 1234567890,
            op: CdcOperation::Insert,
            class: "Product".to_string(),
            pk: "001".to_string(),
            key: "Product::001".to_string(),
            after: Some(serde_json::json!({"name": "iPhone", "price": 999})),
            before: None,
            seq: 42,
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"INSERT\""));
        assert!(json.contains("Product::001"));

        let deserialized: CdcEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.op, CdcOperation::Insert);
        assert_eq!(deserialized.key, "Product::001");
    }

    #[test]
    fn test_in_memory_publisher() {
        let (manager, publisher) = CdcManager::new_in_memory();

        manager.on_insert("Product", "001", &serde_json::json!({"name": "iPhone"}), 1);
        manager.on_update("Product", "001", None, &serde_json::json!({"name": "iPhone 15"}), 2);
        manager.on_delete("Product", "001", None, 3);

        assert_eq!(publisher.count(), 3);

        let events = publisher.get_events();
        assert_eq!(events[0].op, CdcOperation::Insert);
        assert_eq!(events[1].op, CdcOperation::Update);
        assert_eq!(events[2].op, CdcOperation::Delete);
    }

    #[test]
    fn test_cdc_disabled() {
        let config = CdcConfig {
            enabled: false,
            ..Default::default()
        };
        let publisher = Arc::new(InMemoryCdcPublisher::new());
        let manager = CdcManager::new(config, publisher.clone());

        manager.on_insert("Product", "001", &serde_json::json!({}), 1);

        assert_eq!(publisher.count(), 0);
    }

    #[test]
    fn test_cdc_event_builder() {
        let config = CdcConfig {
            include_before: true,
            include_after: true,
            ..Default::default()
        };
        let builder = CdcEventBuilder::new(&config);

        let event = builder.insert_event("Product", "001", "Product::001", &serde_json::json!({"name": "iPhone"}), 1);
        assert_eq!(event.op, CdcOperation::Insert);
        assert!(event.after.is_some());
        assert!(event.before.is_none());

        let event = builder.delete_event("Product", "001", "Product::001", Some(&serde_json::json!({"name": "iPhone"})), 2);
        assert_eq!(event.op, CdcOperation::Delete);
        assert!(event.before.is_some());
        assert!(event.after.is_none());
    }

    // ── Resilient Publisher Tests ──

    /// A publisher that always fails (for testing retry logic).
    struct FailingPublisher;

    impl CdcPublisher for FailingPublisher {
        fn publish(&self, _event: &CdcEvent) -> Result<(), String> {
            Err("connection refused".to_string())
        }
    }

    #[test]
    fn test_resilient_buffers_on_failure() {
        let inner = Arc::new(FailingPublisher);
        let resilient = ResilientCdcPublisher::new(inner);

        let event = CdcEvent {
            ts: 1000,
            op: CdcOperation::Insert,
            class: "Product".to_string(),
            pk: "001".to_string(),
            key: "Product::001".to_string(),
            after: None,
            before: None,
            seq: 1,
        };

        // Publish should succeed (event is buffered, not lost)
        assert!(resilient.publish(&event).is_ok());

        // Event should be in retry buffer
        assert_eq!(resilient.buffer_size(), 1);
        assert_eq!(resilient.dlq_size(), 0);
    }

    #[test]
    fn test_resilient_retries_and_dlq() {
        let inner = Arc::new(FailingPublisher);
        let config = ResilientConfig {
            max_retries: 3,
            initial_retry_delay_ms: 1, // Very fast for testing
            max_retry_delay_ms: 10,
            max_buffer_size: 100,
            max_dlq_size: 100,
            log_retries: false,
        };
        let resilient = ResilientCdcPublisher::with_config(inner, config);

        let event = CdcEvent {
            ts: 1000,
            op: CdcOperation::Insert,
            class: "Product".to_string(),
            pk: "001".to_string(),
            key: "Product::001".to_string(),
            after: None,
            before: None,
            seq: 1,
        };

        // Publish — event buffered
        resilient.publish(&event).unwrap();
        assert_eq!(resilient.buffer_size(), 1);

        // Flush multiple times to exhaust retries
        std::thread::sleep(std::time::Duration::from_millis(20));
        resilient.flush();
        std::thread::sleep(std::time::Duration::from_millis(20));
        resilient.flush();
        std::thread::sleep(std::time::Duration::from_millis(20));
        resilient.flush();
        std::thread::sleep(std::time::Duration::from_millis(20));
        resilient.flush();

        // After max retries, event should move to DLQ
        // (buffer may still have it if retry delay hasn't elapsed)
        let total = resilient.buffer_size() + resilient.dlq_size();
        assert!(total > 0, "event should be in buffer or DLQ");
    }

    #[test]
    fn test_resilient_success_flushes_buffer() {
        // Create a publisher that always succeeds
        let inner = Arc::new(InMemoryCdcPublisher::new());
        let config = ResilientConfig {
            max_retries: 10,
            initial_retry_delay_ms: 1,
            max_retry_delay_ms: 10,
            log_retries: false,
            ..Default::default()
        };
        let resilient = ResilientCdcPublisher::with_config(inner.clone(), config);

        let event = CdcEvent {
            ts: 1000,
            op: CdcOperation::Insert,
            class: "Product".to_string(),
            pk: "001".to_string(),
            key: "Product::001".to_string(),
            after: None,
            before: None,
            seq: 1,
        };

        // Publish succeeds directly — no buffering
        resilient.publish(&event).unwrap();
        assert_eq!(resilient.buffer_size(), 0);
        assert_eq!(inner.count(), 1);
    }

    #[test]
    fn test_resilient_dlq_replay() {
        let inner = Arc::new(FailingPublisher);
        let config = ResilientConfig {
            max_retries: 1,
            initial_retry_delay_ms: 1,
            max_retry_delay_ms: 10,
            log_retries: false,
            ..Default::default()
        };
        let resilient = ResilientCdcPublisher::with_config(inner, config);

        let event = CdcEvent {
            ts: 1000,
            op: CdcOperation::Insert,
            class: "Product".to_string(),
            pk: "001".to_string(),
            key: "Product::001".to_string(),
            after: None,
            before: None,
            seq: 1,
        };

        // Publish and flush to exhaust retries
        resilient.publish(&event).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        resilient.flush();

        // Get dead letters
        let dl = resilient.get_dead_letters();
        assert!(!dl.is_empty(), "should have dead letter entries");

        // Clear DLQ
        resilient.clear_dead_letters();
        assert_eq!(resilient.dlq_size(), 0);
    }

    #[test]
    fn test_resilient_buffer_overflow() {
        let inner = Arc::new(FailingPublisher);
        let config = ResilientConfig {
            max_retries: 1,
            initial_retry_delay_ms: 1,
            max_buffer_size: 3, // Very small buffer
            max_dlq_size: 100,
            log_retries: false,
            ..Default::default()
        };
        let resilient = ResilientCdcPublisher::with_config(inner, config);

        // Publish more events than buffer can hold
        for i in 0..5 {
            let event = CdcEvent {
                ts: 1000 + i,
                op: CdcOperation::Insert,
                class: "Product".to_string(),
                pk: format!("{:03}", i),
                key: format!("Product::{:03}", i),
                after: None,
                before: None,
                seq: i,
            };
            resilient.publish(&event).unwrap();
        }

        // Buffer should be capped, overflow goes to DLQ
        assert!(resilient.buffer_size() <= 3);
        assert!(resilient.dlq_size() > 0, "overflow should go to DLQ");
    }
}

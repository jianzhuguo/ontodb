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

        // Use async runtime to send
        let topic = self.topic.clone();
        let producer = self.producer.clone();

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
}

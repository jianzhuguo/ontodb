//! Value metadata for living data mechanism.
//!
//! Stores per-entity value scores in independent LSM keys (`__val_meta__::{class}::{pk}`).
//! Scores are computed at write time and decay is calculated at read time.
//!
//! ## Lambda (λ) unit convention
//!
//! Lambda is the **per-second** decay rate. The decay formula is:
//!
//!   `current_score = value_score × e^(-λ × elapsed_seconds)`
//!
//! Common presets (use `LAMBDA_*` constants):
//!
//! | Preset | Half-life | λ (per second) |
//! |--------|-----------|----------------|
//! | `LAMBDA_7H`   | 7 hours  | 2.75e-5 |
//! | `LAMBDA_70D`  | 70 days  | 1.15e-7 |
//! | `LAMBDA_2Y`   | 2 years  | 1.10e-8 |

use serde::{Deserialize, Serialize};

/// 7-hour half-life: λ = ln(2) / (7 × 3600)
pub const LAMBDA_7H: f64 = 0.693 / (7.0 * 3600.0);
/// 70-day half-life: λ = ln(2) / (70 × 86400)
pub const LAMBDA_70D: f64 = 0.693 / (70.0 * 86400.0);
/// 2-year half-life: λ = ln(2) / (2 × 365 × 86400)
pub const LAMBDA_2Y: f64 = 0.693 / (2.0 * 365.0 * 86400.0);

/// Value metadata stored per entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueMetadata {
    /// Initial score assessed at write time (0.0 ~ 1.0, never changes).
    pub base_score: f64,
    /// Current score including activation boosts (0.0 ~ 1.0, can be increased by activation).
    pub value_score: f64,
    /// Decay rate (λ) in per-second units.
    /// Use `LAMBDA_7H`, `LAMBDA_70D`, `LAMBDA_2Y` presets.
    pub lambda: f64,
    /// Timestamp of last activation/reset (seconds since epoch).
    pub last_activated_at: u64,
    /// Original write timestamp (seconds since epoch, never modified).
    pub created_at: u64,
    /// Number of times this entity has been activated.
    pub activation_count: u32,
}

impl ValueMetadata {
    /// Creates new metadata with the given base score and lambda.
    pub fn new(base_score: f64, lambda: f64) -> Self {
        let now = now_secs();
        Self {
            base_score,
            value_score: base_score,
            lambda,
            last_activated_at: now,
            created_at: now,
            activation_count: 0,
        }
    }

    /// Computes the current score with real-time decay.
    /// `current = value_score × e^(-λ × Δt)`
    /// This is computed at read time, never persisted.
    pub fn current_score(&self) -> f64 {
        let now = now_secs();
        let elapsed = now.saturating_sub(self.last_activated_at) as f64;
        let decayed = self.value_score * (-self.lambda * elapsed).exp();
        decayed.clamp(0.0, 1.0)
    }

    /// Activate: boost from current decayed score and reset decay clock.
    ///
    /// Uses `current_score() + delta` as the new base, so activation is
    /// relative to the data's current temperature, not its original score.
    /// Example: cold data (0.2) + activate(0.5) → 0.7, not 1.0.
    pub fn activate(&mut self, delta: f64) {
        let current = self.current_score();
        self.value_score = (current + delta).min(1.0);
        self.last_activated_at = now_secs();
        self.activation_count += 1;
    }

    /// Returns the LSM key for this metadata.
    pub fn meta_key(class: &str, pk: &str) -> Vec<u8> {
        format!("__val_meta__::{}::{}", class, pk).into_bytes()
    }

    /// Parses class and pk from a meta key.
    pub fn parse_meta_key(key: &[u8]) -> Option<(String, String)> {
        let s = std::str::from_utf8(key).ok()?;
        let rest = s.strip_prefix("__val_meta__::")?;
        let (class, pk) = rest.split_once("::")?;
        Some((class.to_string(), pk.to_string()))
    }

    /// Serializes to bytes for LSM storage.
    pub fn to_bytes(&self) -> Vec<u8> {
        // Use compact JSON for now; can switch to BinaryRow later for efficiency
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserializes from LSM bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// Simple rule-based value scorer.
pub struct ValueScorer;

impl ValueScorer {
    /// Assess a document's value score using simple rules.
    /// Returns a score in [0.0, 1.0].
    pub fn assess(doc: &serde_json::Map<String, serde_json::Value>) -> f64 {
        let mut score = 0.5;

        // Rule 1: has text content → +0.2
        let has_text = doc.values().any(|v| {
            v.as_str().is_some_and(|s| s.len() > 20)
        });
        if has_text { score += 0.2; }

        // Rule 2: field completeness → +0.1
        let total_fields = doc.len();
        let non_null_fields = doc.values().filter(|v| !v.is_null()).count();
        if total_fields > 0 {
            let completeness = non_null_fields as f64 / total_fields as f64;
            score += completeness * 0.1;
        }

        // Rule 3: reasonable size → +0.1
        let size = serde_json::to_string(doc).map(|s| s.len()).unwrap_or(0);
        if size > 50 && size < 50000 {
            score += 0.1;
        }

        // Rule 4: has __class__ (structured data) → +0.1
        if doc.contains_key("__class__") {
            score += 0.1;
        }

        score.clamp(0.0, 1.0)
    }
}

/// Current timestamp in seconds since epoch.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_decay_formula() {
        // Use LAMBDA_7H: 7-hour half-life
        let mut meta = ValueMetadata::new(1.0, LAMBDA_7H);
        // Force last_activated_at to 7 hours ago (25200 seconds)
        meta.last_activated_at = now_secs() - 25200;
        let score = meta.current_score();
        // After one half-life, score should be ~0.5
        assert!((score - 0.5).abs() < 0.01, "expected ~0.5, got {}", score);
    }

    #[test]
    fn test_no_decay_at_zero() {
        let meta = ValueMetadata::new(0.8, LAMBDA_70D);
        // last_activated_at = now, so elapsed ≈ 0
        let score = meta.current_score();
        assert!((score - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_activate_resets_decay() {
        let mut meta = ValueMetadata::new(0.5, LAMBDA_7H);
        // Simulate 14 hours of decay (2 half-lives)
        meta.last_activated_at = now_secs() - (14 * 3600);
        let before = meta.current_score();
        // After 2 half-lives: 0.5 × 0.25 = 0.125
        assert!(before < 0.2, "expected < 0.2, got {}", before);

        meta.activate(0.3); // boost from current decayed score
        let after = meta.current_score();
        // New behavior: value_score = current_score() + delta = ~0.125 + 0.3 = ~0.425
        assert!(after > before, "after should be greater than before");
        assert!(after > 0.3, "expected > 0.3, got {}", after);
        assert!(after < 0.5, "expected < 0.5 (not jumping to raw value_score), got {}", after);
        assert_eq!(meta.activation_count, 1);
    }

    #[test]
    fn test_activate_cap_at_1() {
        let mut meta = ValueMetadata::new(0.9, LAMBDA_7H);
        meta.activate(0.5);
        assert!(meta.value_score <= 1.0);
    }

    #[test]
    fn test_2year_half_life() {
        // 2-year half-life preset
        let mut meta = ValueMetadata::new(1.0, LAMBDA_2Y);
        // After 2 years, score should be ~0.5
        let two_years_secs = 2 * 365 * 86400;
        meta.last_activated_at = now_secs() - two_years_secs;
        let score = meta.current_score();
        assert!((score - 0.5).abs() < 0.05, "expected ~0.5 after 2 years, got {}", score);
    }

    #[test]
    fn test_70day_half_life() {
        let mut meta = ValueMetadata::new(1.0, LAMBDA_70D);
        meta.last_activated_at = now_secs() - (70 * 86400);
        let score = meta.current_score();
        assert!((score - 0.5).abs() < 0.05, "expected ~0.5 after 70 days, got {}", score);
    }

    #[test]
    fn test_7hour_half_life() {
        let mut meta = ValueMetadata::new(1.0, LAMBDA_7H);
        meta.last_activated_at = now_secs() - (7 * 3600);
        let score = meta.current_score();
        assert!((score - 0.5).abs() < 0.05, "expected ~0.5 after 7 hours, got {}", score);
    }

    #[test]
    fn test_multiple_half_lives() {
        let mut meta = ValueMetadata::new(1.0, LAMBDA_7H);
        // 3 half-lives = 21 hours → score should be ~0.125
        meta.last_activated_at = now_secs() - (21 * 3600);
        let score = meta.current_score();
        assert!((score - 0.125).abs() < 0.02, "expected ~0.125 after 3 half-lives, got {}", score);
    }

    #[test]
    fn test_meta_key_roundtrip() {
        let key = ValueMetadata::meta_key("BioTask", "001");
        assert_eq!(key, b"__val_meta__::BioTask::001");
        
        let (class, pk) = ValueMetadata::parse_meta_key(&key).unwrap();
        assert_eq!(class, "BioTask");
        assert_eq!(pk, "001");
    }

    #[test]
    fn test_scorer_basic() {
        let doc = json!({
            "name": "基因组测序任务",
            "status": "active",
            "description": "这是一个关于人类基因组测序的实验任务",
            "__class__": "BioTask"
        }).as_object().unwrap().clone();
        
        let score = ValueScorer::assess(&doc);
        assert!(score >= 0.8, "expected >= 0.8, got {}", score);
    }

    #[test]
    fn test_scorer_minimal() {
        let doc = json!({}).as_object().unwrap().clone();
        let score = ValueScorer::assess(&doc);
        assert!((score - 0.5).abs() < 0.01, "expected 0.5, got {}", score);
    }
}

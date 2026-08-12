//! STTRL (Spatio-Temporal Rule Language) engine for OntoDB.
//!
//! Implements spatio-temporal rules that combine spatial and temporal conditions.
//! These rules enable complex event processing over location and time data.
//!
//! Rule categories:
//! 1. Geofence rules: enter/exit/dwell in a region
//! 2. Proximity rules: entities within distance
//! 3. Motion rules: speed, direction, acceleration
//! 4. Pattern rules: recurring visits, routines
//! 5. Anomaly rules: unusual locations, timing
//!
//! Usage:
//! ```ignore
//! use onto_core::sttrl::*;
//!
//! let rule = Rule::new("geofence_enter", RuleType::GeofenceEnter {
//!     region: Region::Circle { center_lon: 116.4, center_lat: 39.9, radius_m: 1000.0 },
//! });
//!
//! let event = SpatioTemporalEvent {
//!     entity_id: "vehicle_001".to_string(),
//!     lon: 116.401,
//!     lat: 39.901,
//!     timestamp: 1000000,
//!     properties: HashMap::new(),
//! };
//!
//! let result = rule.evaluate(&event, &[]);
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Types ──

/// A spatio-temporal event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpatioTemporalEvent {
    /// Entity ID (e.g., "vehicle_001", "sensor_01").
    pub entity_id: String,
    /// Longitude.
    pub lon: f64,
    /// Latitude.
    pub lat: f64,
    /// Timestamp (nanoseconds since Unix epoch).
    pub timestamp: i64,
    /// Additional properties.
    pub properties: HashMap<String, String>,
}

/// A spatial region.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Region {
    /// Circle with center and radius.
    Circle {
        center_lon: f64,
        center_lat: f64,
        radius_m: f64,
    },
    /// Rectangle with bounds.
    Rectangle {
        min_lon: f64,
        min_lat: f64,
        max_lon: f64,
        max_lat: f64,
    },
    /// Polygon with vertices.
    Polygon(Vec<(f64, f64)>),
}

impl Region {
    /// Check if a point is inside this region.
    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        match self {
            Region::Circle { center_lon, center_lat, radius_m } => {
                haversine_distance(*center_lon, *center_lat, lon, lat) <= *radius_m
            }
            Region::Rectangle { min_lon, min_lat, max_lon, max_lat } => {
                lon >= *min_lon && lon <= *max_lon && lat >= *min_lat && lat <= *max_lat
            }
            Region::Polygon(vertices) => point_in_polygon(lon, lat, vertices),
        }
    }
}

/// Direction of movement.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Direction {
    North,
    South,
    East,
    West,
    Northeast,
    Northwest,
    Southeast,
    Southwest,
}

/// Rule type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuleType {
    /// Entity enters a region.
    GeofenceEnter { region: Region },
    /// Entity exits a region.
    GeofenceExit { region: Region },
    /// Entity stays in a region for a duration.
    GeofenceDwell { region: Region, min_duration_secs: u64 },
    /// Two entities are within distance.
    Proximity { max_distance_m: f64, other_entity: String },
    /// Entity speed exceeds threshold.
    SpeedLimit { max_speed_mps: f64 },
    /// Entity moves in a specific direction.
    DirectionMove { direction: Direction, region: Region },
    /// Entity visits a location repeatedly.
    RecurringVisit { region: Region, min_visits: usize, period_secs: u64 },
    /// Entity is at an unusual location.
    UnusualLocation { known_locations: Vec<Region>, tolerance_m: f64 },
    /// Entity stops moving for a duration.
    StopDetection { min_duration_secs: u64, max_distance_m: f64 },
    /// Entity enters a region at an unusual time.
    TimeAnomaly { region: Region, expected_hours: Vec<u8> },
}

/// A spatio-temporal rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Rule name/ID.
    pub name: String,
    /// Rule type.
    pub rule_type: RuleType,
    /// Whether the rule is enabled.
    pub enabled: bool,
    /// Rule description.
    pub description: String,
}

impl Rule {
    /// Create a new rule.
    pub fn new(name: impl Into<String>, rule_type: RuleType) -> Self {
        Self {
            name: name.into(),
            rule_type,
            enabled: true,
            description: String::new(),
        }
    }

    /// Set description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Evaluate this rule against an event and history.
    pub fn evaluate(&self, event: &SpatioTemporalEvent, history: &[SpatioTemporalEvent]) -> RuleResult {
        if !self.enabled {
            return RuleResult::NotTriggered;
        }

        match &self.rule_type {
            RuleType::GeofenceEnter { region } => {
                if region.contains(event.lon, event.lat) {
                    RuleResult::Triggered {
                        rule_name: self.name.clone(),
                        entity_id: event.entity_id.clone(),
                        message: format!("Entity {} entered region", event.entity_id),
                        severity: RuleSeverity::Info,
                    }
                } else {
                    RuleResult::NotTriggered
                }
            }
            RuleType::GeofenceExit { region } => {
                // Check if entity was previously inside
                let was_inside = history.iter().any(|e| {
                    e.entity_id == event.entity_id && region.contains(e.lon, e.lat)
                });
                let is_outside = !region.contains(event.lon, event.lat);

                if was_inside && is_outside {
                    RuleResult::Triggered {
                        rule_name: self.name.clone(),
                        entity_id: event.entity_id.clone(),
                        message: format!("Entity {} exited region", event.entity_id),
                        severity: RuleSeverity::Info,
                    }
                } else {
                    RuleResult::NotTriggered
                }
            }
            RuleType::GeofenceDwell { region, min_duration_secs } => {
                if !region.contains(event.lon, event.lat) {
                    return RuleResult::NotTriggered;
                }

                // Find when entity entered the region
                let entry_time = history.iter()
                    .rev()
                    .find(|e| e.entity_id == event.entity_id && region.contains(e.lon, e.lat))
                    .map(|e| e.timestamp);

                if let Some(entry_ts) = entry_time {
                    let dwell_ns = event.timestamp.saturating_sub(entry_ts);
                    let dwell_secs = dwell_ns / 1_000_000_000;
                    if dwell_secs >= *min_duration_secs as i64 {
                        return RuleResult::Triggered {
                            rule_name: self.name.clone(),
                            entity_id: event.entity_id.clone(),
                            message: format!(
                                "Entity {} dwelled in region for {} seconds",
                                event.entity_id, dwell_secs
                            ),
                            severity: RuleSeverity::Warning,
                        };
                    }
                }

                RuleResult::NotTriggered
            }
            RuleType::Proximity { max_distance_m, other_entity } => {
                let is_nearby = history.iter().any(|e| {
                    e.entity_id == *other_entity
                        && haversine_distance(event.lon, event.lat, e.lon, e.lat) <= *max_distance_m
                });

                if is_nearby {
                    RuleResult::Triggered {
                        rule_name: self.name.clone(),
                        entity_id: event.entity_id.clone(),
                        message: format!(
                            "Entity {} is within {}m of {}",
                            event.entity_id, max_distance_m, other_entity
                        ),
                        severity: RuleSeverity::Info,
                    }
                } else {
                    RuleResult::NotTriggered
                }
            }
            RuleType::SpeedLimit { max_speed_mps } => {
                // Calculate speed from previous position
                let prev = history.iter()
                    .rev()
                    .find(|e| e.entity_id == event.entity_id);

                if let Some(prev_event) = prev {
                    let distance = haversine_distance(
                        prev_event.lon, prev_event.lat,
                        event.lon, event.lat,
                    );
                    let time_diff = event.timestamp.saturating_sub(prev_event.timestamp);
                    let time_secs = time_diff as f64 / 1_000_000_000.0;

                    if time_secs > 0.0 {
                        let speed = distance / time_secs;
                        if speed > *max_speed_mps {
                            return RuleResult::Triggered {
                                rule_name: self.name.clone(),
                                entity_id: event.entity_id.clone(),
                                message: format!(
                                    "Entity {} speed {:.1} m/s exceeds limit {:.1} m/s",
                                    event.entity_id, speed, max_speed_mps
                                ),
                                severity: RuleSeverity::Warning,
                            };
                        }
                    }
                }

                RuleResult::NotTriggered
            }
            RuleType::RecurringVisit { region, min_visits, period_secs } => {
                // Count visits within the period
                let period_ns = *period_secs as i64 * 1_000_000_000;
                let cutoff = event.timestamp.saturating_sub(period_ns);

                let visits = history.iter()
                    .filter(|e| {
                        e.entity_id == event.entity_id
                            && e.timestamp >= cutoff
                            && region.contains(e.lon, e.lat)
                    })
                    .count();

                if visits >= *min_visits {
                    RuleResult::Triggered {
                        rule_name: self.name.clone(),
                        entity_id: event.entity_id.clone(),
                        message: format!(
                            "Entity {} visited region {} times in {} seconds",
                            event.entity_id, visits, period_secs
                        ),
                        severity: RuleSeverity::Info,
                    }
                } else {
                    RuleResult::NotTriggered
                }
            }
            RuleType::UnusualLocation { known_locations, tolerance_m: _ } => {
                let is_known = known_locations.iter().any(|r| r.contains(event.lon, event.lat));

                if !is_known {
                    RuleResult::Triggered {
                        rule_name: self.name.clone(),
                        entity_id: event.entity_id.clone(),
                        message: format!(
                            "Entity {} at unusual location ({}, {})",
                            event.entity_id, event.lon, event.lat
                        ),
                        severity: RuleSeverity::Warning,
                    }
                } else {
                    RuleResult::NotTriggered
                }
            }
            RuleType::StopDetection { min_duration_secs, max_distance_m } => {
                // Check if entity has been stationary
                let prev = history.iter()
                    .rev()
                    .find(|e| e.entity_id == event.entity_id);

                if let Some(prev_event) = prev {
                    let distance = haversine_distance(
                        prev_event.lon, prev_event.lat,
                        event.lon, event.lat,
                    );
                    let time_diff = event.timestamp.saturating_sub(prev_event.timestamp);
                    let time_secs = time_diff / 1_000_000_000;

                    if distance <= *max_distance_m && time_secs >= *min_duration_secs as i64 {
                        return RuleResult::Triggered {
                            rule_name: self.name.clone(),
                            entity_id: event.entity_id.clone(),
                            message: format!(
                                "Entity {} stopped for {} seconds",
                                event.entity_id, time_secs
                            ),
                            severity: RuleSeverity::Info,
                        };
                    }
                }

                RuleResult::NotTriggered
            }
            RuleType::TimeAnomaly { region, expected_hours } => {
                if !region.contains(event.lon, event.lat) {
                    return RuleResult::NotTriggered;
                }

                // Check if current hour is in expected hours
                let hour = ((event.timestamp / 3_600_000_000_000) % 24) as u8;
                if !expected_hours.contains(&hour) {
                    return RuleResult::Triggered {
                        rule_name: self.name.clone(),
                        entity_id: event.entity_id.clone(),
                        message: format!(
                            "Entity {} in region at unexpected hour {}",
                            event.entity_id, hour
                        ),
                        severity: RuleSeverity::Warning,
                    };
                }

                RuleResult::NotTriggered
            }
            _ => RuleResult::NotTriggered,
        }
    }
}

/// Rule evaluation result.
#[derive(Debug, Clone)]
pub enum RuleResult {
    /// Rule was triggered.
    Triggered {
        rule_name: String,
        entity_id: String,
        message: String,
        severity: RuleSeverity,
    },
    /// Rule was not triggered.
    NotTriggered,
}

/// Rule severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleSeverity {
    Info,
    Warning,
    Critical,
}

/// Rule engine that evaluates multiple rules.
pub struct RuleEngine {
    rules: Vec<Rule>,
    /// History of events for context-dependent rules.
    history: Vec<SpatioTemporalEvent>,
    /// Maximum history size.
    max_history: usize,
}

impl RuleEngine {
    /// Create a new rule engine.
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            history: Vec::new(),
            max_history: 10000,
        }
    }

    /// Add a rule.
    pub fn add_rule(&mut self, rule: Rule) {
        self.rules.push(rule);
    }

    /// Evaluate all rules against an event.
    pub fn evaluate(&mut self, event: &SpatioTemporalEvent) -> Vec<RuleResult> {
        let results: Vec<RuleResult> = self.rules.iter()
            .filter_map(|rule| {
                let result = rule.evaluate(event, &self.history);
                match result {
                    RuleResult::NotTriggered => None,
                    triggered => Some(triggered),
                }
            })
            .collect();

        // Add event to history
        self.history.push(event.clone());
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }

        results
    }

    /// Get the number of rules.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

// ── Helpers ──

fn haversine_distance(lon1: f64, lat1: f64, lon2: f64, lat2: f64) -> f64 {
    const R: f64 = 6_371_000.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    R * c
}

fn point_in_polygon(lon: f64, lat: f64, polygon: &[(f64, f64)]) -> bool {
    let n = polygon.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = polygon[i];
        let (xj, yj) = polygon[j];
        if ((yi > lat) != (yj > lat))
            && (lon < (xj - xi) * (lat - yi) / (yj - yi) + xi)
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(id: &str, lon: f64, lat: f64, ts: i64) -> SpatioTemporalEvent {
        SpatioTemporalEvent {
            entity_id: id.to_string(),
            lon,
            lat,
            timestamp: ts,
            properties: HashMap::new(),
        }
    }

    #[test]
    fn test_geofence_enter() {
        let rule = Rule::new("enter_zone", RuleType::GeofenceEnter {
            region: Region::Circle {
                center_lon: 116.4,
                center_lat: 39.9,
                radius_m: 1000.0,
            },
        });

        let inside = make_event("v1", 116.401, 39.901, 1000);
        let outside = make_event("v1", 117.0, 40.0, 1000);

        assert!(matches!(rule.evaluate(&inside, &[]), RuleResult::Triggered { .. }));
        assert!(matches!(rule.evaluate(&outside, &[]), RuleResult::NotTriggered));
    }

    #[test]
    fn test_geofence_exit() {
        let region = Region::Circle {
            center_lon: 116.4,
            center_lat: 39.9,
            radius_m: 1000.0,
        };
        let rule = Rule::new("exit_zone", RuleType::GeofenceExit { region });

        let history = vec![make_event("v1", 116.401, 39.901, 100)];
        let exit_event = make_event("v1", 117.0, 40.0, 200);

        assert!(matches!(rule.evaluate(&exit_event, &history), RuleResult::Triggered { .. }));
    }

    #[test]
    fn test_speed_limit() {
        let rule = Rule::new("speed_check", RuleType::SpeedLimit { max_speed_mps: 30.0 });

        let history = vec![make_event("v1", 116.4, 39.9, 1000_000_000_000)];
        // Move ~100m in 1 second = 100 m/s (way over limit)
        let fast_event = make_event("v1", 116.401, 39.9, 1001_000_000_000);

        assert!(matches!(rule.evaluate(&fast_event, &history), RuleResult::Triggered { .. }));
    }

    #[test]
    fn test_proximity() {
        let rule = Rule::new("nearby", RuleType::Proximity {
            max_distance_m: 100.0,
            other_entity: "v2".to_string(),
        });

        let history = vec![make_event("v2", 116.4, 39.9, 100)];
        let event = make_event("v1", 116.4001, 39.9001, 200);

        assert!(matches!(rule.evaluate(&event, &history), RuleResult::Triggered { .. }));
    }

    #[test]
    fn test_unusual_location() {
        let rule = Rule::new("unusual", RuleType::UnusualLocation {
            known_locations: vec![Region::Circle {
                center_lon: 116.4,
                center_lat: 39.9,
                radius_m: 1000.0,
            }],
            tolerance_m: 100.0,
        });

        let normal = make_event("v1", 116.401, 39.901, 100);
        let unusual = make_event("v1", 121.5, 31.2, 200);

        assert!(matches!(rule.evaluate(&normal, &[]), RuleResult::NotTriggered));
        assert!(matches!(rule.evaluate(&unusual, &[]), RuleResult::Triggered { .. }));
    }

    #[test]
    fn test_rule_engine() {
        let mut engine = RuleEngine::new();
        engine.add_rule(Rule::new("enter", RuleType::GeofenceEnter {
            region: Region::Circle {
                center_lon: 116.4,
                center_lat: 39.9,
                radius_m: 1000.0,
            },
        }));
        engine.add_rule(Rule::new("speed", RuleType::SpeedLimit { max_speed_mps: 30.0 }));

        let event = make_event("v1", 116.401, 39.901, 1000);
        let results = engine.evaluate(&event);
        assert_eq!(results.len(), 1); // Only geofence triggers
    }
}

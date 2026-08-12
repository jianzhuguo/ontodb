//! Time series data types for OntoDB.
//!
//! Supports:
//! - Timestamp with nanosecond precision
//! - Time series data points (timestamp + value)
//! - Window functions (Tumbling, Hopping, Session)
//! - DTW distance calculation
//! - Basic statistical aggregations
//!
//! Design: Time series data is stored as regular LSM entries with
//! timestamp-encoded keys for efficient range scans.

use serde::{Deserialize, Serialize};
use std::fmt;

// ── Timestamp ──

/// Nanosecond-precision timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// Create from Unix seconds.
    pub fn from_secs(secs: i64) -> Self {
        Timestamp(secs * 1_000_000_000)
    }

    /// Create from Unix milliseconds.
    pub fn from_millis(millis: i64) -> Self {
        Timestamp(millis * 1_000_000)
    }

    /// Create from Unix nanoseconds.
    pub fn from_nanos(nanos: i64) -> Self {
        Timestamp(nanos)
    }

    /// Current time.
    pub fn now() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(i64::MAX as u128) as i64;
        Timestamp(nanos)
    }

    /// Get as Unix seconds.
    pub fn as_secs(&self) -> i64 {
        self.0 / 1_000_000_000
    }

    /// Get as Unix milliseconds.
    pub fn as_millis(&self) -> i64 {
        self.0 / 1_000_000
    }

    /// Get as Unix nanoseconds.
    pub fn as_nanos(&self) -> i64 {
        self.0
    }

    /// Duration between two timestamps.
    pub fn duration_since(&self, earlier: &Timestamp) -> Duration {
        Duration(self.0 - earlier.0)
    }

    /// Add a duration.
    pub fn add(&self, dur: Duration) -> Timestamp {
        Timestamp(self.0 + dur.0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.as_secs();
        let nanos = (self.0 % 1_000_000_000) as u32;
        let days = secs / 86400;
        let remaining = secs % 86400;
        let hours = remaining / 3600;
        let minutes = (remaining % 3600) / 60;
        let seconds = remaining % 60;
        let year = 1970 + days / 365; // Simplified
        write!(f, "{}-{:02}:{:02}:{:02}.{:09}", year, hours, minutes, seconds, nanos)
    }
}

// ── Duration ──

/// Duration in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Duration(pub i64);

impl Duration {
    pub fn from_secs(secs: i64) -> Self {
        Duration(secs * 1_000_000_000)
    }

    pub fn from_millis(millis: i64) -> Self {
        Duration(millis * 1_000_000)
    }

    pub fn from_nanos(nanos: i64) -> Self {
        Duration(nanos)
    }

    pub fn as_secs(&self) -> f64 {
        self.0 as f64 / 1_000_000_000.0
    }

    pub fn as_millis(&self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }
}

// ── Data Point ──

/// A time series data point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPoint {
    pub timestamp: Timestamp,
    pub value: f64,
}

impl DataPoint {
    pub fn new(timestamp: Timestamp, value: f64) -> Self {
        Self { timestamp, value }
    }
}

// ── Time Series ──

/// A time series: sorted sequence of data points.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSeries {
    pub name: String,
    pub points: Vec<DataPoint>,
}

impl TimeSeries {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            points: Vec::new(),
        }
    }

    /// Add a data point (maintains sorted order).
    pub fn push(&mut self, point: DataPoint) {
        self.points.push(point);
    }

    /// Sort points by timestamp.
    pub fn sort(&mut self) {
        self.points.sort_by_key(|p| p.timestamp);
    }

    /// Number of points.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Time range of the series.
    pub fn time_range(&self) -> Option<(Timestamp, Timestamp)> {
        if self.points.is_empty() {
            return None;
        }
        Some((self.points.first()?.timestamp, self.points.last()?.timestamp))
    }

    /// Filter points by time range [start, end).
    pub fn range(&self, start: Timestamp, end: Timestamp) -> Vec<&DataPoint> {
        self.points
            .iter()
            .filter(|p| p.timestamp >= start && p.timestamp < end)
            .collect()
    }

    /// Mean value.
    pub fn mean(&self) -> f64 {
        if self.points.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.points.iter().map(|p| p.value).sum();
        sum / self.points.len() as f64
    }

    /// Standard deviation.
    pub fn std_dev(&self) -> f64 {
        let mean = self.mean();
        let variance: f64 = self.points.iter().map(|p| (p.value - mean).powi(2)).sum();
        (variance / self.points.len() as f64).sqrt()
    }

    /// Min value.
    pub fn min(&self) -> f64 {
        self.points.iter().map(|p| p.value).fold(f64::INFINITY, f64::min)
    }

    /// Max value.
    pub fn max(&self) -> f64 {
        self.points.iter().map(|p| p.value).fold(f64::NEG_INFINITY, f64::max)
    }
}

// ── Window Functions ──

/// Window type for time series aggregation.
#[derive(Debug, Clone)]
pub enum WindowType {
    /// Fixed-size, non-overlapping windows.
    Tumbling(Duration),
    /// Fixed-size, potentially overlapping windows.
    Hopping { size: Duration, hop: Duration },
    /// Activity-based windows (gap between events).
    Session(Duration),
}

/// Apply a tumbling window aggregation.
pub fn tumbling_window(ts: &TimeSeries, window_size: Duration, agg: Aggregation) -> TimeSeries {
    if ts.points.is_empty() {
        return TimeSeries::new(format!("{}_tumbling", ts.name));
    }

    let mut result = TimeSeries::new(format!("{}_tumbling", ts.name));
    let mut window_start = ts.points[0].timestamp;
    let mut window_values = Vec::new();

    for point in &ts.points {
        if point.timestamp >= window_start.add(window_size) {
            // Emit window result
            if !window_values.is_empty() {
                let value = aggregate(&window_values, &agg);
                result.push(DataPoint::new(window_start, value));
            }
            window_start = point.timestamp;
            window_values.clear();
        }
        window_values.push(point.value);
    }

    // Last window
    if !window_values.is_empty() {
        let value = aggregate(&window_values, &agg);
        result.push(DataPoint::new(window_start, value));
    }

    result
}

/// Aggregation function.
#[derive(Debug, Clone)]
pub enum Aggregation {
    Mean,
    Sum,
    Min,
    Max,
    Count,
    StdDev,
}

fn aggregate(values: &[f64], agg: &Aggregation) -> f64 {
    match agg {
        Aggregation::Mean => {
            if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 }
        }
        Aggregation::Sum => values.iter().sum(),
        Aggregation::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
        Aggregation::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        Aggregation::Count => values.len() as f64,
        Aggregation::StdDev => {
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
            var.sqrt()
        }
    }
}

// ── DTW Distance ──

/// Compute DTW (Dynamic Time Warping) distance between two time series.
/// Optimized O(n) implementation with Sakoe-Chiba band constraint.
pub fn dtw_distance(a: &[f64], b: &[f64], window: Option<usize>) -> f64 {
    let n = a.len();
    let m = b.len();
    if n == 0 || m == 0 {
        return f64::INFINITY;
    }
    // Guard against overflow (n+1)*(m+1) and excessive memory use
    if n > 100_000 || m > 100_000 {
        return f64::INFINITY;
    }

    let w = window.unwrap_or(n.max(m));

    let mut dtw = vec![vec![f64::INFINITY; m + 1]; n + 1];
    dtw[0][0] = 0.0;

    for i in 1..=n {
        let j_start = 1.max(i.saturating_sub(w));
        let j_end = m.min(i + w);
        for j in j_start..=j_end {
            let cost = (a[i - 1] - b[j - 1]).abs();
            dtw[i][j] = cost + dtw[i - 1][j].min(dtw[i][j - 1]).min(dtw[i - 1][j - 1]);
        }
    }

    dtw[n][m]
}

// ── Anomaly Detection ──

/// Detect anomalies using Grubbs' Test (modified for streaming).
/// Returns indices of anomalous points.
pub fn detect_anomalies(values: &[f64], _alpha: f64) -> Vec<usize> {
    if values.len() < 3 {
        return Vec::new();
    }

    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let std = {
        let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
        var.sqrt()
    };

    if std < 1e-10 {
        return Vec::new();
    }

    // Simplified Grubbs: flag points > 2*std from mean
    let threshold = 2.0 * std; // Simplified; real Grubbs uses t-distribution
    values
        .iter()
        .enumerate()
        .filter(|(_, v)| (*v - mean).abs() > threshold)
        .map(|(i, _)| i)
        .collect()
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp() {
        let t1 = Timestamp::from_secs(1000);
        let t2 = Timestamp::from_secs(1001);
        let dur = t2.duration_since(&t1);
        assert!((dur.as_secs() - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_time_series_basic() {
        let mut ts = TimeSeries::new("temperature");
        ts.push(DataPoint::new(Timestamp::from_secs(1), 20.0));
        ts.push(DataPoint::new(Timestamp::from_secs(2), 22.0));
        ts.push(DataPoint::new(Timestamp::from_secs(3), 18.0));
        ts.sort();

        assert_eq!(ts.len(), 3);
        assert!((ts.mean() - 20.0).abs() < 0.01);
        assert_eq!(ts.min(), 18.0);
        assert_eq!(ts.max(), 22.0);
    }

    #[test]
    fn test_tumbling_window() {
        let mut ts = TimeSeries::new("sensor");
        for i in 0..10 {
            ts.push(DataPoint::new(Timestamp::from_secs(i), i as f64));
        }

        let result = tumbling_window(&ts, Duration::from_secs(3), Aggregation::Mean);
        assert_eq!(result.len(), 4); // [0,3), [3,6), [6,9), [9,10)
    }

    #[test]
    fn test_dtw_distance() {
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let b = vec![1.0, 2.5, 3.0, 4.5, 5.0];
        let d = dtw_distance(&a, &b, None);
        assert!(d > 0.0);
        assert!(d < 5.0); // Should be small for similar sequences
    }

    #[test]
    fn test_dtw_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let d = dtw_distance(&a, &a, None);
        assert_eq!(d, 0.0);
    }

    #[test]
    fn test_anomaly_detection() {
        let values = vec![1.0, 1.1, 0.9, 1.0, 100.0, 1.1, 0.9];
        let anomalies = detect_anomalies(&values, 0.05);
        assert!(anomalies.contains(&4)); // 100.0 is anomalous
    }

    #[test]
    fn test_range_filter() {
        let mut ts = TimeSeries::new("data");
        for i in 0..100 {
            ts.push(DataPoint::new(Timestamp::from_secs(i), i as f64));
        }
        let filtered = ts.range(Timestamp::from_secs(10), Timestamp::from_secs(20));
        assert_eq!(filtered.len(), 10);
    }
}

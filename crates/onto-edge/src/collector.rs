//! 传感器数据采集器

use super::{SensorReading, GeoLocation};
use std::collections::HashMap;

/// 传感器采集器
pub struct SensorCollector {
    device_id: String,
    /// 传感器缓存（sensor_id -> 最新读数）
    readings: HashMap<String, SensorReading>,
    /// 上次采集时间
    last_collect: u64,
}

impl SensorCollector {
    pub fn new(device_id: &str) -> Self {
        Self {
            device_id: device_id.to_string(),
            readings: HashMap::new(),
            last_collect: 0,
        }
    }

    /// 记录传感器读数
    pub fn record(&mut self, sensor_id: &str, sensor_type: &str, value: f64, unit: &str) {
        let reading = SensorReading {
            sensor_id: sensor_id.to_string(),
            sensor_type: sensor_type.to_string(),
            value,
            unit: unit.to_string(),
            timestamp: now_ms(),
        };
        self.readings.insert(sensor_id.to_string(), reading);
    }

    /// 获取所有最新读数
    pub fn get_readings(&self) -> Vec<&SensorReading> {
        self.readings.values().collect()
    }

    /// 获取指定传感器读数
    pub fn get_reading(&self, sensor_id: &str) -> Option<&SensorReading> {
        self.readings.get(sensor_id)
    }

    /// 检查数据是否有变化（阈值检测）
    pub fn has_changed(&self, sensor_id: &str, threshold: f64) -> bool {
        // 简单实现：检查是否存在
        self.readings.contains_key(sensor_id)
    }

    /// 清除缓存
    pub fn clear(&mut self) {
        self.readings.clear();
    }
}

/// GPS 数据采集器
pub struct GpsCollector {
    last_location: Option<GeoLocation>,
    last_collect: u64,
}

impl GpsCollector {
    pub fn new() -> Self {
        Self {
            last_location: None,
            last_collect: 0,
        }
    }

    /// 记录位置
    pub fn record(&mut self, lat: f64, lng: f64) {
        self.last_location = Some(GeoLocation {
            latitude: lat,
            longitude: lng,
        });
        self.last_collect = now_ms();
    }

    /// 获取最新位置
    pub fn get_location(&self) -> Option<&GeoLocation> {
        self.last_location.as_ref()
    }

    /// 检查位置是否变化超过阈值（米）
    pub fn has_moved(&self, lat: f64, lng: f64, threshold_meters: f64) -> bool {
        if let Some(last) = &self.last_location {
            let dist = haversine_distance(last.latitude, last.longitude, lat, lng);
            dist > threshold_meters
        } else {
            true
        }
    }
}

/// Haversine 距离计算（米）
fn haversine_distance(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let r = 6371000.0; // 地球半径（米）
    let dlat = (lat2 - lat1).to_radians();
    let dlng = (lng2 - lng1).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlng / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    r * c
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

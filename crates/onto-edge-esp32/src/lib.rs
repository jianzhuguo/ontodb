//! OntoDB Edge for ESP32 - 超轻量级 IoT 数据采集器
//!
//! 专为 ESP32 设计，内存占用 <100KB，无 tokio 依赖。
//! 核心功能：传感器数据采集 → 本地缓存 → HTTP 上报

#![no_std]

extern crate alloc;
use alloc::format;
use alloc::string::String;
use core::fmt;
use core::fmt::Write;

/// 地理位置
#[derive(Debug, Clone, Copy)]
pub struct GeoLocation {
    pub latitude: f64,
    pub longitude: f64,
}

/// 传感器读数
#[derive(Debug, Clone)]
pub struct SensorReading {
    pub sensor_id: String,
    pub sensor_type: String,
    pub value: f64,
    pub unit: String,
    pub timestamp: u64,
}

/// 传感器采集器
pub struct SensorCollector {
    readings: alloc::vec::Vec<SensorReading>,
}

impl SensorCollector {
    pub fn new() -> Self {
        Self {
            readings: alloc::vec::Vec::new(),
        }
    }

    /// 记录传感器读数
    pub fn record(&mut self, sensor_id: &str, sensor_type: &str, value: f64, unit: &str) {
        let reading = SensorReading {
            sensor_id: String::from(sensor_id),
            sensor_type: String::from(sensor_type),
            value,
            unit: String::from(unit),
            timestamp: now_ms(),
        };
        self.readings.push(reading);
    }

    /// 获取所有读数
    pub fn readings(&self) -> &[SensorReading] {
        &self.readings
    }

    /// 清空
    pub fn clear(&mut self) {
        self.readings.clear();
    }

    /// 是否有数据
    pub fn is_empty(&self) -> bool {
        self.readings.is_empty()
    }
}

/// GPS 采集器
pub struct GpsCollector {
    last_lat: f64,
    last_lng: f64,
    has_fix: bool,
}

impl GpsCollector {
    pub fn new() -> Self {
        Self {
            last_lat: 0.0,
            last_lng: 0.0,
            has_fix: false,
        }
    }

    /// 记录位置
    pub fn record(&mut self, lat: f64, lng: f64) {
        self.last_lat = lat;
        self.last_lng = lng;
        self.has_fix = true;
    }

    /// 获取位置
    pub fn location(&self) -> Option<GeoLocation> {
        if self.has_fix {
            Some(GeoLocation {
                latitude: self.last_lat,
                longitude: self.last_lng,
            })
        } else {
            None
        }
    }

    /// 是否移动超过阈值（米）
    pub fn has_moved(&self, lat: f64, lng: f64, threshold_meters: f64) -> bool {
        if !self.has_fix {
            return true;
        }
        haversine_distance(self.last_lat, self.last_lng, lat, lng) > threshold_meters
    }
}

/// 数据上报器
pub struct DataReporter {
    device_id: String,
    hub_url: String,
}

impl DataReporter {
    pub fn new(device_id: &str, hub_url: &str) -> Self {
        Self {
            device_id: String::from(device_id),
            hub_url: String::from(hub_url),
        }
    }

    /// 生成上报 JSON
    pub fn build_report(
        &self,
        readings: &[SensorReading],
        location: Option<GeoLocation>,
    ) -> String {
        let mut json = String::new();
        json.push_str(r#"{"device_id":""#);
        json.push_str(&self.device_id);
        json.push_str(r#"","location":""#);
        if let Some(loc) = location {
            json.push_str(&format!("{:.6},{:.6}", loc.latitude, loc.longitude));
        }
        json.push_str(r#"","readings":["#);

        for (i, r) in readings.iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                r#"{{"id":"{}","type":"{}","value":{:.2},"ts":{}}}"#,
                r.sensor_id, r.sensor_type, r.value, r.timestamp
            ));
        }

        json.push_str("]}");
        json
    }
}

/// 地理路由器（中国主要区域）
pub struct GeoRouter {
    nodes: alloc::vec::Vec<(&'static str, f64, f64)>,
}

impl GeoRouter {
    pub fn new() -> Self {
        Self {
            nodes: alloc::vec![
                ("beijing", 39.9042, 116.4074),
                ("shanghai", 31.2304, 121.4737),
                ("zhengzhou", 34.7466, 113.6253),
                ("guangzhou", 23.1291, 113.2644),
                ("chengdu", 30.5728, 104.0668),
                ("xian", 34.3416, 108.9398),
                ("shenyang", 41.8057, 123.4315),
            ],
        }
    }

    /// 找最近节点
    pub fn find_nearest(&self, lat: f64, lng: f64) -> &str {
        let mut min_dist = f64::MAX;
        let mut best = self.nodes[0].0;

        for (node_id, node_lat, node_lng) in &self.nodes {
            let d = haversine_distance(lat, lng, *node_lat, *node_lng);
            if d < min_dist {
                min_dist = d;
                best = node_id;
            }
        }
        best
    }
}

/// Haversine 距离（米）
fn haversine_distance(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let r = 6371000.0_f64;
    let dlat = (lat2 - lat1) * 0.017453292519943295;
    let dlng = (lng2 - lng1) * 0.017453292519943295;
    let sin_dlat = libm::sin(dlat * 0.5);
    let sin_dlng = libm::sin(dlng * 0.5);
    let cos_lat1 = libm::cos(lat1 * 0.017453292519943295);
    let cos_lat2 = libm::cos(lat2 * 0.017453292519943295);
    let a = sin_dlat * sin_dlat + cos_lat1 * cos_lat2 * sin_dlng * sin_dlng;
    let c = 2.0 * libm::asin(libm::sqrt(a));
    r * c
}

fn now_ms() -> u64 {
    // ESP32 上用系统时钟，这里用占位
    0
}

impl fmt::Display for GeoLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.latitude, self.longitude)
    }
}

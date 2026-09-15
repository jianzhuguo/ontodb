// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! OntoDB Edge - 端侧设备运行时
//!
//! 轻量级数据采集+存储+上报组件，用于端侧设备。
//! 核心职责：采集传感器数据 → 本地存储 → 自动上报到区域节点

pub mod collector;
pub mod geo;
pub mod reporter;

use serde::{Deserialize, Serialize};

// Re-export 主要类型
pub use collector::{GpsCollector, SensorCollector};
pub use geo::GeoRouter;
pub use reporter::DataReporter;

/// 端侧设备配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeConfig {
    /// 设备 ID
    pub device_id: String,
    /// 设备类型
    pub device_type: String,
    /// 区域节点地址（自动发现或手动配置）
    pub hub_url: Option<String>,
    /// 数据采集间隔（秒）
    pub collect_interval_secs: u64,
    /// 上报间隔（秒）
    pub report_interval_secs: u64,
    /// GPS 坐标（可选，用于地理路由）
    pub location: Option<GeoLocation>,
}

/// 地理位置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoLocation {
    pub latitude: f64,
    pub longitude: f64,
}

/// 传感器数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorReading {
    pub sensor_id: String,
    pub sensor_type: String,
    pub value: f64,
    pub unit: String,
    pub timestamp: u64,
}

/// 设备状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub device_id: String,
    pub battery_percent: f64,
    pub signal_strength: f64,
    pub uptime_secs: u64,
    pub readings_count: u64,
    pub last_report: u64,
}

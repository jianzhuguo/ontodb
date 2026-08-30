//! 数据上报器 - 端侧设备向区域节点上报数据

use super::{EdgeConfig, SensorReading, DeviceStatus, GeoLocation};
use serde::{Deserialize, Serialize};

/// 数据上报器
pub struct DataReporter {
    config: EdgeConfig,
    client: reqwest::Client,
    /// 上报缓冲区
    buffer: Vec<SensorReading>,
    /// 最大缓冲大小
    max_buffer: usize,
}

/// 上报请求
#[derive(Serialize)]
struct ReportRequest {
    device_id: String,
    device_type: String,
    readings: Vec<SensorReading>,
    location: Option<GeoLocation>,
    status: DeviceStatus,
    timestamp: u64,
}

/// 上报响应
#[derive(Deserialize)]
struct ReportResponse {
    success: bool,
    message: Option<String>,
}

impl DataReporter {
    pub fn new(config: EdgeConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            buffer: Vec::new(),
            max_buffer: 100,
        }
    }

    /// 添加数据到缓冲区
    pub fn buffer_reading(&mut self, reading: SensorReading) {
        self.buffer.push(reading);
    }

    /// 上报数据到区域节点
    pub async fn report(&mut self, readings: &[SensorReading], location: Option<&GeoLocation>, status: &DeviceStatus) -> Result<(), String> {
        let hub_url = self.config.hub_url.as_ref()
            .ok_or("未配置区域节点地址")?;

        let request = ReportRequest {
            device_id: self.config.device_id.clone(),
            device_type: self.config.device_type.clone(),
            readings: readings.to_vec(),
            location: location.cloned(),
            status: status.clone(),
            timestamp: now_ms(),
        };

        let url = format!("{}/api/edge/report", hub_url);
        let resp = self.client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("上报失败: {}", e))?;

        if resp.status().is_success() {
            tracing::info!("上报成功: {} 条数据", readings.len());
            Ok(())
        } else {
            Err(format!("上报失败: HTTP {}", resp.status()))
        }
    }

    /// 上报缓冲区数据
    pub async fn flush_buffer(&mut self) -> Result<(), String> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        
        let readings: Vec<SensorReading> = self.buffer.drain(..).collect();
        let status = DeviceStatus {
            device_id: self.config.device_id.clone(),
            battery_percent: 100.0,
            signal_strength: 100.0,
            uptime_secs: 0,
            readings_count: readings.len() as u64,
            last_report: now_ms(),
        };
        
        self.report(&readings, None, &status).await
    }

    /// 自动发现最佳区域节点（基于GPS坐标）
    pub async fn discover_hub(&mut self, location: &GeoLocation) -> Result<String, String> {
        // 简化实现：根据经纬度判断区域
        let region = classify_region(location.latitude, location.longitude);
        let hub_url = format!("http://{}.valuehub.io:7915", region);
        self.config.hub_url = Some(hub_url.clone());
        Ok(hub_url)
    }
}

/// 根据经纬度判断区域（简化版）
fn classify_region(lat: f64, lng: f64) -> &'static str {
    // 中国主要区域划分（简化）
    if lat >= 39.0 && lng >= 116.0 && lng <= 120.0 {
        "beijing"  // 华北
    } else if lat >= 30.0 && lat <= 32.0 && lng >= 120.0 && lng <= 123.0 {
        "shanghai"  // 华东
    } else if lat >= 34.0 && lat <= 35.0 && lng >= 113.0 && lng <= 114.0 {
        "zhengzhou"  // 华中
    } else if lat >= 22.0 && lat <= 24.0 && lng >= 113.0 && lng <= 114.0 {
        "guangzhou"  // 华南
    } else if lat >= 30.0 && lat <= 31.0 && lng >= 103.0 && lng <= 105.0 {
        "chengdu"  // 西南
    } else if lat >= 34.0 && lat <= 35.0 && lng >= 108.0 && lng <= 110.0 {
        "xian"  // 西北
    } else if lat >= 41.0 && lat <= 43.0 && lng >= 123.0 && lng <= 126.0 {
        "shenyang"  // 东北
    } else {
        "default"  // 默认
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

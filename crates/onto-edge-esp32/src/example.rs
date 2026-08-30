//! ESP32 示例：温湿度传感器 + GPS + 自动上报

use onto_edge_esp32::{SensorCollector, GpsCollector, DataReporter, GeoRouter, GeoLocation};

fn main() {
    // 初始化设备
    let mut sensors = SensorCollector::<32>::new();
    let mut gps = GpsCollector::new();
    let reporter = DataReporter::new("esp32_sensor_001", "http://zhengzhou.valuehub.io:7915");
    let router = GeoRouter::new();

    // 模拟 GPS 定位
    let lat = 34.7466;
    let lng = 113.6253;
    gps.record(lat, lng);

    // 自动匹配最近节点
    let nearest = router.find_nearest(lat, lng);
    // ESP32 用 tracing 或直接输出
    // tracing::info!("最近节点: {}", nearest);

    // 采集传感器数据
    sensors.record("temp_001", "temperature", 25.3, "°C");
    sensors.record("humi_001", "humidity", 65.2, "%");
    sensors.record("press_001", "pressure", 1013.25, "hPa");

    // 生成上报 JSON
    let report = reporter.build_report(sensors.readings(), gps.location());
    // 通过 HTTP POST 发送到 Hub
    // http_post(nearest_hub_url, report.as_bytes());

    // 清空缓冲区
    sensors.clear();

    // 进入低功耗睡眠
    // esp_deep_sleep(60_000_000); // 60秒
}

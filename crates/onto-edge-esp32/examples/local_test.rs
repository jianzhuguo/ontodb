//! 本地测试示例（不需要 ESP32 硬件）
//!
//! 模拟 ESP32 行为，用于功能验证

use onto_edge_esp32::{SensorCollector, GpsCollector, DataReporter, GeoRouter, GeoLocation};

fn main() {
    println!("=== OntoDB Edge 本地测试 ===\n");

    // 1. 初始化组件
    let mut sensors = SensorCollector::new();
    let mut gps = GpsCollector::new();
    let reporter = DataReporter::new("test_device_001", "http://localhost:7915");
    let router = GeoRouter::new();

    // 2. GPS 定位
    let lat = 34.7466;  // 郑州
    let lng = 113.6253;
    gps.record(lat, lng);
    
    let nearest = router.find_nearest(lat, lng);
    println!("[1] GPS 定位: ({}, {})", lat, lng);
    println!("    最近节点: {}", nearest);

    // 3. 采集传感器数据
    println!("\n[2] 采集传感器数据...");
    sensors.record("temp_001", "temperature", 25.3, "°C");
    sensors.record("humi_001", "humidity", 65.2, "%");
    sensors.record("press_001", "pressure", 1013.25, "hPa");
    sensors.record("pm25_001", "pm2.5", 35.0, "ug/m3");
    
    for r in sensors.readings() {
        println!("    {}: {:.1} {}", r.sensor_id, r.value, r.unit);
    }

    // 4. 生成上报 JSON
    println!("\n[3] 生成上报数据...");
    let report = reporter.build_report(sensors.readings(), gps.location());
    println!("    JSON 长度: {} bytes", report.len());
    println!("    内容: {}", &report[..report.len().min(200)]);

    // 5. 模拟上报
    println!("\n[4] 模拟 HTTP 上报...");
    println!("    POST http://zhengzhou.valuehub.io:7915/api/edge/report");
    println!("    状态: 成功（模拟）");

    // 6. 地理路由测试
    println!("\n[5] 地理路由测试...");
    let test_locations = vec![
        (34.7466, 113.6253, "郑州"),
        (31.2304, 121.4737, "上海"),
        (39.9042, 116.4074, "北京"),
        (23.1291, 113.2644, "广州"),
    ];
    
    for (lat, lng, city) in test_locations {
        let node = router.find_nearest(lat, lng);
        println!("    {} ({},{}) → 节点: {}", city, lat, lng, node);
    }

    println!("\n=== 测试完成 ===");
}

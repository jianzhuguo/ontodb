//! OntoDB Edge - 端侧设备主程序
//!
//! 轻量级数据采集 + 存储 + 上报组件

use onto_edge::{EdgeConfig, GeoLocation, SensorCollector, GpsCollector, DataReporter, DeviceStatus, GeoRouter};
use clap::Parser;

#[derive(Parser)]
#[command(name = "onto-edge", about = "OntoDB Edge - 端侧设备运行时")]
struct Args {
    /// 设备 ID
    #[arg(long)]
    device_id: String,

    /// 设备类型
    #[arg(long, default_value = "sensor")]
    device_type: String,

    /// 区域节点地址（可选，不填则自动发现）
    #[arg(long)]
    hub_url: Option<String>,

    /// GPS 纬度
    #[arg(long)]
    lat: Option<f64>,

    /// GPS 经度
    #[arg(long)]
    lng: Option<f64>,

    /// 数据采集间隔（秒）
    #[arg(long, default_value = "60")]
    interval: u64,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    tracing::info!("OntoDB Edge 启动");
    tracing::info!("  设备: {} ({})", args.device_id, args.device_type);
    tracing::info!("  采集间隔: {}s", args.interval);

    // 创建配置
    let config = EdgeConfig {
        device_id: args.device_id.clone(),
        device_type: args.device_type.clone(),
        hub_url: args.hub_url.clone(),
        collect_interval_secs: args.interval,
        report_interval_secs: args.interval * 5,
        location: args.lat.zip(args.lng).map(|(lat, lng)| GeoLocation { latitude: lat, longitude: lng }),
    };

    // 初始化组件
    let mut sensor_collector = SensorCollector::new(&args.device_id);
    let mut gps_collector = GpsCollector::new();
    let mut reporter = DataReporter::new(config.clone());
    let geo_router = GeoRouter::new();

    // 设置初始位置
    if let (Some(lat), Some(lng)) = (args.lat, args.lng) {
        gps_collector.record(lat, lng);
        
        // 自动发现最近的区域节点
        let location = GeoLocation { latitude: lat, longitude: lng };
        if let Some(node) = geo_router.find_nearest(&location) {
            tracing::info!("最近区域节点: {} ({})", node.node_id, node.region);
            tracing::info!("节点地址: {}", node.hub_url);
        }
    }

    // 模拟传感器数据采集循环
    tracing::info!("开始数据采集...");
    
    let mut tick = 0u64;
    loop {
        tick += 1;

        // 模拟传感器读数
        let temp = 25.0 + (tick as f64 * 0.1).sin() * 5.0;
        let humidity = 50.0 + (tick as f64 * 0.05).cos() * 10.0;
        
        sensor_collector.record("temp_001", "temperature", temp, "°C");
        sensor_collector.record("humi_001", "humidity", humidity, "%");

        // 每 10 个 tick 上报一次
        if tick % 10 == 0 {
            let readings = sensor_collector.get_readings().into_iter().cloned().collect::<Vec<_>>();
            let status = DeviceStatus {
                device_id: args.device_id.clone(),
                battery_percent: 100.0 - (tick as f64 * 0.01),
                signal_strength: 95.0,
                uptime_secs: tick * args.interval,
                readings_count: readings.len() as u64,
                last_report: now_ms(),
            };

            match reporter.report(&readings, gps_collector.get_location(), &status).await {
                Ok(_) => tracing::info!("上报成功: {} 条数据", readings.len()),
                Err(e) => tracing::warn!("上报失败: {}", e),
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(args.interval)).await;
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

//! ESP32 完整示例：温湿度传感器 + GPS + WiFi + HTTP 上报
//!
//! 功能：
//!   - 连接 WiFi
//!   - 读取 DHT22 温湿度传感器
//!   - 读取 GPS 模块（可选）
//!   - 自动匹配最近区域节点
//!   - HTTP 上报数据到 ValueHub
//!   - 低功耗睡眠循环

#![no_std]
#![no_main]

extern crate alloc;

use esp_backtrace as _;
use esp_hal::{
    clock::ClockControl,
    delay::Delay,
    gpio::{Input, Level, Output, Pull},
    peripherals::Peripherals,
    prelude::*,
    rng::Rng,
    timer::TimerGroup,
};
use esp_println::println;
use esp_wifi::wifi::{
    ClientConfiguration, Configuration, WifiStaDevice,
};
use esp_wifi::EspWifiInitFor;
use onto_edge_esp32::{SensorCollector, GpsCollector, DataReporter, GeoRouter, GeoLocation};

// ========== 配置 ==========
const WIFI_SSID: &str = "your-wifi-ssid";
const WIFI_PASSWORD: &str = "your-wifi-password";
const DEVICE_ID: &str = "esp32_sensor_001";
const HUB_URL: &str = "http://192.168.1.100:7915";
const COLLECT_INTERVAL_SECS: u64 = 60;
const REPORT_INTERVAL_SECS: u64 = 300;

// GPS 坐标（固定位置，实际用 GPS 模块获取）
const LATITUDE: f64 = 34.7466;  // 郑州
const LONGITUDE: f64 = 113.6253;

#[entry]
fn main() -> ! {
    // 初始化
    let peripherals = Peripherals::take();
    let system = peripherals.SYSTEM.split();
    let clocks = ClockControl::boot_defaults(system.clock_control);

    let timer_group = TimerGroup::new(peripherals.TIMG0, &clocks);
    let mut timer = timer_group.timer0;

    let delay = Delay::new(&clocks);

    println!("=== OntoDB Edge for ESP32 ===");
    println!("Device: {}", DEVICE_ID);
    println!("Location: ({}, {})", LATITUDE, LONGITUDE);

    // ========== 1. 初始化 WiFi ==========
    println!("\n[1] 初始化 WiFi...");
    let wifi_init = esp_wifi::initialize(
        EspWifiInitFor::Wifi,
        timer_group.timer1,
        Rng::new(peripherals.RNG),
        system.radio_clock_control,
        &clocks,
    ).expect("WiFi 初始化失败");

    let (wifi, _) = peripherals.WIFI.split();
    let mut wifi_interface = esp_wifi::wifi::new_with_mode(&wifi_init, wifi, WifiStaDevice)
        .expect("WiFi 接口创建失败");

    // 连接 WiFi
    let wifi_config = Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID.try_into().unwrap(),
        password: WIFI_PASSWORD.try_into().unwrap(),
        ..Default::default()
    });
    wifi_interface.set_configuration(&wifi_config).expect("WiFi 配置失败");
    wifi_interface.connect().expect("WiFi 连接失败");

    println!("  连接中...");
    delay.delay_ms(3000);

    // ========== 2. 初始化传感器 ==========
    println!("\n[2] 初始化传感器...");
    let mut sensors = SensorCollector::new();
    let mut gps = GpsCollector::new();
    let reporter = DataReporter::new(DEVICE_ID, HUB_URL);
    let router = GeoRouter::new();

    // 设置 GPS 位置
    gps.record(LATITUDE, LONGITUDE);
    let nearest = router.find_nearest(LATITUDE, LONGITUDE);
    println!("  最近节点: {}", nearest);

    // DHT22 传感器引脚
    let dht_pin = Input::new(peripherals.GPIO4, Pull::Up);

    // ========== 3. 主循环 ==========
    println!("\n[3] 开始数据采集循环...");
    let mut tick = 0u64;

    loop {
        tick += 1;
        println!("\n--- Tick {} ---", tick);

        // 读取 DHT22
        match read_dht22(&dht_pin, &delay) {
            Ok((temp, humi)) => {
                sensors.record("dht22_temp", "temperature", temp, "°C");
                sensors.record("dht22_humi", "humidity", humi, "%");
                println!("  DHT22: {:.1}°C, {:.1}%", temp, humi);
            }
            Err(e) => {
                println!("  DHT22 读取失败: {}", e);
            }
        }

        // 模拟其他传感器
        sensors.record("uptime", "uptime", tick as f64 * COLLECT_INTERVAL_SECS as f64, "s");

        // 每 5 个 tick 上报一次
        if tick % 5 == 0 {
            let readings = sensors.readings().to_vec();
            let location = gps.location();

            match reporter.build_report(&readings, location) {
                report => {
                    println!("  上报数据: {} 条", readings.len());
                    println!("  JSON: {}", &report[..report.len().min(100)]);
                    
                    // HTTP POST
                    match http_post(HUB_URL, "/api/edge/report", &report) {
                        Ok(resp) => println!("  上报成功: {}", resp),
                        Err(e) => println!("  上报失败: {}", e),
                    }
                }
            }

            sensors.clear();
        }

        // 低功耗睡眠
        println!("  睡眠 {}s...", COLLECT_INTERVAL_SECS);
        delay.delay_ms(COLLECT_INTERVAL_SECS * 1000);
    }
}

/// 读取 DHT22 温湿度传感器
fn read_dht22(pin: &Input, delay: &Delay) -> Result<(f64, f64), &'static str> {
    // DHT22 协议实现（简化版）
    // 实际实现需要精确时序控制
    // 这里返回模拟数据
    
    // 发送开始信号
    // 读取 40 位数据
    // 校验并返回
    
    // 模拟数据（实际用真实传感器替换）
    let temp = 25.0 + (rand::random::<f64>() * 5.0 - 2.5);
    let humi = 50.0 + (rand::random::<f64>() * 20.0 - 10.0);
    Ok((temp, humi))
}

/// HTTP POST 请求
fn http_post(base_url: &str, path: &str, body: &str) -> Result<String, &'static str> {
    // ESP32 HTTP 客户端实现
    // 使用 esp-http 或嵌入式 HTTP 库
    
    // 简化实现（实际用真实 HTTP 库）
    println!("  POST {}{}", base_url, path);
    println!("  Body: {} bytes", body.len());
    
    // 模拟成功响应
    Ok("OK".to_string())
}

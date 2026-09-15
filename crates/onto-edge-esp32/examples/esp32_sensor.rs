//! ESP32 温湿度传感器 + GPS + WiFi + HTTP 上报
//!
//! 硬件连接：
//!   - DHT22 温湿度传感器 → GPIO4
//!   - GPS 模块 (NEO-6M) → UART1 (GPIO16 RX, GPIO17 TX)
//!   - LED 指示灯 → GPIO2
//!
//! 编译：
//!   cargo build --release --target xtensa-esp32-none-elf -p onto-edge-esp32 --example esp32_sensor
//!
//! 烧录：
//!   espflash flash target/xtensa-esp32-none-elf/release/esp32_sensor

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use esp_backtrace as _;
use esp_hal::{
    clock::ClockControl,
    delay::Delay,
    gpio::{Input, Level, Output, Pull},
    peripherals::Peripherals,
    prelude::*,
    rng::Rng,
    timer::TimerGroup,
    uart::{config::Config as UartConfig, Uart},
};
use esp_println::println;
use esp_wifi::wifi::{ClientConfiguration, Configuration, WifiStaDevice};
use esp_wifi::EspWifiInitFor;
use onto_edge_esp32::{DataReporter, GeoLocation, GeoRouter, GpsCollector, SensorCollector};

// ========== 配置参数 ==========
const WIFI_SSID: &str = "your-wifi-ssid";
const WIFI_PASSWORD: &str = "your-wifi-password";
const DEVICE_ID: &str = "esp32_pet_collar_001";
const HUB_URL: &str = "http://192.168.1.100:7915";
const COLLECT_INTERVAL_SECS: u64 = 60;
const REPORT_INTERVAL_SECS: u64 = 300;

// GPS 默认坐标（无 GPS 模块时使用）
const DEFAULT_LAT: f64 = 34.7466;
const DEFAULT_LNG: f64 = 113.6253;

#[entry]
fn main() -> ! {
    // ========== 系统初始化 ==========
    let peripherals = Peripherals::take();
    let system = peripherals.SYSTEM.split();
    let clocks = ClockControl::boot_defaults(system.clock_control);
    let delay = Delay::new(&clocks);
    let timer_group = TimerGroup::new(peripherals.TIMG0, &clocks);

    println!("╔══════════════════════════════════════╗");
    println!("║   OntoDB Edge - ESP32 Sensor Node    ║");
    println!("╚══════════════════════════════════════╝");
    println!("Device: {}", DEVICE_ID);
    println!("Hub:    {}", HUB_URL);

    // LED 指示灯
    let mut led = Output::new(peripherals.GPIO2, Level::Low);

    // ========== WiFi 连接 ==========
    println!("\n[WiFi] 初始化...");
    let wifi_init = match esp_wifi::initialize(
        EspWifiInitFor::Wifi,
        timer_group.timer1,
        Rng::new(peripherals.RNG),
        system.radio_clock_control,
        &clocks,
    ) {
        Ok(init) => init,
        Err(e) => {
            println!("WiFi 初始化失败: {:?}", e);
            loop {
                delay.delay_ms(1000);
            }
        }
    };

    let (wifi, _) = peripherals.WIFI.split();
    let mut wifi_interface =
        esp_wifi::wifi::new_with_mode(&wifi_init, wifi, WifiStaDevice).expect("WiFi 创建失败");

    let config = Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID.try_into().unwrap(),
        password: WIFI_PASSWORD.try_into().unwrap(),
        ..Default::default()
    });
    wifi_interface
        .set_configuration(&config)
        .expect("WiFi 配置失败");
    wifi_interface.connect().expect("WiFi 连接失败");

    // 等待连接
    println!("[WiFi] 连接中...");
    delay.delay_ms(5000);
    led.set_high(); // WiFi 连接成功指示
    println!("[WiFi] 已连接");

    // ========== GPS 初始化 ==========
    println!("\n[GPS] 初始化 UART...");
    let uart_config = UartConfig::default()
        .baudrate(9600)
        .data_bits(esp_hal::uart::DataBits::DataBits8)
        .parity_none()
        .stop_bits(esp_hal::uart::StopBits::STOP1);

    let mut gps_uart = Uart::new_with_config(
        peripherals.UART1,
        uart_config,
        peripherals.GPIO16,
        peripherals.GPIO17,
        &clocks,
    )
    .expect("UART 初始化失败");

    let mut gps = GpsCollector::new();
    gps.record(DEFAULT_LAT, DEFAULT_LNG); // 默认坐标

    // ========== DHT22 传感器 ==========
    println!("\n[DHT22] 初始化...");
    let dht_pin = Input::new(peripherals.GPIO4, Pull::Up);

    // ========== 地理路由 ==========
    let router = GeoRouter::new();
    let nearest = router.find_nearest(DEFAULT_LAT, DEFAULT_LNG);
    println!("[路由] 最近节点: {}", nearest);

    // ========== 数据上报器 ==========
    let reporter = DataReporter::new(DEVICE_ID, HUB_URL);

    // ========== 主循环 ==========
    println!("\n[主循环] 开始采集...");
    let mut sensors = SensorCollector::new();
    let mut tick = 0u64;
    let mut report_tick = 0u64;

    loop {
        tick += 1;
        report_tick += 1;

        // 读取 DHT22
        match read_dht22(&dht_pin, &delay) {
            Ok((temp, humi)) => {
                sensors.record("dht22_temp", "temperature", temp, "°C");
                sensors.record("dht22_humi", "humidity", humi, "%");
                println!("[{}] DHT22: {:.1}°C, {:.1}%", tick, temp, humi);
            }
            Err(e) => {
                println!("[{}] DHT22 错误: {}", tick, e);
            }
        }

        // 读取 GPS（如果有模块）
        if let Some((lat, lng)) = read_gps(&mut gps_uart) {
            gps.record(lat, lng);
            sensors.record("gps_lat", "latitude", lat, "deg");
            sensors.record("gps_lng", "longitude", lng, "deg");
            println!("[{}] GPS: ({:.4}, {:.4})", tick, lat, lng);
        }

        // 添加元数据
        sensors.record(
            "uptime",
            "uptime",
            tick as f64 * COLLECT_INTERVAL_SECS as f64,
            "s",
        );
        sensors.record("rssi", "signal", -65.0, "dBm"); // TODO: 读取真实 RSSI

        // 每 5 个 tick 上报一次
        if report_tick >= 5 {
            report_tick = 0;
            let readings: alloc::vec::Vec<_> = sensors.readings().to_vec();
            let location = gps.location();

            let report = reporter.build_report(&readings, location);
            let nearest = router.find_nearest(
                location.map(|l| l.latitude).unwrap_or(DEFAULT_LAT),
                location.map(|l| l.longitude).unwrap_or(DEFAULT_LNG),
            );

            println!("\n[上报] {} 条数据 → {}", readings.len(), nearest);
            println!("  JSON: {} bytes", report.len());

            // TODO: 实际 HTTP POST
            // let url = format!("http://{}.valuehub.io:7915/api/edge/report", nearest);
            // http_post(&url, &report);

            led.set_low(); // 闪烁指示
            delay.delay_ms(100);
            led.set_high();

            sensors.clear();
        }

        // 睡眠
        println!("  睡眠 {}s...\n", COLLECT_INTERVAL_SECS);
        delay.delay_ms(COLLECT_INTERVAL_SECS * 1000);
    }
}

/// 读取 DHT22 温湿度传感器
fn read_dht22(pin: &Input, delay: &Delay) -> Result<(f64, f64), &'static str> {
    // TODO: 实现真实的 DHT22 协议
    // 当前返回模拟数据
    let temp = 25.0 + (rand_value() % 10) as f64 * 0.5 - 2.5;
    let humi = 50.0 + (rand_value() % 20) as f64 * 0.5 - 5.0;
    Ok((temp, humi))
}

/// 读取 GPS 数据
fn read_gps(uart: &mut Uart<esp_hal::peripherals::UART1>) -> Option<(f64, f64)> {
    // TODO: 实现 NMEA 协议解析
    // 当前返回 None（使用默认坐标）
    None
}

/// 简单随机数（用于模拟）
fn rand_value() -> u64 {
    // ESP32 上用 Rng 硬件
    42 // 占位
}

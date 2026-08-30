# OntoDB Edge for ESP32

超轻量级 IoT 数据采集器，专为 ESP32 设计。

## 特点

- `#![no_std]` + `alloc`，内存占用 <100KB
- 无 tokio 依赖，同步执行
- GPS + 传感器数据采集
- HTTP 自动上报到 ValueHub
- 地理路由（自动匹配最近节点）

## 支持的传感器

| 传感器 | 接口 | 数据 |
|--------|------|------|
| DHT22 | GPIO | 温度、湿度 |
| BMP280 | I2C | 气压、温度 |
| GPS | UART | 经纬度 |
| PM2.5 | UART | 颗粒物浓度 |
| 压力传感器 | ADC | 压力值 |

## 快速开始

```bash
# 安装 ESP32 工具链
cargo install espup
espup install

# 编译
cargo build --release --target xtensa-esp32-none-elf

# 烧录
espflash flash target/xtensa-esp32-none-elf/release/esp32-demo
```

## 示例代码

```rust
use onto_edge_esp32::{SensorCollector, GpsCollector, DataReporter, GeoRouter};

fn main() {
    let mut sensors = SensorCollector::new();
    let mut gps = GpsCollector::new();
    let reporter = DataReporter::new("device_001", "http://hub:7915");
    let router = GeoRouter::new();

    // GPS 定位
    gps.record(34.7466, 113.6253);
    let node = router.find_nearest(34.7466, 113.6253);

    // 采集数据
    sensors.record("temp", "temperature", 25.3, "°C");
    sensors.record("humi", "humidity", 65.2, "%");

    // 上报
    let report = reporter.build_report(sensors.readings(), gps.location());
    // http_post(node_url, report);
}
```

## 部署架构

```
ESP32 设备
    │
    ├─ 采集传感器数据
    ├─ GPS 定位
    ├─ 自动匹配最近节点
    └─ HTTP 上报到 ValueHub

ValueHub 区域节点
    │
    ├─ 接收数据
    ├─ 本地存储
    └─ 上报元数据到总部
```

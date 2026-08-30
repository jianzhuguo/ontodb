# Edge & IoT

OntoDB supports deployment on edge devices and IoT sensors.

## Architecture

```
┌─────────────────────────────────────────┐
│              Cloud (OntoDB Server)       │
│  Full-featured: SQL, vectors, graph      │
│  Aggregation, analytics, ML              │
└──────────────────┬──────────────────────┘
                   │ Sync
┌──────────────────┴──────────────────────┐
│              Edge (OntoDB Edge)          │
│  Lightweight: local storage, filtering   │
│  Offline operation, sync when connected  │
└──────────────────┬──────────────────────┘
                   │ Collect
┌──────────────────┴──────────────────────┐
│              IoT Devices                 │
│  Sensors, actuators, microcontrollers    │
└─────────────────────────────────────────┘
```

## OntoDB Edge

Lightweight runtime for edge devices.

### Features

| Feature | Description |
|---------|-------------|
| Local storage | Store data locally when offline |
| Data collection | Collect data from sensors |
| Data reporting | Sync to central server |
| Geo support | Device geolocation |
| Offline operation | Continue working without network |
| Auto-sync | Sync when connection restored |

### Supported Platforms

| Platform | Binary | Description |
|----------|--------|-------------|
| Linux x86_64 | `onto-edge` | Standard Linux edge device |
| Linux ARM64 | `onto-edge-arm64` | Raspberry Pi, Jetson |
| ESP32 | `onto-edge-esp32` | Microcontroller |

### ESP32 Support

Ultra-lightweight IoT data collector for ESP32 microcontrollers.

**Features:**
- Minimal memory footprint
- WiFi connectivity
- Sensor data collection
- Batch upload to central server
- Deep sleep support

**Configuration:**

```rust
// ESP32 configuration
let config = EdgeConfig {
    wifi_ssid: "MyNetwork",
    wifi_password: "password",
    server_url: "http://192.168.1.100:7912",
    collect_interval_ms: 5000,
    batch_size: 10,
};
```

## Data Flow

### Collection

```rust
// Edge device collects sensor data
let data = SensorData {
    temperature: 25.5,
    humidity: 60.0,
    timestamp: chrono::Utc::now().timestamp(),
};

// Store locally
edge.store("sensors", data)?;

// Report to central server (when connected)
edge.report("sensors", data)?;
```

### Sync

```rust
// Sync local data to central server
edge.sync().await?;

// Sync with conflict resolution
edge.sync_with_strategy(ConflictResolution::LastWriteWins).await?;
```

## Use Cases

| Use Case | Description |
|----------|-------------|
| Factory sensors | Collect machine data, sync to cloud |
| Smart buildings | Temperature, occupancy, energy monitoring |
| Vehicles | Telemetry data collection |
| Agriculture | Soil moisture, weather sensors |
| Healthcare | Patient monitoring devices |

## Offline Operation

OntoDB Edge continues working when network is unavailable:

1. **Store locally** — Data saved to local storage
2. **Queue changes** — Modifications queued for sync
3. **Auto-reconnect** — Detects network restoration
4. **Sync on reconnect** — Uploads queued changes

## Security

Edge devices authenticate with API keys:

```rust
let config = EdgeConfig {
    server_url: "http://192.168.1.100:7912",
    api_key: "sk-edge-device-001",
    // ...
};
```

## Performance

| Metric | Value |
|--------|-------|
| Memory usage | < 50MB |
| Startup time | < 100ms |
| Write latency | < 1ms (local) |
| Sync throughput | 1000+ records/sec |

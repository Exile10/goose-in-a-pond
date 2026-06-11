# End-to-End Workflow Example: Weather Feature

This guide walks through implementing a new feature — **Getting the Weather** — using the Hexagonal Architecture. The weather feature is already implemented; this doc explains the pattern so you can follow it for new features.

> **Note:** `pond-builder` (the code-generation CLI) has been removed. Follow these steps manually.

---

## The Strategy

We will implement a **Driven Port** (the interface for weather), a **Service** (the logic that uses it), and an **Adapter** (the actual API call). This keeps the `pond-core` brain free of HTTP/API details.

---

## Step 1: Create the Port

The Port is the contract. It lives in `pond-core`.

**File:** `crates/pond-core/src/mcp/ports/weather.rs`

```rust
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait WeatherProvider: Send + Sync {
    async fn get_weather(&self, lat: f64, lon: f64) -> Result<WeatherData>;
}
```

Register it:

```rust
// crates/pond-core/src/mcp/ports/mod.rs
pub mod weather;
```

---

## Step 2: Define Domain Types

**File:** `crates/pond-core/src/mcp/domain/weather.rs` (or within `pond-adapters-weather` if the type is adapter-specific)

```rust
pub struct WeatherData {
    pub temperature_c: f32,
    pub condition: String,
    pub humidity_pct: u8,
}
```

> In the actual implementation, `WeatherData` and `WeatherProvider` live in `crates/pond-adapters-weather/src/` because weather is only ever exposed via MCP tools, not core domain logic. Place types in `pond-core` when multiple adapters or core services consume them.

---

## Step 3: Write a Mock and Tests (TDD first)

**File:** `crates/pond-core/src/mcp/mocks/mock_weather.rs`

```rust
use async_trait::async_trait;
use anyhow::Result;
use crate::mcp::ports::weather::{WeatherProvider, WeatherData};

pub struct MockWeather;

#[async_trait]
impl WeatherProvider for MockWeather {
    async fn get_weather(&self, _lat: f64, _lon: f64) -> Result<WeatherData> {
        Ok(WeatherData { temperature_c: 22.0, condition: "Sunny".into(), humidity_pct: 40 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn mock_returns_data() {
        let w = MockWeather;
        let data = w.get_weather(0.0, 0.0).await.unwrap();
        assert_eq!(data.condition, "Sunny");
    }
}
```

Run quickly (no Goose compilation):

```bash
cargo test -p pond-core
```

---

## Step 4: Implement the Adapter

Create a new adapter crate (or add to an existing one):

**File:** `crates/pond-adapters-weather/src/lib.rs`

```rust
pub struct OpenMeteoWeatherAdapter {
    client: reqwest::Client,
    cache: tokio::sync::Mutex<Option<(Instant, WeatherData)>>,
    ttl: Duration,
}

#[async_trait]
impl WeatherProvider for OpenMeteoWeatherAdapter {
    async fn get_weather(&self, lat: f64, lon: f64) -> Result<WeatherData> {
        // Check cache, then fetch from Open-Meteo free API
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current_weather=true"
        );
        // ... parse response ...
    }
}
```

Add to `Cargo.toml` workspace members, add `reqwest` as a workspace dep if not already present.

---

## Step 5: Wire into the Server

In `crates/pond-server/src/main.rs`:

```rust
// 1. Build the adapter (only if weather is enabled in settings)
let weather: Option<Arc<dyn WeatherProvider>> = if settings.weather_enabled {
    Some(Arc::new(OpenMeteoWeatherAdapter::new(Duration::from_secs(900))))
} else {
    None
};

// 2. Pass to GIAP MCP service handles (exposed to the LLM as a tool)
init_giap_services(Arc::new(GiapServiceHandles { weather, device_registry, scheduler }));
```

Weather is **not** in `AppState` — it goes into `GiapServiceHandles` and reaches the LLM via the `giap__get_current_weather` MCP tool.

---

## Summary

1. **Port** defines *what* can be done (`WeatherProvider` trait in `pond-core`).
2. **Mock** enables fast tests without HTTP (`MockWeather` in `pond-core`).
3. **Adapter** does the real work (`OpenMeteoWeatherAdapter` in `pond-adapters-weather`).
4. **Server** wires it all together and passes to MCP or `AppState`.

By following this flow you can swap Open-Meteo for any other provider by implementing `WeatherProvider` — zero changes to `pond-core`.

//! `OpenMeteoWeatherAdapter` — fetches current weather from the free
//! [Open-Meteo](https://open-meteo.com) API (no API key required).
//!
//! Results are cached for `cache_ttl` (default 15 minutes) so every chat
//! message doesn't trigger an upstream HTTP call.
//!
//! **API used:**
//! ```text
//! GET https://api.open-meteo.com/v1/forecast
//!   ?latitude={lat}&longitude={lon}
//!   &current=temperature_2m,relative_humidity_2m,apparent_temperature,
//!            precipitation,weather_code,wind_speed_10m,wind_direction_10m
//!   &temperature_unit=celsius&wind_speed_unit=kmh&precipitation_unit=mm
//! ```

use crate::wmo;
use crate::{WeatherData, WeatherProvider};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ── Response shape ────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct ApiResponse {
    current: CurrentFields,
}

#[derive(serde::Deserialize)]
struct CurrentFields {
    temperature_2m:          f64,
    relative_humidity_2m:    u32,
    apparent_temperature:    f64,
    precipitation:           f64,
    weather_code:            u32,
    wind_speed_10m:          f64,
    wind_direction_10m:      u32,
}

// ── Adapter ───────────────────────────────────────────────────────────────────

struct CacheEntry {
    data:     WeatherData,
    fetched:  Instant,
}

pub struct OpenMeteoWeatherAdapter {
    client:        reqwest::Client,
    latitude:      f64,
    longitude:     f64,
    location_name: String,
    base_url:      String,
    cache_ttl:     Duration,
    cache:         Mutex<Option<CacheEntry>>,
}

impl OpenMeteoWeatherAdapter {
    /// Create with real Open-Meteo endpoint.
    pub fn new(latitude: f64, longitude: f64, location_name: impl Into<String>) -> Self {
        Self::with_base_url(
            latitude,
            longitude,
            location_name,
            "https://api.open-meteo.com",
        )
    }

    /// Create pointing at a custom base URL (used in tests with wiremock).
    pub fn with_base_url(
        latitude: f64,
        longitude: f64,
        location_name: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            latitude,
            longitude,
            location_name: location_name.into(),
            base_url: base_url.into(),
            cache_ttl: Duration::from_secs(15 * 60),
            cache: Mutex::new(None),
        }
    }

    async fn fetch_fresh(&self) -> Result<WeatherData> {
        let url = format!(
            "{}/v1/forecast\
             ?latitude={}&longitude={}\
             &current=temperature_2m,relative_humidity_2m,apparent_temperature,\
             precipitation,weather_code,wind_speed_10m,wind_direction_10m\
             &temperature_unit=celsius&wind_speed_unit=kmh&precipitation_unit=mm",
            self.base_url, self.latitude, self.longitude,
        );

        let resp = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("weather API request failed")?
            .error_for_status()
            .context("weather API returned error status")?;

        let api: ApiResponse = resp.json().await.context("failed to parse weather response")?;
        let c = api.current;

        Ok(WeatherData {
            temperature_c:      c.temperature_2m,
            feels_like_c:       c.apparent_temperature,
            humidity_pct:       c.relative_humidity_2m,
            description:        wmo::describe(c.weather_code).to_string(),
            wind_speed_kmh:     c.wind_speed_10m,
            wind_direction_deg: c.wind_direction_10m,
            precipitation_mm:   c.precipitation,
            location_name:      self.location_name.clone(),
            fetched_at:         Utc::now(),
        })
    }
}

#[async_trait]
impl WeatherProvider for OpenMeteoWeatherAdapter {
    async fn current(&self) -> Result<WeatherData> {
        // Check cache (lock is held briefly for reads, not across await)
        let cached = {
            let guard = self.cache.lock().unwrap();
            guard.as_ref().and_then(|e| {
                if e.fetched.elapsed() < self.cache_ttl {
                    Some(e.data.clone())
                } else {
                    None
                }
            })
        };

        if let Some(data) = cached {
            tracing::debug!("weather: serving from cache (location={})", self.location_name);
            return Ok(data);
        }

        tracing::debug!("weather: fetching from Open-Meteo (lat={}, lon={})", self.latitude, self.longitude);
        let data = self.fetch_fresh().await?;

        {
            let mut guard = self.cache.lock().unwrap();
            *guard = Some(CacheEntry { data: data.clone(), fetched: Instant::now() });
        }

        Ok(data)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_response() -> serde_json::Value {
        serde_json::json!({
            "current": {
                "temperature_2m": 24.3,
                "relative_humidity_2m": 68,
                "apparent_temperature": 25.1,
                "precipitation": 0.2,
                "weather_code": 2,
                "wind_speed_10m": 13.5,
                "wind_direction_10m": 180
            }
        })
    }

    async fn make_adapter(server: &MockServer) -> OpenMeteoWeatherAdapter {
        OpenMeteoWeatherAdapter::with_base_url(-1.286, 36.817, "Nairobi, KE", server.uri())
    }

    #[tokio::test]
    async fn fetches_and_parses_weather() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_response()))
            .mount(&server)
            .await;

        let adapter = make_adapter(&server).await;
        let data = adapter.current().await.unwrap();

        assert_eq!(data.temperature_c, 24.3);
        assert_eq!(data.humidity_pct, 68);
        assert_eq!(data.description, "Partly cloudy");
        assert_eq!(data.wind_speed_kmh, 13.5);
        assert_eq!(data.location_name, "Nairobi, KE");
    }

    #[tokio::test]
    async fn context_block_is_formatted_correctly() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_response()))
            .mount(&server)
            .await;

        let data = make_adapter(&server).await.current().await.unwrap();
        let block = data.as_context_block();

        assert!(block.contains("Nairobi, KE"));
        assert!(block.contains("24.3°C"));
        assert!(block.contains("Partly cloudy"));
        assert!(block.contains("Humidity: 68%"));
    }

    #[tokio::test]
    async fn cache_serves_second_call_without_second_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_response()))
            .expect(1) // only ONE real request
            .mount(&server)
            .await;

        let adapter = make_adapter(&server).await;
        adapter.current().await.unwrap();
        adapter.current().await.unwrap(); // served from cache

        server.verify().await; // asserts exactly 1 HTTP call
    }

    #[tokio::test]
    async fn api_error_returns_err() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let adapter = make_adapter(&server).await;
        assert!(adapter.current().await.is_err());
    }
}

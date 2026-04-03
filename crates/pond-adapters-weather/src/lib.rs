mod open_meteo;
mod wmo;

pub use open_meteo::OpenMeteoWeatherAdapter;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Current weather snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherData {
    pub temperature_c: f64,
    pub feels_like_c: f64,
    pub humidity_pct: u32,
    pub description: String,
    pub wind_speed_kmh: f64,
    pub wind_direction_deg: u32,
    pub precipitation_mm: f64,
    pub location_name: String,
    pub fetched_at: DateTime<Utc>,
}

impl WeatherData {
    /// Returns a compact context block ready to be appended to the LLM system prompt.
    pub fn as_context_block(&self) -> String {
        format!(
            "[Current Weather — {}]\n{} | {:.1}°C (feels like {:.1}°C) | Humidity: {}% | Wind: {:.0} km/h | Precip: {:.1} mm",
            self.location_name,
            self.description,
            self.temperature_c,
            self.feels_like_c,
            self.humidity_pct,
            self.wind_speed_kmh,
            self.precipitation_mm,
        )
    }
}

/// Port for fetching current weather conditions.
/// Implementations should cache results for ~15 minutes.
#[async_trait]
pub trait WeatherProvider: Send + Sync {
    async fn current(&self) -> Result<WeatherData>;
}

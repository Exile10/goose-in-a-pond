# End-to-End Workflow Example: Weather Feature

This guide walks you through implementing a new feature—**Getting the Weather**—using the `pond-builder` and our Hexagonal Architecture.

---

## 🏗️ 1. The Strategy
We will implement a **Driven Port** (the interface for weather), a **Service** (the logic that uses the weather), and an **Adapter** (the actual API call).

---

## 🛠️ Step 1: Create the Port
The Port is the contract. It lives in `pond-core`.

```powershell
cargo run -p pond-builder -- make:port Weather --type driven
```

**Result**: Created `crates/pond-core/src/ports/weather.rs`.

**Next**: Add `pub mod weather;` to `crates/pond-core/src/ports/mod.rs`.

---

## 🧠 Step 2: Define Domain Models
Update `crates/pond-core/src/domain/mod.rs` (or create a new domain file) to include weather data:

```rust
pub struct WeatherReport {
    pub temperature: f32,
    pub condition: String,
}
```

---

## 🧪 Step 3: Implement Logic (TDD)
Create the Service that orchestrates the logic.

```powershell
cargo run -p pond-builder -- make:service weather
```

**Result**: Created `crates/pond-core/src/services/weather.rs`.

### Write a Unit Test (Mocking)
In `crates/pond-core/src/services/weather.rs`, write a test that ensures the service correctly processes weather data without calling a real API.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // Mock Weather Port implementation here...
}
```

---

## 🔌 Step 4: Implement the Adapter
Now we implement the "Real World" connection in `pond-infra`.

```powershell
cargo run -p pond-builder -- make:adapter open_weather --for Weather --target-crate crates/pond-infra
```

**Result**: Created `crates/pond-infra/src/open_weather_weather.rs`.

**Next**: Implement the `Weather` trait in this file, making the actual HTTP calls using `reqwest`.

---

## 🔌 Step 5: Wiring in the Server
Finally, wire the Adapter to the Service in `crates/pond-server/src/main.rs`.

```rust
// 1. Initialize Adapter
let weather_adapter = OpenWeatherWeather::new(api_key);

// 2. Inject into Service
let weather_service = WeatherService::new(Box::new(weather_adapter));

// 3. Add to Application State
let state = AppState { weather_service };
```

---

## ✅ Summary
1. **Port** defines **What** can be done.
2. **Service** defines **How** the assistant thinks about it.
3. **Adapter** defines **Where** the data actually comes from.
4. **Server** wires the **Plugin** to the **Core**.

By following this flow, you can swap out OpenWeather for any other provider without touching a single line of your business logic!

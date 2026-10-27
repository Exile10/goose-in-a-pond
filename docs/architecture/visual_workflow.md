# Visual workflow: the weather feature

One feature end to end, as diagrams. Weather is small and touches every layer: an adapter for an
external service, a REST route, an MCP tool the model can call, and wiring in the composition
root.

## Components

```mermaid
flowchart TD
    subgraph External
        OM["Open-Meteo API<br/>(forecast + geocoding, no key)"]
    end

    subgraph Driving["Driving side"]
        Route["pond-api: GET /api/v1/weather<br/>(get_weather)"]
        Tool["pond-mcp-server: giap-weather<br/>get_current_weather, get_weather_forecast"]
    end

    subgraph Port["The port"]
        Trait["WeatherProvider trait<br/>(pond-adapters-weather)"]
    end

    subgraph Driven["Driven adapter"]
        Adapter["OpenMeteoWeatherAdapter + Geocoder<br/>15-minute cache"]
        Egress["pond_core egress gate<br/>check_egress / record_egress"]
    end

    Root["pond-server main.rs<br/>composition root"]

    Route -- calls --> Trait
    Tool -- calls --> Trait
    Adapter -- implements --> Trait
    Adapter --> Egress --> OM
    Root -- "builds from the resolved location" --> Adapter
    Root -- "AppState.weather_provider" --> Route
    Root -- "init_weather_deps" --> Tool
```

The trait lives in the adapter crate, not in `pond-core`: `WeatherProvider` and its data types
(`WeatherData`, `ForecastData`) are declared in `crates/pond-adapters-weather/src/lib.rs`, so
`pond-api`, `pond-mcp-server` and `pond-adapters-goose` depend on that crate. It is the one
place this feature departs from the dependency rule in [overview.md](./overview.md).

## The model asks for the weather

```mermaid
sequenceDiagram
    participant U as User
    participant H as pond-api drive_turn
    participant GA as GooseAdapter
    participant G as Goose agent loop
    participant W as giap-weather (WeatherMcpServer)
    participant A as OpenMeteoWeatherAdapter
    participant OM as Open-Meteo

    U->>H: "Will it rain tomorrow?" (POST /api/v1/chat/stream)
    H->>GA: Agent::chat_stream
    GA->>G: reply (tools include giap-weather when ext_weather_enabled)
    G->>W: get_weather_forecast {location?}
    W->>A: forecast(days) or forecast_for(location, days)
    alt cached within 15 minutes
        A-->>W: ForecastData
    else
        A->>A: check_egress (refused when the pond is offline)
        A->>OM: HTTPS GET
        OM-->>A: JSON
        A-->>W: ForecastData
    end
    W-->>G: CallToolResult (text + UI data)
    G-->>GA: answer tokens
    GA-->>H: AgentStreamEvent::ToolResult, Text
    H-->>U: SSE tool_result, text, done
```

The tool returns an error result, never an invented forecast, when no location is configured.

## The dashboard asks for the weather

`GET /api/v1/weather` (`get_weather` in `crates/pond-api/src/routes.rs`) calls
`WeatherProvider::current` and `forecast(4)` directly, without the model. It answers
`{"enabled": false}` when `AppState.weather_provider` is `None`, and 502 when the upstream call
fails.

## Wiring

```mermaid
graph BT
    Settings["Settings: weather_enabled, location,<br/>ext_weather_enabled"] --> Root
    Root["pond-server main.rs"] -->|"OpenMeteoWeatherAdapter::new(lat, lon, loc)"| Adapter["shared WeatherProvider"]
    Adapter --> AppState["pond-api AppState.weather_provider"]
    Adapter --> Reg["register_giap_extensions<br/>(pond-adapters-goose/src/giap_registration.rs)"]
    Reg -->|"init_weather_deps + register_builtin_extension"| Ext["giap-weather builtin MCP server"]
```

1. When `weather_enabled` is on and `pond_core::user_data::services::location::resolve` yields a
   weather target (coordinates, or a place name the adapter geocodes), `pond-server` builds
   `OpenMeteoWeatherAdapter`; otherwise the provider is `None`.
2. The same `Arc<dyn WeatherProvider>` goes into `AppState` for the REST route and into
   `register_giap_extensions`, which calls `pond_mcp_server::init_weather_deps` and registers
   `spawn_weather_server` with Goose as the `giap-weather` builtin when `ext_weather_enabled` is on.
3. Goose starts the builtin in-process over a duplex stream when a session first loads its
   extensions (see [agent_pipeline.md](./agent_pipeline.md)).

## Takeaways

- Driving adapters (the REST route, the MCP server) depend on the trait, never on Open-Meteo.
- The driven adapter owns caching, geocoding and the egress gate, so both callers get them.
- Swapping the weather source means a new `WeatherProvider` implementation and one line in
  `pond-server`; neither caller changes.

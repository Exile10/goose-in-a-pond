# Visual Architecture & Example Flow

This document provides a visual guide to the **Goose-in-a-Pond** architecture using the **Weather Feature** as a concrete implementation example.

---

## 🗺️ 1. Flowchart: Component Relationships

This flowchart illustrates how the logic is decoupled. Notice how the **Core** knows about the **Port**, but never about the **Adapter**.

```mermaid
flowchart TD
    subgraph "External Integration"
        OpenWeatherAPI["OpenWeather (HTTP/JSON)"]
    end

    subgraph "Driving Adapters (Inputs)"
        Server["pond-server (Axum Host)"]
    end

    subgraph "The Pure Core (pond-core)"
        Service["WeatherService (Orchestrator)"]
        Port["WeatherPort (Trait)"]
        Domain["WeatherReport (Domain Model)"]
    end

    subgraph "Driven Adapters (Outputs)"
        Adapter["OpenWeatherAdapter (pond-infra)"]
    end

    %% Flow of events
    Server -- "Calls" --> Service
    Service -- "Uses" --> Port
    Service -- "Returns" --> Domain
    Adapter -- "Implements" --> Port
    Adapter -- "Fetches from" --> OpenWeatherAPI

    %% Styling
    style Service fill:#f9f,stroke:#333,stroke-width:2px
    style Port fill:#bbf,stroke:#333,stroke-width:4px
    style Adapter fill:#dfd,stroke:#333,stroke-width:2px
```

---

## ⚡ 2. Sequence Diagram: Implementation Lifecycle

This diagram shows the "Time-Based" flow of data during a single weather request.

```mermaid
sequenceDiagram
    participant UI as User Interface
    participant S as pond-server
    participant C as WeatherService (Core)
    participant P as WeatherPort (Trait)
    participant A as OpenWeatherAdapter (Infra)
    participant API as External Weather API

    UI->>S: GET /api/weather?city=London
    S->>C: get_weather("London")
    
    Note over C,P: Core uses the Port interface
    C->>P: fetch_current_weather("London")
    
    Note over P,A: The implementation is injected at runtime
    P->>A: fetch_current_weather("London")
    
    A->>API: HTTP GET api.openweathermap.org/...
    API-->>A: 200 OK { temperature: 15 }
    
    A-->>P: WeatherReport { temp: 15 }
    P-->>C: WeatherReport { temp: 15 }
    
    Note over C: Core applies business rules (e.g. converting C to F)
    
    C-->>S: Return Report
    S-->>UI: JSON Response
```

---

## 🧩 3. Dependency Injection (Wiring)

In Hexagonal Architecture, the "Wiring" happens in the main entry point (`pond-server`).

### The Blueprint
1. **The Service** asks for a `Box<dyn WeatherPort>`.
2. **The Server** builds the `OpenWeatherAdapter`.
3. **The Server** "plugs" the Adapter into the Service.

### Visualized Wiring
```mermaid
graph BT
    S[WeatherService] --> |Requires| P(WeatherPort Trait)
    A[OpenWeatherAdapter] -.-> |Satisfies| P
    Sub[App Startup] --> |Wires| A
    Sub --> |Injects into| S
```

---

## 📝 Key Takeaways
- **Driven (Output)**: The Core defines the trait; the infra implements it.
- **Driving (Input)**: The server defines the endpoint; it calls the Core.
- **Direction of Dependencies**: All arrows point towards the **Core** or the **Port Traits** defined in the Core.

# Agent Loop Diagrams

Trace of a prompt like `"Who is John Cena, How Old is he and remember I love him!"` through the full GIAP pipeline.

---

## 1. High-Level Sequence Diagram

```mermaid
sequenceDiagram
    participant U as User
    participant FE as React Chat
    participant API as PondApiClient
    participant AX as Axum Router
    participant TA as ToolAgent
    participant DB as SQLite
    participant GA as GooseAdapter
    participant LLM as LLM Provider
    participant MCP as MCP Tools
    participant ME as MemoryExtractor
    participant AR as AnswerReviewer

    U->>FE: Type message + Send
    FE->>FE: Create user msg + empty agent msg
    FE->>API: chatStream message sessionId
    API->>AX: POST /api/v1/chat/stream SSE

    Note over AX: Auth + Rate Limit + CORS

    AX->>AX: Fast-path check - not greeting - skip
    AX->>AX: Build system prompt

    par Parallel Prep
        AX->>TA: process message
        TA->>LLM: classify via complete
        LLM-->>TA: needs_tool true tool wikipedia
        TA->>MCP: try_tool_agent wikipedia msg
        MCP-->>TA: John Cena article compacted
        TA-->>AX: augmented message with tool context
    and
        AX->>DB: persist user message
        DB-->>AX: ok
    end

    AX->>GA: chat_stream AgentRequest

    par GooseAdapter Prep
        GA->>DB: fetch prompt template
        GA->>DB: fetch devices
        GA->>DB: fetch prompt extras and skills
        GA->>DB: fetch recent memories
        GA->>DB: fetch relevant memories
    end

    GA->>GA: Build partitioned system prompt
    GA->>GA: Inject memories into prompt
    GA->>LLM: agent.reply augmented_msg

    loop Token Streaming
        LLM-->>GA: token
        GA-->>AX: AgentStreamEvent Text
        AX->>AX: ThoughtFilter strip think tags
        AX-->>API: SSE text event
        API-->>FE: yield ChatEvent
        FE->>FE: Append token to agent bubble
    end

    LLM-->>GA: stream complete
    GA-->>AX: AgentStreamEvent Done

    opt Answer Review
        AX->>AR: review question answer context
        AR->>LLM: critic prompt
        LLM-->>AR: score and feedback
        AR-->>AX: ReviewResult
        AX-->>FE: SSE review_status event
    end

    par Post-Processing
        AX->>ME: spawn extract user_msg assistant_resp
        ME->>LLM: extraction prompt
        LLM-->>ME: User loves John Cena
        ME->>DB: save memory fragment
    and
        AX->>DB: persist assistant response
    and
        AX->>DB: increment_usage tokens
    end

    AX-->>API: SSE done event
    API-->>FE: yield done
    FE->>FE: streaming false - render final
```

---

## 2. Phase Flowchart

```mermaid
flowchart TD
    START(["User sends message"])

    subgraph FE["Phase 0 - Frontend"]
        F1["Trim input and clear textarea"]
        F2["Create user Message object"]
        F3["Create empty agent Message"]
        F4["Call api.chatStream"]
        F1 --> F2 --> F3 --> F4
    end

    subgraph HTTP["Phase 1 - HTTP Layer"]
        H1["CORS Layer"]
        H2["Rate Limiter 600req/60s"]
        H3["Auth Middleware - Bearer token"]
        H4["SSE Semaphore - acquire permit"]
        H1 --> H2 --> H3 --> H4
    end

    subgraph FAST["Phase 2 - Fast Path"]
        FP{"Is trivial greeting?"}
        FP -->|Yes| FPR["Return canned response"]
        FP -->|No| CONT["Continue to full pipeline"]
    end

    subgraph PROMPT["Phase 3 - System Prompt"]
        P1["Load Settings"]
        P2["Load Profile Context"]
        P3["Load template or build default"]
        P4["Append MCP memory instructions"]
        P5["Append extension tool guidance"]
        P1 --> P2 --> P3 --> P4 --> P5
    end

    subgraph PARALLEL["Phase 4 - Parallel Prep via tokio join"]
        TC1["ToolAgent: LLM classify message"]
        TC2["Result: needs_tool=true tool=wikipedia"]
        DB1["Persist: INSERT user message"]
        TC1 --> TC2
    end

    subgraph TOOLS["Phase 5 - Tool Execution"]
        T1{"Check tool cache"}
        T1 -->|HIT| T2["Return cached result"]
        T1 -->|MISS| T3["pond_mcp_server try_tool_agent"]
        T3 --> T4["Wikipedia: fetch John Cena article"]
        T3 --> T5["save_memory: store loves John Cena"]
        T4 --> T6["Compact tool output"]
        T6 --> T7["format_tool_context - wrap in TOOL CONTEXT block"]
        T5 --> T7
        T2 --> T7
    end

    subgraph GOOSE["Phase 7 - GooseAdapter"]
        G1["resolve_goose_session - map session ID"]
        G2["tokio join: template devices extras skills memories"]
        G3["Partition prompt: static prefix + dynamic suffix"]
        G4["Inject memories sorted by importance"]
        G5["Inject upcoming schedules"]
        G6["ensure_provider_current"]
        G7["Remove all Goose extensions - tool-free mode"]
        G8["agent.reply - start inference stream"]
        G1 --> G2 --> G3 --> G4 --> G5 --> G6 --> G7 --> G8
    end

    subgraph STREAM["Phase 8 - SSE Streaming"]
        S1["Receive token from Goose"]
        S2["ThoughtFilter.push token"]
        S3{"Visible text?"}
        S3 -->|Yes| S4["Emit SSE text event"]
        S3 -->|No| S5["Discard thinking content"]
        S4 --> S6["Frontend appends to agent bubble"]
        S1 --> S2 --> S3
    end

    subgraph REVIEW["Phase 9 - Answer Review"]
        R1{"review_mode setting?"}
        R1 -->|off| R3["Skip review"]
        R1 -->|on or auto| R2["Send question+answer to critic LLM"]
        R2 --> R4{"Score >= threshold?"}
        R4 -->|Yes| R5["Emit: Answer verified score 4/5"]
        R4 -->|No| R6["Revise answer and emit review_revision"]
    end

    subgraph POST["Phase 10 - Post-Processing"]
        M1["MemoryExtractor: LLM extract facts"]
        M2["Fact: User loves John Cena - Preference 0.7"]
        M3["Save to memory_repo"]
        P6["Save assistant response to DB"]
        P7["increment_usage tokens"]
        P8["Record telemetry TTFT latency model"]
        P9["Context monitor check fill rate"]
        M1 --> M2 --> M3
    end

    subgraph DONE["Phase 11 - Done"]
        D1["Emit SSE done event with session_id and usage"]
        D2["Release SSE semaphore permit"]
        D3["Frontend: streaming=false render final"]
    end

    START --> FE --> HTTP --> FAST
    CONT --> PROMPT --> PARALLEL --> TOOLS --> GOOSE --> STREAM
    STREAM --> REVIEW --> POST --> DONE
```

---

## 3. Concurrency Model

```mermaid
gantt
    title Timeline for a single chat turn
    dateFormat X
    axisFormat %L ms

    section Phase 4 Parallel Prep
    ToolAgent LLM classify     :active, tc, 0, 400
    Persist user msg SQLite    :active, db, 0, 10

    section Phase 5 Tool Exec
    Wikipedia fetch and compact :tool1, 400, 800
    save_memory MCP            :tool2, 400, 500
    format_tool_context        :fmt, 800, 820

    section Phase 7 Goose Prep
    tokio join template devices memories extras skills :gp, 820, 900
    Build partitioned prompt   :bp, 900, 920
    Inject memories            :im, 920, 930

    section Phase 8 LLM Streaming
    Goose agent.reply          :crit, stream, 930, 8000
    ThoughtFilter and SSE emit :active, sse, 1200, 8000

    section Phase 9 Review
    Critic LLM call            :review, 8000, 9500

    section Phase 10 Post-Processing
    Memory extraction spawned  :active, mem, 8000, 10000
    Persist assistant msg      :persist, 9500, 9510
    Token usage and telemetry  :telem, 9510, 9530

    section Done
    SSE done event             :milestone, done, 9530, 9530
```

---

## 4. Data Transformation Pipeline

```mermaid
flowchart LR
    subgraph INPUT
        RAW["Raw user input"]
    end

    subgraph CLASSIFY
        CL["LLM Classifier"]
        CL2["needs_tool=true tool=wikipedia"]
        CL --> CL2
    end

    subgraph AUGMENT
        AUG["Augmented message with TOOL CONTEXT block"]
    end

    subgraph PROMPT_ASM["System Prompt Assembly"]
        SP["Static prefix: identity personality tools"]
        DP["Dynamic suffix: date time profile"]
        MEM["Memory block: preferences identity facts"]
    end

    subgraph LLM_OUT["LLM Output"]
        TOKENS["Raw token stream with think tags"]
    end

    subgraph FILTER["ThoughtFilter"]
        CLEAN["Visible text only - tags stripped"]
    end

    subgraph REVIEW_STEP["Answer Review"]
        REVIEWED["Verified or revised answer"]
    end

    subgraph EXTRACT["Memory Extraction"]
        FACTS["Extracted: User loves John Cena"]
        FACTS2["segment=Preference importance=0.7 tier=Long"]
        FACTS --> FACTS2
    end

    RAW --> CL2 --> AUG
    AUG --> LLM_OUT
    SP --> LLM_OUT
    DP --> LLM_OUT
    MEM --> LLM_OUT
    LLM_OUT --> FILTER --> REVIEWED
    REVIEWED --> EXTRACT
```

---

## 5. Memory Lifecycle for this turn

```mermaid
stateDiagram-v2
    [*] --> UserSays: User sends message

    UserSays --> KeywordExtract: Extract keywords from message
    KeywordExtract --> ParallelFetch: john cena love

    state ParallelFetch {
        RecentMemories: Fetch recent memories by recency
        RelevantMemories: Fetch relevant memories by keyword match
    }

    ParallelFetch --> Merge: Deduplicate by ID
    Merge --> SortByImportance: Sort by importance score descending
    Merge --> BudgetTruncate: Apply CompactionProfile token budget

    BudgetTruncate --> InjectIntoPrompt: Extend system prompt with memories block

    state PostProcessing {
        ExtractFacts: MemoryExtractor analyzes turn
        ClassifySegment: auto_classify_segment via keywords
        SaveMemory: Store to memory_repo
        ExtractFacts --> ClassifySegment
        ClassifySegment --> SaveMemory
    }

    InjectIntoPrompt --> LLMInference
    LLMInference --> PostProcessing: After streaming completes

    SaveMemory --> Active: New memory state

    state MemoryLifecycle {
        Active --> Archived: importance below 0.15
        Archived --> Pruned: importance below 0.05
        Active --> Merged: Consolidation merges similar
    }

    Active --> [*]
```

---

## Source file locations

| Phase | Primary file | Line |
|-------|-------------|------|
| Frontend send | `pond-desktop/src/sections/Chat.tsx` | 225 |
| API client SSE | `pond-desktop/src/api/PondApiClient.ts` | 395 |
| chat_stream handler | `crates/pond-api/src/routes.rs` | 526 |
| Fast-path check | `crates/pond-api/src/routes.rs` | 560 |
| System prompt build | `crates/pond-api/src/routes.rs` | 618 |
| Parallel prep tokio join | `crates/pond-api/src/routes.rs` | 722 |
| ToolAgent classify | `crates/pond-server/src/main.rs` | 2117 |
| Tool execution | `crates/pond-server/src/main.rs` | 2184 |
| GooseAdapter chat_stream | `crates/pond-adapters-goose/src/goose_agent.rs` | 469 |
| Memory injection | `crates/pond-adapters-goose/src/goose_agent.rs` | 668 |
| ThoughtFilter | `crates/pond-api/src/thought_filter.rs` | 1 |
| SSE event loop | `crates/pond-api/src/routes.rs` | 871 |
| Answer review | `crates/pond-api/src/routes.rs` | 962 |
| Memory extraction | `crates/pond-api/src/routes.rs` | 1020 |
| Persist + telemetry | `crates/pond-api/src/routes.rs` | 1036 |
| Done event | `crates/pond-api/src/routes.rs` | 1133 |

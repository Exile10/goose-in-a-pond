pub mod chat;
pub mod compact_encoding;
pub mod context_budget;
pub mod context_compactor;
pub mod context_monitor;
pub mod fallback_provider;
pub mod fallback_voice_output;
pub mod fast_responder;
pub mod history_manager;
pub mod instant_activation;
pub mod marketplace;
pub mod memory_cleanup;
pub mod memory_consolidation;
pub mod memory_extraction;
pub mod memory_graph;
pub mod model_service;
pub mod oauth_providers;
pub mod onboarding;
pub mod print_output;
pub mod prompt_builder;
pub mod stdin_input;
pub mod telemetry;
pub mod thought_filter;
pub mod tool_cache;
pub mod tool_registry;

// Mock re-export shims. Gated identically to the quadrant `mocks` modules they
// forward to, so a plain (non-test, no `test-mocks`) build never compiles them.
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_agent;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_device_registry;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_memory;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_model_catalog_provider;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_model_downloader;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_model_repository;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_model_storage;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_profile;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_prompt_extra;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_prompt_template;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_provider;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_sensor;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_session;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_settings;
#[cfg(any(test, feature = "test-mocks"))]
pub mod mock_skill;

//! Private Pond Compute — trust-scoped P2P inference mesh (#132).
//!
//! Domain types and port traits only. Real transport/payment/ledger
//! implementations live in `pond-adapters-mesh-*` and `pond-infra`.

pub mod domain;
pub mod ports;
pub mod services;

#[cfg(any(test, feature = "test-mocks"))]
pub mod mocks;

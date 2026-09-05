//! Personal context streaming (PAI-8): a corpus of [`ContextItem`](domain::ContextItem)s, each
//! owned by one household member, classified, and governed by a retention window. P1 and P2 only:
//! no connector, no OAuth, no webhook receiver, no outbound request of any kind. That boundary is
//! written in `domain::SourceKind::availability` and enforced in `ingest::IngestPipeline::ingest`.

pub mod bus_ingest;
pub mod chunking;
pub mod domain;
pub mod index_maintenance;
pub mod ingest;
pub mod ports;
pub mod producer;
pub mod retention;
pub mod retrieval;
pub mod retrieval_service;
pub mod scope;
pub mod summary_indexing;
pub mod vector_index;

#[cfg(any(test, feature = "test-mocks"))]
pub mod mocks;

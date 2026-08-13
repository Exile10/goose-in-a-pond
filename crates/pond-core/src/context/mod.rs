//! Personal context streaming — PAI-8.
//!
//! The pond knows what you told it and what its sensors saw. It does not know
//! that your flight moved, that the landlord replied, or that the meeting you
//! are about to miss exists. This module is where the second kind of knowing
//! lives: a corpus of [`ContextItem`](domain::ContextItem)s, each owned by one
//! household member, each classified, each governed by a retention window.
//!
//! **P1 and P2 only.** What is here is the domain, the storage port, the ingest
//! pipeline for sources the pond already holds (sensor, camera, voice), and the
//! retrieval side. What is deliberately NOT here is any connector: no OAuth, no
//! webhook receiver, no outbound request of any kind. [`SourceKind::availability`]
//! is where that boundary is written down, and
//! [`IngestPipeline::ingest`](ingest::IngestPipeline::ingest) is where it is
//! enforced.
//!
//! # Reading order
//!
//! - [`domain`] — the types, and the three invariants the compiler holds.
//! - [`scope`] — which rows a [`ProfileScope`](crate::user_data::domain::profile::ProfileScope)
//!   may see. One predicate, two implementations, cross-checked.
//! - [`ports`] — the storage port. No default bodies, on purpose.
//! - [`ingest`] — the pipeline, and what it refuses.
//! - [`producer`] — the on-pond producer: the bus event to [`RawItem`] map, and
//!   the judgement about which readings are worth a durable row.
//! - [`bus_ingest`] — the one moving part: the producer joined to the pipeline,
//!   so wiring it in `pond-server` is a call rather than four policy decisions.
//! - [`retention`] — how long an item lives, and the `0 = forever` trap.
//! - [`retrieval`] — P2: ranking and the second `<system-context>` budget.
//!
//! [`SourceKind::availability`]: domain::SourceKind::availability
//! [`RawItem`]: ingest::RawItem

pub mod bus_ingest;
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

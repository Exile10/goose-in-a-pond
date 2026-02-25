//! Goose On The Go — Mobile Companion Adapters
//!
//! This crate implements the server-side adapters for handling requests
//! from the GOTG mobile app. It bridges the mobile app protocol with
//! the GIAP pond-core ports.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────┐          ┌──────────────────┐
//! │  GOTG Mobile App │  ◄─────► │  pond-adapters-   │
//! │  (iOS/Android)   │  HTTP    │  gotg             │
//! └──────────────────┘          │                   │
//!                               │  ┌─────────────┐ │
//!                               │  │ Handshake    │ │
//!                               │  │ Adapter      │─┼──► pond-core::ports::handshake
//!                               │  ├─────────────┤ │
//!                               │  │ Device       │ │
//!                               │  │ Adapter      │─┼──► pond-core::ports::device_registry
//!                               │  ├─────────────┤ │
//!                               │  │ Notification │ │
//!                               │  │ Adapter      │─┼──► pond-core::ports::notification
//!                               │  └─────────────┘ │
//!                               └──────────────────┘
//! ```
//!
//! # TODO
//! - [ ] Implement GotgHandshakeAdapter (GIAP ↔ GOTG authentication)
//! - [ ] Implement GotgDeviceAdapter (register/manage mobile device)
//! - [ ] Implement GotgNotificationAdapter (push events to mobile)
//! - [ ] Define the GOTG ↔ GIAP wire protocol (JSON over HTTP)
//! - [ ] Add mDNS/Bonjour discovery so GOTG can find the pond on LAN

pub mod handshake_adapter;
pub mod device_adapter;
pub mod notification_adapter;

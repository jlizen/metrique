// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Field tag markers consumed by [`OtelSink`](crate::OtelSink).
//!
//! Apply to entry fields via `#[metrics(field_tag(...))]` or a struct-level
//! `#[metrics(default_field_tag(...))]` to declare the OTel instrument kind
//! the sink should record observations against. The sink reads these tags
//! once per [`DescriptorId`](metrique_writer_core::DescriptorId) and dispatches
//! observations to the appropriate instrument.
//!
//! ```ignore
//! use metrique::unit_of_work::metrics;
//! use metrique_otel::tags::{Counter, Histogram};
//!
//! #[metrics(rename_all = "PascalCase")]
//! struct RequestMetrics {
//!     operation: String,
//!     #[metrics(field_tag(Counter))]   request_count: u64,
//!     #[metrics(field_tag(Histogram))] latency_ms: std::time::Duration,
//! }
//! ```

/// Tag for fields that record onto an OTel monotonic counter.
pub struct Counter;

/// Tag for fields that record onto an OTel up-down counter.
pub struct UpDownCounter;

/// Tag for fields that record onto an OTel histogram instrument.
pub struct Histogram;

/// Tag for fields that record onto an OTel asynchronous gauge.
pub struct Gauge;

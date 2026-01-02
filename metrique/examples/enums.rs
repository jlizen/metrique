// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Enum examples showing both value(string) and entry enums.
//!
//! This example demonstrates:
//! - Value enums with `#[metrics(value(string))]`
//! - Entry enums with tuple and struct variants
//! - Using enums with append_on_drop

use metrique::unit_of_work::metrics;
use metrique::writer::sink::VecEntrySink;

// Value enum - represents operation type as a string
#[metrics(value(string), rename_all = "snake_case")]
#[derive(Copy, Clone)]
enum OperationType {
    Read,
    Write,
    Delete,
}

#[metrics(subfield)]
struct ReadMetrics {
    bytes_read: usize,
    cache_hit: bool,
}

#[metrics(subfield)]
struct WriteMetrics {
    bytes_written: usize,
    fsync_required: bool,
}

// Entry enum - different fields per operation type
#[metrics]
enum OperationMetrics {
    Read(#[metrics(flatten)] ReadMetrics),
    Write(#[metrics(flatten)] WriteMetrics),
    Delete { key_count: usize },
}

#[metrics(rename_all = "PascalCase")]
struct RequestMetrics {
    #[metrics(sample_group)]
    operation: OperationType,

    #[metrics(flatten)]
    details: OperationMetrics,

    success: bool,
}

fn main() {
    let sink = VecEntrySink::new();

    // Read operation
    {
        RequestMetrics {
            operation: OperationType::Read,
            details: OperationMetrics::Read(ReadMetrics {
                bytes_read: 1024,
                cache_hit: true,
            }),
            success: true,
        }
        .append_on_drop(sink.clone());
    }

    // Write operation
    {
        RequestMetrics {
            operation: OperationType::Write,
            details: OperationMetrics::Write(WriteMetrics {
                bytes_written: 2048,
                fsync_required: true,
            }),
            success: true,
        }
        .append_on_drop(sink.clone());
    }

    // Delete operation (struct variant)
    {
        RequestMetrics {
            operation: OperationType::Delete,
            details: OperationMetrics::Delete { key_count: 5 },
            success: true,
        }
        .append_on_drop(sink.clone());
    }

    let entries = sink.drain();
    println!("Emitted {} metric entries", entries.len());

    for entry in entries {
        let test_entry = metrique::test_util::to_test_entry(&entry);
        println!("\nMetrics: {:#?}", test_entry.metrics);
    }
}

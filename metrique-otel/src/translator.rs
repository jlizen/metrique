// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::{any::TypeId, borrow::Cow, collections::HashMap};

use metrique_writer_core::{
    EntryConfig, MetricFlags, Observation, Unit, ValidationError,
    descriptor::DescriptorRef,
    entry::EntryWriter,
    value::{Distribution, Value, ValueWriter},
};
use opentelemetry::KeyValue;

use crate::{
    metrics::{InstrumentCache, InstrumentKind},
    tags,
};

/// Resolution of a single field's instrument kind, built once per
/// [`DescriptorId`](metrique_writer_core::descriptor::DescriptorId).
#[derive(Clone, Debug)]
pub(crate) struct FieldKind {
    pub(crate) kind: InstrumentKind,
}

/// Pre-resolved plan for one entry shape: how to handle each named field at
/// write time without walking the descriptor again. `kinds` maps the
/// runtime field name (the same string the macro/`Value` impl passes to
/// [`ValueWriter::metric`]) to the OTel instrument kind tagged on the field.
///
/// Field names not present in `kinds` fall back to runtime classification:
/// strings become attributes; metrics with the [`Distribution`] flag map to
/// a histogram; anything else is dropped and counted as unclassified.
///
/// `scope` is `&'static str` because the OTel `MeterProvider::meter()` API
/// requires it; we intern via `Box::leak` once per unique entry shape at
/// plan-build time, which leaks O(#entry_types) bytes for the process —
/// acceptable in exchange for keeping the plan cheap to share.
#[derive(Clone, Debug)]
pub(crate) struct EntryPlan {
    pub(crate) scope: &'static str,
    pub(crate) kinds: HashMap<String, FieldKind>,
    /// Names of fields that arrived from a descriptor but carried no
    /// instrument-kind tag. Captured at plan-build time so we can warn once
    /// per descriptor rather than once per write.
    pub(crate) unclassified: Vec<String>,
}

impl EntryPlan {
    /// Plan for a hand-rolled `Entry` that emits no descriptors. Strings
    /// become attributes; only `Distribution`-flagged metrics are recorded.
    pub(crate) fn fallback() -> Self {
        Self {
            scope: "metrique-otel",
            kinds: HashMap::new(),
            unclassified: Vec::new(),
        }
    }

    /// Build a plan from one or more descriptor segments emitted by a single
    /// entry. The meter scope name is taken from the first segment's
    /// canonical entry name.
    pub(crate) fn from_descriptors(segments: &[DescriptorRef<'_>]) -> Self {
        let scope: &'static str = match segments.first() {
            Some(d) => Box::leak(format!("metrique/{}", d.name()).into_boxed_str()),
            None => "metrique-otel",
        };

        let mut kinds = HashMap::new();
        let mut unclassified = Vec::new();
        for desc in segments {
            for field in desc.fields() {
                let mut full = String::new();
                for part in field.name_parts() {
                    full.push_str(part);
                }
                match resolve_kind(&field) {
                    Some(kind) => {
                        kinds.insert(full, FieldKind { kind });
                    }
                    None => {
                        unclassified.push(full);
                    }
                }
            }
        }

        Self {
            scope,
            kinds,
            unclassified,
        }
    }
}

fn resolve_kind(field: &metrique_writer_core::descriptor::FieldView<'_>) -> Option<InstrumentKind> {
    use metrique_writer_core::descriptor::FieldTagState;

    let counter = TypeId::of::<tags::Counter>();
    let up_down = TypeId::of::<tags::UpDownCounter>();
    let histogram = TypeId::of::<tags::Histogram>();
    let gauge = TypeId::of::<tags::Gauge>();

    for tag in field.tags() {
        if tag.state() != FieldTagState::Present {
            continue;
        }
        let id = tag.tag_id();
        if id == counter {
            return Some(InstrumentKind::Counter);
        } else if id == up_down {
            return Some(InstrumentKind::UpDownCounter);
        } else if id == histogram {
            return Some(InstrumentKind::Histogram);
        } else if id == gauge {
            return Some(InstrumentKind::Gauge);
        }
    }
    None
}

/// A pending metric observation captured during `Entry::write`, replayed
/// against the instrument cache once we have the full entry-level attribute
/// set. Buffering is what lets a string field declared *after* a metric
/// field still ride along as an attribute on that metric.
struct PendingMetric {
    name: String,
    kind: InstrumentKind,
    observations: Vec<Observation>,
    unit: Unit,
    per_metric_dimensions: Vec<KeyValue>,
}

pub(crate) struct OtelEntryWriter<'sink, 'plan> {
    pub(crate) cache: &'sink InstrumentCache,
    pub(crate) plan: &'plan EntryPlan,
    /// String fields collected during the walk; applied as attributes to
    /// every metric in this entry at `finish()` time.
    entry_attributes: Vec<KeyValue>,
    pending: Vec<PendingMetric>,
}

impl<'sink, 'plan> OtelEntryWriter<'sink, 'plan> {
    pub(crate) fn new(cache: &'sink InstrumentCache, plan: &'plan EntryPlan) -> Self {
        Self {
            cache,
            plan,
            entry_attributes: Vec::new(),
            pending: Vec::new(),
        }
    }

    pub(crate) fn finish(self) {
        for m in self.pending {
            // Per-metric dimensions take precedence by appearing first; the
            // entry-level attributes follow. The OTEL SDK does not de-dup
            // attribute keys, so any collision is left visible — that's a
            // user-data problem, not something to paper over here.
            let mut attributes = m.per_metric_dimensions;
            attributes.extend(self.entry_attributes.iter().cloned());
            self.cache.record(
                self.plan.scope,
                &m.name,
                m.kind,
                m.observations,
                m.unit,
                &attributes,
            );
        }
    }
}

impl<'a, 'sink, 'plan> EntryWriter<'a> for OtelEntryWriter<'sink, 'plan> {
    fn timestamp(&mut self, _timestamp: std::time::SystemTime) {
        // OTEL meter readers stamp measurements with their own clock; the
        // entry timestamp is informational only.
    }

    fn value(&mut self, name: impl Into<Cow<'a, str>>, value: &(impl Value + ?Sized)) {
        let name = name.into();
        let writer = OtelValueWriter { parent: self, name };
        value.write(writer);
    }

    fn config(&mut self, _config: &'a dyn EntryConfig) {
        // OTEL-specific entry config is not consumed yet.
    }
}

pub(crate) struct OtelValueWriter<'a, 'sink, 'plan> {
    pub(crate) parent: &'a mut OtelEntryWriter<'sink, 'plan>,
    pub(crate) name: Cow<'a, str>,
}

impl<'a, 'sink, 'plan> ValueWriter for OtelValueWriter<'a, 'sink, 'plan> {
    fn string(self, value: &str) {
        // String fields become entry-wide attributes attached to every
        // metric this entry produces.
        self.parent
            .entry_attributes
            .push(KeyValue::new(self.name.into_owned(), value.to_owned()));
    }

    fn metric<'b>(
        self,
        distribution: impl IntoIterator<Item = Observation>,
        unit: Unit,
        dimensions: impl IntoIterator<Item = (&'b str, &'b str)>,
        flags: MetricFlags<'_>,
    ) {
        // Resolve the instrument kind:
        //   1. Plan-provided kind tag wins (descriptor-driven).
        //   2. `Distribution` flag (from metrique-aggregation's histogram
        //      strategy) maps to a histogram instrument.
        //   3. Anything else is unclassified and dropped, a one-time warn
        //      is emitted at plan-build time for descriptors that contain
        //      such fields.
        let kind = match self.parent.plan.kinds.get(self.name.as_ref()) {
            Some(fk) => fk.kind,
            None if flags.downcast::<Distribution>().is_some() => InstrumentKind::Histogram,
            None => return,
        };

        let per_metric_dimensions: Vec<KeyValue> = dimensions
            .into_iter()
            .map(|(k, v)| KeyValue::new(k.to_owned(), v.to_owned()))
            .collect();
        self.parent.pending.push(PendingMetric {
            name: self.name.into_owned(),
            kind,
            observations: distribution.into_iter().collect(),
            unit,
            per_metric_dimensions,
        });
    }

    fn error(self, _error: ValidationError) {
        // Validation errors are silently dropped for now.
    }
}

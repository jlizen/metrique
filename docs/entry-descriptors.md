# Entry descriptors, sources, and field tags

A system on top of metrique's existing `Entry` / `Value` / `CloseValue` traits that lets sinks introspect entry structure and lift structural context out of closed entries.

Three pieces:

- An **entry descriptor** (`&'static EntryDescriptor`) describing a macro-derived entry's closed shape: ordered fields, tags, optionality, dynamic-key maps, units, and sources.
- A **source** system that lets entries declare structural capabilities (timestamps, ids, etc.) and lets sinks extract typed snapshots from the closed entry.
- A **field tag** system that lets sinks define their own opt-in tags and lets users apply them at struct or field scope.

Plus a narrow `no_emit` attribute for fields that must participate in close but not in normal emission.

## What it enables

- Sinks that inspect the complete set of fields an entry can emit, including optional fields and dynamic maps, without observing multiple emissions.
- Sinks that hoist caller-thread context (timestamp, task id, correlation id) out of a closed entry before encoding fields.
- Per-sink, per-field opt-in without sink-specific newtypes on field values.
- First-class units in the descriptor, surfaced however each sink prefers.
- All of the above after `BoxEntry` erasure.

Sinks that do not care pay nothing.

## At a glance

```rust
// Sink crate declares its tags and source shape.
pub struct Export;               // field tag
pub struct RequestContext;       // source tag
pub struct RequestCtx {
    pub request_id: String,
    pub started_at_monotonic_ns: u64,
}

// Application entry uses them.
#[metrics(source(audit::RequestContext))]
#[metrics(default_field_tag(skip(audit::Export)))]
struct RequestAudit { /* ... */ }

#[metrics(default_field_tag(audit::Export))]
struct RequestMetrics {
    #[metrics(no_emit)]
    audit: RequestAudit,

    operation: &'static str,
    request_id: String,

    #[metrics(field_tag(skip(audit::Export)))]
    debug_blob: String,
}

// Sink reads it.
let desc = entry.descriptor()?;
let ctx = desc.source::<audit::RequestContext>(entry.inner_any())?;
audit::open_event(ctx.request_id, ctx.started_at_monotonic_ns);
entry.write(&mut AuditWriter { desc, tag: &audit::Export });
audit::close_event();
```

## The descriptor model

```rust
pub struct EntryDescriptor {
    pub fields: &'static [FieldDescriptor],
    pub sources: &'static [SourceDescriptor],
    pub source_extractors: &'static [SourceExtractor],
}

pub struct FieldDescriptor {
    pub name: &'static str,
    pub tags: &'static [ResolvedFieldTag],
    pub shape: FieldShape,
    pub unit: Option<Unit>,
}

pub enum FieldShape {
    Known(KnownShape),
    Optional(&'static FieldShape),
    Flex { key: StringShape, value: &'static FieldShape },
    Opaque,
}
```

`FieldShape` describes the closed/emitted shape, not the raw Rust field type. `Timer` lowers to `Known(U64)`; `Option<Duration>` to `Optional(Known(U64))`; `Flex<(String, u64)>` to `Flex { key: String, value: Known(U64) }`.

The descriptor is a `'static` constant. Sinks can cache anything derived from it keyed on the pointer.

Hand-written `Entry` impls return `None` for the descriptor. Descriptor-aware sinks treat them as opaque and skip or report.

## Descriptor lookup

Lookup goes through the erased entry vtable, not by widening `BoxEntry`:

```rust
trait ErasedEntry {
    fn write_erased(&self, w: &mut dyn ErasedEntryWriter);
    fn inner_any(&self) -> &(dyn Any + Send + 'static);
    fn descriptor(&self) -> Option<&'static EntryDescriptor>;
}
```

Sinks that don't need descriptors never call `descriptor()`. `BoxEntry` stays the same size.

## Field tags

Sinks define tag types in their own crate. Any type works; the macro does not interpret tag identity beyond equality.

```rust
// Struct-scope default:
#[metrics(default_field_tag(audit::Export))]
#[metrics(default_field_tag(skip(audit::Export)))]

// Field override:
#[metrics(field_tag(audit::Export))]
#[metrics(field_tag(skip(audit::Export)))]
```

Each field/tag pair resolves to `unspecified`, `present`, or `absent`. Field-level overrides beat struct-level defaults. `flatten` preserves the child's explicit decisions; the parent's defaults fill only unspecified slots.

`skip(T)` is an argument form, not a separate attribute.

`#[metrics(tag(...))]` is unchanged and still means entry-enum variant tag.

Full resolution rules including worked inheritance and flatten cases are documented alongside the macro's other field attributes.

## Sources and extractors

Declaration is user-facing:

```rust
#[metrics(source(audit::RequestContext))]
struct RequestAudit { /* ... */ }
```

The generated implementation is an extractor against the closed value:

```rust
pub trait Source<C> {
    type Snapshot;
    fn snapshot(&self) -> Self::Snapshot;
}

impl Source<audit::RequestContext> for ClosedRequestAudit {
    type Snapshot = audit::RequestCtx;
    fn snapshot(&self) -> audit::RequestCtx { /* … */ }
}
```

Sinks look sources up by tag through the descriptor and call the extractor on the closed entry's `inner_any`.

An ad-hoc field form is supported as an escape hatch for types that do not self-describe as a source:

```rust
#[metrics(source(audit::RequestContext), no_emit)]
ctx: MyAdHocContext,
```

Prefer self-describing source types where possible.

## `no_emit`

```text
constructed                yes
closed                     yes
retained                   yes
source-extractable         yes
emitted via Entry::write   no
```

Distinct from `ignore`, which excludes the field from metrics machinery entirely. `no_emit` keeps the field in the closed entry so source extractors (and any future mechanism that reads closed state) can see it.

`no_emit` is primarily reached for when the field exists to provide a source. Source fields do not have to be `no_emit`: if the context data is also useful as normal payload, users typically either leave it as a regular emitted field or attach `#[metrics(flatten)]` so its inner fields become part of the parent entry. `no_emit` is the right choice only when the data must survive close but should not appear in normal emission.

`no_emit` is mutually exclusive with `flatten`.

## Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│ COMPILE TIME: metrique macro                                │
│                                                             │
│ For each macro-derived entry:                               │
│   impl Entry for ClosedX (as today)                         │
│   static EntryDescriptor                                    │
│   impl Source<C> for ClosedX (per declared source)          │
│   descriptor() hook on the erased entry vtable              │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│ RUNTIME: construction                                       │
│                                                             │
│ Caller-thread capture happens inside source fields.         │
│ Other fields populated normally.                            │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│ RUNTIME: append-on-drop / close                             │
│                                                             │
│ CloseValue closes all fields.                               │
│ no_emit fields are retained on the closed entry and remain  │
│ reachable to source extractors.                             │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│ RUNTIME: BackgroundQueue / tee                              │
│                                                             │
│ BoxEntry flows to one or more sinks.                        │
│                                                             │
│  ├── descriptor-unaware sink                                │
│  │     calls Entry::write; never calls descriptor()         │
│  │                                                          │
│  └── descriptor-aware sink                                  │
│        calls entry.descriptor()                             │
│          None    -> skip (hand-written entry, opaque)       │
│          Some(d) -> optional up-front checks:               │
│                     - no relevant tags?      drop cheaply   │
│                     - relevant tag but       report/error   │
│                       no matching source                    │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│ RUNTIME: inside a descriptor-aware sink                     │
│                                                             │
│ ctx = desc.source::<audit::RequestContext>(inner_any)       │
│ open_event(ctx)                                             │
│                                                             │
│ entry.write(SinkWriter { desc, tag: audit::Export }):       │
│   walks Entry::write; consults descriptor to filter         │
│   encoded fields by the sink's tag.                         │
│                                                             │
│ close_event()                                               │
└─────────────────────────────────────────────────────────────┘
```

## Units

```rust
pub struct FieldDescriptor {
    pub unit: Option<Unit>,
    // ...
}
```

Sinks decide how to surface units: a field-name suffix, a schema-level annotation, a separate metadata stream, whatever fits the wire format. Metrique reports units once, structurally, so sinks do not have to infer them.

## Flex

`Flex<(String, T)>` lowers to:

```rust
FieldShape::Flex {
    key: StringShape::String,
    value: &'static FieldShape, // T's closed shape
}
```

One descriptor field regardless of runtime key cardinality. Sinks that understand `Flex` can register one schema; sinks that do not can walk the per-key emissions that `Entry::write` already produces.

## Validation

Validation happens in two places. The macro catches everything it can see without knowing what a tag "means"; a sink catches everything that requires interpreting its own tags.

### Compile-time (at macro expansion)

Intrinsic to the system and independent of any specific sink:

```rust
// duplicate source tag on the same entry
#[metrics(source(audit::RequestContext))]
#[metrics(source(audit::RequestContext))]
struct Bad;
// -> error: duplicate source

// field_tag(T) and field_tag(skip(T)) on the same field
#[metrics(field_tag(audit::Export), field_tag(skip(audit::Export)))]
request_id: String,
// -> error: conflicting field tags

// default_field_tag(T) and default_field_tag(skip(T)) on the same struct
#[metrics(default_field_tag(audit::Export), default_field_tag(skip(audit::Export)))]
struct Bad;
// -> error: conflicting defaults

// unknown attribute argument form
#[metrics(field_tag(audit::Export, extra))]
// -> error: unexpected token
```

Opt-in, sink-driven compile-time checks are possible when a sink ships a helper that the user invokes alongside `#[metrics(...)]`. Those checks run against the descriptor the macro emits; the macro itself does not understand tag identity beyond equality, so it cannot enforce relationships between tags without sink-side help. That is a design choice, not a limitation: it keeps tag ownership in the sink crate.

Examples a sink-side helper could catch:

- A field tagged with the sink's tag but the entry declares no matching source.
- A sink-specific tag (e.g. `InternString`) applied to a field whose closed shape cannot carry string data.
- A value with `FieldShape::Opaque` selected for a tag whose wire format needs a known shape.

### Runtime (at the sink)

Descriptor-aware sinks can repeat any of the sink-driven checks above at startup or on first use of a descriptor, using the static `EntryDescriptor`. Because descriptors are `'static`, these checks can be memoised per descriptor pointer:

- Walk `desc.fields`; confirm that any field tagged with the sink's tag has a shape the sink knows how to encode.
- Walk `desc.sources`; confirm the tags the sink requires are present.
- Cache the verdict. Subsequent entries of the same type pay nothing.

Failures here are reported, not crashed. A tagged field with an opaque shape is skipped on the wire (with a rate-limited log); an entry missing a required source is dropped (with a rate-limited log per descriptor). The rest of the sink continues.

### What is not validated

- **Tag semantics across crates.** The macro cannot know that `alice::X` and `bob::X` in different crates "mean the same thing." Tag identity is nominal.
- **Cross-entry invariants.** The descriptor describes one entry type. Relationships between entries (e.g. "every request start has a corresponding request end") are a sink concern.
- **Value validity.** Whether a field's value is in range, non-empty, etc., is outside this system; metrique's normal value validation applies.

## Future evolution

Short list of things explicitly left out of the initial design that fit the system cleanly:

- Hand-written `Entry` impls opting into descriptors via a `DescribeEntry` trait users implement by hand; same mechanism macro-derived entries use internally.
- Optional sources and multiple sources per tag.
- Heterogeneous values inside `Flex`.
- A compile-time generated per-sink wire plan, for sinks that want to skip runtime `Entry::write` dispatch entirely.

## Appendix: example consumers

Very high level; each concrete sink has its own design.

**dial9 (tracing sink).** Defines `dial9::Dial9` (source tag), `dial9::InTrace` (field tag), `dial9::InternString` (field tag). Extracts `Dial9Context` (worker id, task id, start monotonic timestamp) before encoding. See `dial9-tokio-telemetry/design/metrique-integration.md`.

**OTEL sink (hypothetical).** Would define `otel::Otel` (source tag) and `otel::InSpan` (field tag). Would extract span id, parent span id, and trace id from an `OtelContext` source field.

**Custom user sinks.** Teams can add their own tag types in their own crates with no changes to metrique. Examples: a privacy-tiered export sink with `privacy::Public` / `privacy::Internal`, a metrics-rs bridge with `metricsrs::Export`.
ort`.

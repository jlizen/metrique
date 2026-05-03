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

### The `SourceTag` trait

Every type used as a source tag (the `C` in `source(C)`) must implement `SourceTag`:

```rust
pub trait SourceTag: Any + Send + Sync + 'static {
    /// Called once per distinct `&'static EntryDescriptor` that declares
    /// `source(Self)`. Fires before `main` via link-time registration.
    /// Default is a no-op.
    fn register_descriptor(_desc: &'static EntryDescriptor) {}
}
```

The trait is small on purpose:

- **As a marker**, it identifies types that can legitimately be used as source tags, catching typos at the macro boundary.
- **As a hook**, it lets sinks that want binary-wide startup-time discovery populate their own registry. Sinks that do not care leave the method defaulted; no code runs.

Default `impl` makes adoption trivial:

```rust
impl metrique::SourceTag for audit::RequestContext {}
```

An opt-in sink implements the method:

```rust
static AUDIT_DESCRIPTORS: Lazy<Mutex<Vec<&'static EntryDescriptor>>>
    = Lazy::new(|| Mutex::new(Vec::new()));

impl metrique::SourceTag for audit::RequestContext {
    fn register_descriptor(desc: &'static EntryDescriptor) {
        AUDIT_DESCRIPTORS.lock().unwrap().push(desc);
    }
}
```

How registration is plumbed under the hood (pre-main execution via `ctor`, distributed slices via `linkme`, or a future stable mechanism) is an implementation detail; the public contract is the trait. Sinks can swap the backing mechanism independently.

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

Validation happens in three places, each catching a different class of error.

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

// source(T) where T does not implement SourceTag
#[metrics(source(audit::NotATag))]
struct Oops;
// -> error: the trait bound `audit::NotATag: SourceTag` is not satisfied
```

These are purely structural. The macro does not understand what tags mean; it catches only contradictions in the attributes themselves.

### First-use (descriptor-local, per descriptor)

The first time a descriptor-aware sink encounters a given `&'static EntryDescriptor`, it can walk the descriptor for self-contradictions its own wire format does not support: a sink-specific field tag on an unsuitable `FieldShape`, a field tagged for emission whose closed shape is `Opaque`, a descriptor declaring entry-level tags that require a source the descriptor does not provide.

The checks run once per descriptor (caching on the `&'static` pointer). The sink decides the error policy: `debug_assert!` in debug builds, rate-limited log in release, or both.

### Startup-time (binary-wide, opt-in per sink)

Sinks that want to catch "the sink is attached but no compatible entry types exist in this binary" can use the `SourceTag::register_descriptor` hook. When a sink overrides the hook, every macro-derived descriptor that declares `source(Self)` is registered in whatever store the sink chooses, before `main`. At sink construction, the sink inspects its store and emits a warning (or other signal) if nothing is registered.

This pattern is opt-in for sinks and entirely transparent to end users. Sinks that do not care leave the hook defaulted; metrique emits registration calls regardless, but the default implementation is a no-op and the compiler inlines it away.

Startup-time discovery has known false-positive and false-negative modes that each sink must document for its users:

- **False negatives**: multi-binary workspaces where the entry-bearing struct lives in one binary and the sink lives in another; exotic build configurations that strip pre-main registration sections; dynamically loaded libraries.
- **False positives**: a dependency that ships its own tagged entry types; test binaries that declare test-only tagged entries.

Sinks with non-trivial FP/FN rates should expose an opt-out so users can silence the warning without disabling other validation.

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

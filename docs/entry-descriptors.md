# Entry descriptors, sources, and field tags

A system on top of metrique's existing `Entry` / `Value` / `CloseValue` traits that lets sinks introspect entry structure and lift structural context out of closed entries.

Three pieces:

- An **entry descriptor** (`&'static EntryDescriptor`) describing a macro-derived entry's closed shape: ordered fields, tags, optionality, dynamic-key maps, units, and sources.
- A **source** system that lets entries declare structural capabilities (timestamps, ids, etc.) and lets sinks extract typed snapshots from the closed entry.
- A **field tag** system that lets sinks define their own opt-in tags and lets users apply them at struct or field scope.

Plus a narrow `no_write` attribute for fields that must participate in close but not in normal emission.

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
    #[metrics(no_write)]
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
#[non_exhaustive]
pub struct EntryDescriptor {
    pub fields: &'static [FieldDescriptor],
    pub sources: &'static [SourceDescriptor],
    pub source_extractors: &'static [SourceExtractor],
}

#[non_exhaustive]
pub struct FieldDescriptor {
    pub name: &'static str,
    pub tags: &'static [ResolvedFieldTag],
    pub shape: FieldShape,
    pub unit: Option<Unit>,
}

#[non_exhaustive]
pub enum FieldShape {
    Known(KnownShape),
    Optional(&'static FieldShape),
    Flex { key: StringShape, value: &'static FieldShape },
    Opaque,
}

#[non_exhaustive]
pub enum KnownShape {
    Bool,
    I64,
    U64,
    F64,
    String,
    Bytes,
    // future metrique scalars (Duration subtypes, timestamps, etc.) go here
}

#[non_exhaustive]
pub enum StringShape {
    String,
    // future string variants (cow, interned, etc.) go here
}
```

`FieldShape` describes the closed/emitted shape, not the raw Rust field type. `Timer` lowers to `Known(U64)`; `Option<Duration>` to `Optional(Known(U64))`; `Flex<(String, u64)>` to `Flex { key: String, value: Known(U64) }`.

`Known(KnownShape)` covers scalar types metrique understands intrinsically. Macro-generated `#[metrics(value)]` newtypes over a known scalar lower to the wrapped scalar's shape. User-written `Value` impls that metrique cannot inspect (a bare `impl Value for MyType`) lower to `FieldShape::Opaque`: the sink knows the field is emitted but cannot predict its wire shape.

Because the descriptor is `#[non_exhaustive]` all the way through, future metrique versions can add `KnownShape` variants without breaking hand-written `DescribeEntry` implementors, and new descriptor-aware sinks can introspect older descriptors without compilation breaks.

### The Opaque trapdoor

A field whose closed shape is `FieldShape::Opaque` is fully functional through `Entry::write` (every `Value` impl works; EMF and JSON handle it fine), but descriptor-aware sinks that selected it via a tag have no wire-level shape guarantee for it. Typical sinks skip opaque fields with a diagnostic and continue. This is the price of letting user types implement `Value` without a parallel descriptor hook.

Users who want custom types to flow through descriptor-aware sinks should either use `#[metrics(value)]` (which lowers to a `Known` shape) or wait for the deferred `DescribeValue` extension.

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
pub trait Extractable<C> {
    type Snapshot;
    fn snapshot(&self) -> Self::Snapshot;
}

impl Extractable<audit::RequestContext> for ClosedRequestAudit {
    type Snapshot = audit::RequestCtx;
    fn snapshot(&self) -> audit::RequestCtx { /* … */ }
}
```

Sinks look sources up by tag through the descriptor and call the extractor on the closed entry's `inner_any`.

An ad-hoc field form is supported as an escape hatch for types that do not self-describe as a source:

```rust
#[metrics(source(audit::RequestContext), no_write)]
ctx: MyAdHocContext,
```

Prefer self-describing source types where possible.

### The `SourceTag` trait

Every type used as a source tag (the `C` in `source(C)`) must implement `SourceTag`:

```rust
pub trait SourceTag: Any + Send + Sync + 'static {}
```

It is a pure marker. Implementing it is a one-line impl with no boilerplate:

```rust
impl metrique::SourceTag for audit::RequestContext {}
```

The marker catches typos at the macro boundary (`source(T)` where `T` is not a `SourceTag` is a trait-bound error) and carries no runtime cost. Sinks that want nothing more stop here.

### Opting into binary-wide discovery

Sinks that want to learn "every struct in this binary declaring `source(Self)`" (to validate early, build a registry, warn on empty, etc.) additionally implement `DiscoverableSourceTag`:

```rust
pub trait DiscoverableSourceTag: SourceTag {
    fn register_descriptor(registration: SourceRegistration<'static>);
}

#[non_exhaustive]
pub struct SourceRegistration<'a> {
    pub descriptor: &'a EntryDescriptor,
    // room for future fields
}
```

`register_descriptor` is called once per distinct `&'static EntryDescriptor` that declares `source(Self)`, before `main`, via link-time registration emitted by the metrique macro. How that registration is plumbed under the hood (`linkme`, `ctor`, a future stable mechanism) is an implementation detail.

A sink that wants a binary-wide registry implements both traits:

```rust
static AUDIT_DESCRIPTORS: Lazy<Mutex<Vec<&'static EntryDescriptor>>>
    = Lazy::new(|| Mutex::new(Vec::new()));

impl metrique::SourceTag for audit::RequestContext {}

impl metrique::DiscoverableSourceTag for audit::RequestContext {
    fn register_descriptor(reg: metrique::SourceRegistration<'static>) {
        AUDIT_DESCRIPTORS.lock().unwrap().push(reg.descriptor);
    }
}
```

Sinks that implement only `SourceTag` (not `DiscoverableSourceTag`) emit no link-time registrations. Their binary has no per-source registration statics and no `linkme` machinery; they pay literally nothing. Sinks that opt into discovery pay one `&'static` pointer per `source(Self)` declaration per descriptor.

## `no_write`

Sources are ordinary metrique fields and structs. They close, they live on the closed entry, and by default they emit through `Entry::write` like anything else. Users often `flatten` a source struct or leave it as a regular field if its data is also useful as normal payload.

`no_write` is the opt-out: a field attribute that retains the field in the closed entry (so source extractors can still see it) while excluding it from `Entry::write`. Use it when the data must survive close but should not appear in normal emission. `no_write` is distinct from `ignore`, which excludes the field from metrics machinery entirely.

## Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│ COMPILE TIME: metrique macro                                │
│                                                             │
│ For each macro-derived entry:                               │
│   impl Entry for ClosedX (as today)                         │
│   static EntryDescriptor                                    │
│   impl Extractable<C> for ClosedX (per declared source)          │
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
│ no_write fields are retained on the closed entry and remain  │
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

Sinks that want to catch "the sink is attached but no compatible entry types exist in this binary" implement `DiscoverableSourceTag` and override `register_descriptor`. When a sink provides that impl, every macro-derived descriptor declaring `source(Self)` is registered in whatever store the sink chooses, before `main`. At sink construction, the sink inspects its store and emits a warning (or other signal) if nothing is registered. Sinks that implement only the `SourceTag` marker pay nothing.

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

# Entry descriptors and field tags

> **Status: design, not yet implemented.**

A small system on top of metrique's existing `Entry` / `Value` / `CloseValue` traits that lets sinks introspect the structure of macro-derived entries.

Two pieces, both opt-in for sinks:

- An **entry descriptor** that describes a macro-derived entry's closed shape: ordered fields, their tags, optionality, lists, dynamic-key maps, units, and an optional canonical name.
- A **field tag** system that lets sinks define their own static opt-in markers and lets users apply them at struct or field scope.

Plus a narrow **`no_write`** attribute for fields that participate in close but not in normal emission.

None of this changes the existing `Entry`, `Value`, or `CloseValue` traits. Sinks that do not call `Entry::descriptor` pay nothing.

## Glossary

- **Entry descriptor** (`EntryDescriptor`): a static, metrique-emitted description of a macro-derived entry's closed shape. Sinks read it to learn what fields the entry can emit, in what order, with what tags and units.
- **Field tag**: a user-defined marker type (e.g. `audit::Export`, `dial9::InTrace`) that a sink crate declares and that users apply to fields via `#[metrics(field_tag(T))]`. Sinks read tags off the descriptor to decide per-field behaviour. Metrique does not interpret tag identity.
- **`default_field_tag` / `field_tag`**: struct-level and field-level attributes for applying tags. `skip(T)` is an argument form that inverts a default.
- **`no_write`**: a field attribute that retains the field in the closed value but excludes it from `Entry::write`. Useful for structural data sinks want to introspect without emitting as normal payload.
- **`FieldShape`**: the closed/emitted shape of a field (scalar, optional, list, dynamic-key map, or opaque). Describes what the sink will see, not the raw Rust type.
- **`DescriptorRef`**: the handle returned by `Entry::descriptor()`. Holds either a `&'static EntryDescriptor` (free, macro-derived) or an `Arc<EntryDescriptor>` (cheap to clone, for future enum-per-variant or hand-written cases). Carries a stable `DescriptorId` for cache keying.

## What it enables

- Sinks can inspect the complete set of fields an entry can emit, including optional fields and dynamic maps, without observing multiple live emissions.
- Sinks can declare per-field opt-in via tags users apply to their entries without sink-specific newtypes on field values.
- First-class units in the descriptor, surfaced however each sink prefers.
- All of the above after `BoxEntry` erasure.

Sinks that do not call `Entry::descriptor` pay nothing at runtime.

## At a glance

Here is the end-to-end shape from a user's perspective. The example uses a made-up `audit` sink to keep the mechanics generic.

```rust
// --- in the `audit` sink crate ---

// The audit sink defines a static marker type. Users tag fields with it
// to opt them into the audit stream.
pub struct Export;

// --- in the application ---

use audit::Export;

// The struct declares `Export` as the default for all its fields; individual
// fields can override with `field_tag(skip(Export))`.
#[metrics(default_field_tag(Export))]
struct RequestMetrics {
    // `request_id` inherits the struct default: tagged Export.
    request_id: String,
    // `operation` also inherits the struct default.
    operation: &'static str,
    // `debug_blob` opts out: not in the audit payload.
    #[metrics(field_tag(skip(Export)))]
    debug_blob: String,
}
```

What the macro generates, beyond the existing `Entry` impl, is a static `EntryDescriptor` describing three fields: `request_id` (tag: present(Export), shape: String), `operation` (tag: present(Export), shape: String), `debug_blob` (tag: absent(Export), shape: String).

An audit sink reads the descriptor at sink-side first-use, walks `Entry::write` as usual, and for each value consults the tag map on the descriptor to decide whether to emit that field to its wire format.

## The descriptor model

```rust
pub struct EntryDescriptor {
    // Private fields; use accessor methods (see "Forward compatibility" below).
}

impl EntryDescriptor {
    pub fn name(&self) -> Option<&'static str>;          // canonical entry name, if any
    pub fn fields(&self) -> &'static [FieldDescriptor];
}

pub struct FieldDescriptor {
    // Private fields.
}

impl FieldDescriptor {
    pub fn name(&self) -> &'static str;
    pub fn tags(&self) -> &'static [ResolvedFieldTag];
    pub fn shape(&self) -> FieldShape;
    pub fn unit(&self) -> Option<Unit>;
}

#[non_exhaustive]
pub enum FieldShape {
    Known(KnownShape),
    Optional(&'static FieldShape),
    Flex { key: StringShape, value: &'static FieldShape },
    List(&'static FieldShape),
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

### Forward compatibility

Descriptor structs have private fields and accessor methods (`.name()`, `.fields()`, etc.). This lets metrique add fields to the structs across minor versions without breaking consumer code: consumers call accessors, the accessor signature is stable, internal layout can grow. Descriptor enums (`FieldShape`, `KnownShape`, `StringShape`) are `#[non_exhaustive]`, so consumer `match` expressions must include a `_` arm; new variants are additive.

### Shape mapping

`FieldShape` describes the closed/emitted shape, not the raw Rust field type. Examples:

- `Timer` lowers to `Known(U64)`.
- `Option<Duration>` lowers to `Optional(Known(U64))`.
- `Vec<String>` and `&[String]` lower to `List(Known(String))`.
- `Vec<Option<String>>` lowers to `List(Optional(Known(String)))`.
- `Flex<(String, u64)>` lowers to `Flex { key: String, value: Known(U64) }`.
- `Flex<(String, Option<Duration>)>` lowers to `Flex { key: String, value: Optional(Known(U64)) }`.

The macro recognises one layer of `Optional` inside `List` or `Flex.value`. Deeper combinations (`Vec<Vec<T>>`, `Flex<(String, Vec<T>)>`, `Option<Option<T>>`) lower to `FieldShape::Opaque`; the descriptor enum can represent arbitrary nesting, the macro's syntactic recognition is what is currently restricted.

### The Opaque trapdoor

A field whose closed shape is `FieldShape::Opaque` is fully functional through `Entry::write` (every `Value` impl works; EMF and JSON handle it fine), but descriptor-aware sinks that selected it via a tag have no wire-level shape guarantee for it. Typical sinks skip opaque fields with a diagnostic and continue. This is the price of letting user types implement `Value` without a parallel descriptor hook.

The most common current Opaque case is distribution-typed fields: `metrique_aggregation::Histogram<T>`, `SharedHistogram<T>`, and user-defined types that emit multiple `Observation`s with the `Distribution` flag. The descriptor has no way to represent "this field emits 0..N observations of an inner scalar type." Such fields are safe to use on EMF/JSON sinks today. Tagging them for a descriptor-aware sink produces a diagnostic and skips the field on that sink; see "Future evolution" for the planned `FieldShape::Distribution` variant.

Users who want custom types to flow through descriptor-aware sinks should use `#[metrics(value)]` newtypes over a known scalar.

## Descriptor lookup

The `Entry` trait gains a defaulted method:

```rust
pub trait Entry {
    // existing methods ...

    /// Returns the static descriptor for this entry type, if one exists.
    /// Default returns None; macro-derived entries override.
    fn descriptor(&self) -> Option<DescriptorRef> { None }
}
```

`DescriptorRef` is either borrowed-static (the common macro-derived case) or a cheap-to-clone shared handle:

```rust
pub struct DescriptorRef(DescriptorRefInner);

enum DescriptorRefInner {
    Static(&'static EntryDescriptor),
    Shared(std::sync::Arc<EntryDescriptor>),
}

impl DescriptorRef {
    pub fn as_ref(&self) -> &EntryDescriptor;
    pub fn id(&self) -> DescriptorId;
}

/// Stable identity for caching. Two DescriptorRef clones backed by the same
/// source return equal DescriptorIds. Two distinct Arc-backed descriptors with
/// equivalent contents are not guaranteed to compare equal.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct DescriptorId(/* opaque */);
```

Sinks that want a per-entry-type cache key on `DescriptorId`. For macro-derived entries, `DescriptorId` is derived from the `&'static` pointer and is free. For the future enum-per-variant case (where a variant returns an `Arc`-backed descriptor), `DescriptorId` is derived from the `Arc`'s address and is stable across clones of the same `Arc`.

Extending `Entry` rather than introducing a separate trait keeps descriptor lookup on the path users already know and avoids growing the object-safety surface.

## Field tags

Sinks define tag types in their own crate. Any type works as a tag; the macro does not interpret tag identity beyond equality.

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

## `no_write`

A field attribute that retains the field in the closed entry (so consumers holding the closed value can still inspect it) while excluding it from `Entry::write`. Distinct from `ignore`, which excludes the field from metrics machinery entirely.

`no_write` is intentionally narrow. Its most common use in the initial release is for fields that carry data the parent struct needs during close but does not want emitted (caller-thread state, correlation handles, etc.). Sinks that want structural access to those fields can hold the closed value directly and read them by field name.

## Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│ COMPILE TIME: metrique macro                                │
│                                                             │
│ For each macro-derived entry:                               │
│   impl Entry for ClosedX (as today)                         │
│   static EntryDescriptor                                    │
│   impl Entry::descriptor() returning DescriptorRef::Static  │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│ RUNTIME: construction                                       │
│                                                             │
│ Fields populated normally.                                  │
│ no_write fields are constructed and retained on the closed  │
│ value but excluded from Entry::write.                       │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│ RUNTIME: append-on-drop / close                             │
│                                                             │
│ CloseValue closes all fields.                               │
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
│          Some(d) -> first-use structural checks, cache on   │
│                     d.id(), then proceed                    │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│ RUNTIME: inside a descriptor-aware sink                     │
│                                                             │
│ entry.write(SinkWriter { desc, tag: audit::Export }):       │
│   walks Entry::write; the adapter consults the descriptor   │
│   (cached by DescriptorId) to decide per-field behaviour:   │
│     - field tagged with the sink's tag -> encode            │
│     - otherwise                         -> ignore           │
└─────────────────────────────────────────────────────────────┘
```

## Units

```rust
impl FieldDescriptor {
    pub fn unit(&self) -> Option<Unit>;
}
```

Sinks decide how to surface units: a field-name suffix, a schema-level annotation, a separate metadata stream, whatever fits the wire format. Metrique reports units once, structurally, so sinks do not have to infer them.

## Flex and List

`Flex<(String, T)>` lowers to:

```rust
FieldShape::Flex {
    key: StringShape::String,
    value: /* T's closed shape */,
}
```

`Vec<T>` / `[T]` / `&[T]` lowers to:

```rust
FieldShape::List(/* T's closed shape */)
```

One descriptor entry regardless of runtime cardinality. Sinks that understand `Flex` or `List` can register one schema field for the whole collection; sinks that do not can still fall back to per-element emission through `Entry::write`.

The inner shape may be `Known(_)` or `Optional(Known(_))` in the initial release.

## Validation

Validation happens in two places.

### Compile-time (at macro expansion)

Intrinsic to the system and independent of any specific sink:

```rust
// field_tag(T) and field_tag(skip(T)) on the same field
#[metrics(field_tag(audit::Export), field_tag(skip(audit::Export)))]
request_id: String,
// -> error: conflicting field tags

// default_field_tag(T) and default_field_tag(skip(T)) on the same struct
#[metrics(default_field_tag(audit::Export), default_field_tag(skip(audit::Export)))]
struct Bad;
// -> error: conflicting defaults
```

Sink-specific diagnostics (e.g. `InternString` on a non-string field, an opaque value selected for a sink tag) are produced at runtime by the sink when it first sees a descriptor.

### First-use (descriptor-local, per descriptor)

The first time a descriptor-aware sink encounters a given descriptor (keyed on `DescriptorId`), it can walk the descriptor for self-contradictions its wire format does not support: a sink-specific field tag on an unsuitable `FieldShape`, a field tagged for emission whose closed shape is `Opaque`, etc. The sink decides the error policy (debug_assert + log, log only, silent skip, etc.).

### What is not validated

- **Tag semantics across crates.** The macro cannot know that `alice::X` and `bob::X` in different crates "mean the same thing." Tag identity is nominal.
- **Cross-entry invariants.** The descriptor describes one entry type.
- **Value validity.** Whether a field's value is in range, non-empty, etc., is outside this system; metrique's normal value validation applies.

## Future evolution

Short list of things explicitly left out of the initial design that fit the system cleanly:

- **Typed source extraction.** See the appendix below. Would let sinks pull a typed structural snapshot (timestamp, task id, correlation id, ...) out of the closed entry before encoding fields. Deferred pending a concrete N+1 consumer.
- **Hand-written `Entry` impls opting into descriptors** via a `DescribeEntry` trait users implement by hand; same mechanism macro-derived entries use internally.
- **`FieldShape::Distribution(KnownShape)`** for distribution-typed fields (`Histogram<T>`, `SharedHistogram<T>`, and user types that emit many `Observation`s). Depends on a `DescribeValue` trait so value types can self-describe as distribution-shaped.
- **Nested container recognition beyond one optional layer.** `Vec<Vec<T>>`, `Vec<Flex<..>>`, `Flex<(String, Vec<T>)>`, and double-optional all fall through to `Opaque` today; relaxing the macro recognition is additive.
- **Per-variant descriptors for entry enums.** `DescriptorRef` already supports `Shared(Arc)`-backed variants; a future change can have an entry enum's `Entry::descriptor()` dispatch to per-variant descriptors without breaking the API.
- **A compile-time generated per-sink wire plan**, for sinks that want to skip runtime `Entry::write` dispatch entirely.

## Appendix: possible evolution, typed source extraction

Not shipped in the initial release. Captured here so future consumers (OTEL, a future richer dial9 integration, privacy-tier sinks) can evaluate whether it fits their needs.

Motivation: some sinks want to lift structural data out of a closed entry before encoding fields. Examples: a tracing sink wants a monotonic timestamp + task id to put in the event header; an OTEL sink wants a trace id + span id. Today, a sink either reads those values by field-name convention or relies on the application to put them where the sink expects.

A typed source-extraction system would add:

- A user-facing `#[metrics(source(T))]` attribute on a struct or field, declaring that the entry carries structural data of kind `T`.
- A `SourceTag` trait implemented by the sink's crate on its tag type `T`, carrying the typed `Snapshot` associated type:

  ```rust
  pub trait SourceTag: Any + Send + Sync + 'static {
      type Snapshot: Any + Send;
      fn register_descriptor(_reg: SourceRegistration) {}
  }
  ```

- A `desc.source::<C: SourceTag>(entry: &dyn Any) -> Option<C::Snapshot>` API on the descriptor, returning a typed snapshot.
- An optional `register_descriptor` hook that lets a sink discover, at program-startup time, every descriptor in the binary declaring its source tag. Backed by a link-time mechanism (e.g. `linkme`) behind the hook, so the public API does not pin the mechanism.

The trade-offs were worked through in earlier revisions of this design and are captured in the review doc's "Deferred: typed source extraction" section. The short version:

- Wiring the hook into the `SourceTag` trait means metrique's macro emits one registration per `source(T)` declaration per descriptor whether the hook is overridden or not. Small (one pointer + linkme slot per declaration) but non-zero binary cost for every user.
- Skipping the hook entirely and keeping only the typed extraction API forgoes binary-wide discovery; sinks can still validate per-descriptor on first use.
- Skipping the whole source system and letting sinks read structural fields by convention or by tag-based marker (e.g. a `Dial9ContextField` tag marking which flattened fields hold context) works for the initial dial9 integration without any metrique surface beyond what is already shipped.

The initial release takes the last path. When a second consumer (OTEL, other) materialises, the design-space discussion reopens here.

## Appendix: example consumers

Very high level; each concrete sink has its own design.

**dial9 (tracing sink).** Defines `dial9::Dial9ContextField` (field tag marking context fields), `dial9::InTrace` (field tag), `dial9::InternString` (field tag). Reads context (worker id, task id, start monotonic timestamp) by walking the descriptor for fields tagged `Dial9ContextField`. See `dial9-tokio-telemetry/design/metrique-integration.md`.

**OTEL sink (hypothetical).** Would define `otel::InSpan` (field tag). Would read span id, parent span id, and trace id from fields marked with an OTEL-specific context tag, or push for the typed source-extraction appendix to move in-scope.

**Custom user sinks.** Teams can add their own tag types in their own crates with no changes to metrique. Examples: a privacy-tiered export sink with `privacy::Public` / `privacy::Internal`, a metrics-rs bridge with `metricsrs::Export`.

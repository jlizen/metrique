# Entry descriptors, sources, and field tags: review-only companion

**This document is deleted as part of design sign-off. Anything that survives review lives in `entry-descriptors.md`.**

The permanent doc covers what the system is and how to use it. This doc covers why this shape was picked, what was rejected, and the deeper resolution/validation rules that reviewers want to see but that do not belong in an API reference.

## The problem

Sinks that consume metrique entries fall into three rough camps:

1. **Field-at-a-time renderers.** EMF, JSON, and most text formats. They are happy with `Entry::write` and do not need anything more. The design should not impose cost on them.
2. **Schema-registering sinks.** Binary wire formats that want to pre-register a stable schema per event type, then emit compact payloads that reference the schema by id. Examples: dial9's `dial9-trace-format`, custom internal columnar formats.
3. **Context-hoisting sinks.** Sinks that want to lift structural metadata out of the entry (timestamp, task id, span id, request id, trace id) **before** encoding fields, typically because their wire format puts that metadata in the event header, not in the field list.

Camps 2 and 3 have two problems in common today, neither solvable with `Entry::write` alone:

- **No "all possible fields" view.** A sink that only sees live emissions cannot enumerate optional fields, Flex maps, or enum-shaped entries until it has observed enough traffic to cover them. For optional fields specifically, a realistic entry with `K` optional fields can appear in `2^K` observed shapes, and the sink has no way to collapse that into one schema without introspecting the type.
- **No structural context extraction.** Caller-thread context (timestamp, ids) can live in a field today, but a sink cannot tell the difference between "structural metadata I should hoist into the event header" and "ordinary payload I should encode as a field." The sink has to either name-match (fragile) or accept that the metadata appears inline in the payload (wrong shape for most trace formats).

## Why `EntryConfig` is not enough

`EntryConfig` is metrique's existing per-emission, format-specific metadata mechanism. It is good for the problem it was built for ("sink hands a value through to a format that knows how to use it"), but it is the wrong tool here for several reasons:

- It is per-emission, not per-type. There is no artifact the sink can inspect once and cache.
- It flows sink-to-format, not entry-to-sink. The entry cannot put structural metadata on it in a way that is typed and guaranteed to survive erasure.
- It has no field model. A sink cannot ask "what are all the fields, which are optional, which are dynamic, what units?" through config.
- It is a list of `&dyn Any`, so structural relationships between values are not expressible.

Descriptors + sources are the inverse direction: entry-to-sink, per-type, typed.

## Core requirements

The design had to meet all of these.

**Hard:**

1. Sinks can enumerate an entry's complete emitted shape, including optional fields and dynamic-key maps, without observing live emissions.
2. Sinks can hoist typed structural context out of a closed entry before any field-by-field encoding runs.
3. Sinks can declare per-field opt-in via tags that users apply to their entries without sink-specific newtypes on field values.
4. Works after `BoxEntry` erasure in a heterogeneous queue.
5. Zero cost on sinks that do not use any of this.
6. No changes to `Entry`, `Value`, or `CloseValue`. Sinks that work today keep working.

**Strong preferences:**

- Source-capturing fields participate in the normal `CloseValue` lifecycle, so caller-thread capture (timestamps, ids) happens on the caller thread, and the closed snapshot is what the sink reads on the flush thread.
- Source declarations are struct-level, so user structs do not need per-sink wiring.
- Units stay first-class in the descriptor, so sinks can surface them however fits their wire format.
- Future evolution (hand-written descriptors, optional sources, static wire plans) can slot in without breaking the initial API.

## Non-goals

Explicitly out of scope for this design. Each has a clear evolution path; none is a blocker for the initial work.

- **Hand-written `Entry` impls opted into descriptors.** A type with `impl Entry for MyType {}` but no `#[metrics]` attribute returns `None` from the erased `descriptor()`. Descriptor-aware sinks skip it. Sketched evolution path: a `DescribeEntry` trait users implement by hand. See "Evolution path: hand-written Entry support" below.
- **User-defined `Value` types introspectable as non-`Opaque`.** Today, `impl Value for MyType` lowers to `FieldShape::Opaque`. Users who want macro-known shape use `#[metrics(value)]` newtypes. A parallel `DescribeValue` trait is sketched but not shipped.
- **Distribution-shaped fields (`Histogram<T>`, `SharedHistogram<T>`, user distribution types).** Lower to `FieldShape::Opaque` in this release. Descriptor-aware sinks that select a distribution field via a tag produce a diagnostic and skip the field; EMF and JSON continue to render the distribution normally. Evolution path: add `FieldShape::Distribution(KnownShape)` once `DescribeValue` lands, so aggregation types can self-describe as distribution-shaped without the macro needing to recognise their names syntactically.
- **Nested containers beyond one level.** `FieldShape::List(&'static FieldShape)` and `FieldShape::Flex { value: &'static FieldShape, .. }` accept `Known(_)` or `Optional(Known(_))` only. Deeper combinations (`Vec<Vec<T>>`, `Vec<Flex<..>>`, `Flex<(String, Vec<T>)>`, `Option<Option<T>>`, etc.) lower to `FieldShape::Opaque`. The descriptor enum is already shaped to represent arbitrary nesting; the restriction lives in the macro's syntactic recognition and can be relaxed additively without changing the descriptor type.
- **Optional sources on an entry.** An entry either declares `source(T)` or it does not. No "this entry might or might not carry `T`" form.
- **Multiple sources for the same tag on one entry.** Rejected by the macro.
- **Heterogeneous values inside a single `Flex` map.** `Flex<(String, T)>` has a fixed `T` per type; no `map<string, Any>` form.
- **A compile-time generated per-sink wire plan.** The descriptor-plus-`Entry::write` path is enough to unlock functional requirements. A static plan is strictly additive on top when a consumer needs flush-thread CPU savings beyond the descriptor path.
- **Ad-hoc field-level `#[metrics(source(T))]` on arbitrary field types without a self-describing struct.** Users declare sources at the struct level. Field-level source attribution is deferred.

## Tradeoffs worth reviewer attention

- **Closed-shape descriptor, not Rust-shape descriptor.** The descriptor describes what the entry emits after `CloseValue`, not the raw Rust field types. This matches what sinks need but means descriptor emission depends on the macro's understanding of each field's closed shape. Opaque user `Value` impls fall through to `FieldShape::Opaque`, and sinks that selected opaque fields into one of their tags have to either accept runtime-unknown encoding or reject the entry.
- **Tag identity is opaque to the macro.** The macro records tag paths and forwards them. It does not know which tags are "the audit tag" or "the dial9 tag," and it cannot by itself enforce sink-specific rules like "this tag requires a matching source." Those diagnostics live either in a sink's own derive helper or as a runtime report from the sink. The tradeoff is flexibility (no hardcoded sink list) versus worse default diagnostics.
- **Source extraction runs on the closed value.** Sources cannot observe mid-request state; they see whatever the closed entry has. This is the right model for a tracing sink (caller-thread capture happens in the field's constructor, flush-thread extraction reads the closed snapshot), but it means "capture something at close time" and "capture something at construction time" look the same on the wire. If a sink ever needs to observe pre-close state, it needs a different primitive.
- **Flex lowers to `map<string, T>` only.** Current metrique Flex is `Flex<(String, T)>`. The descriptor reflects that exactly. Heterogeneous or multi-level dynamic maps would need a richer shape language; the design deliberately does not pay that cost now.
- **Descriptor lookup through the erased vtable.** We extend the erased entry trait object with one new method (`descriptor()`), returning `Option<&'static EntryDescriptor>`. That is a one-time surface change to the trait object; after that, `BoxEntry` size is unchanged and descriptor-unaware sinks never call the new method. The addition is a SemVer minor version (not breaking for users of public metrique APIs), but downstream code that directly `impl`s the internal dyn-trait would need to add the method; in practice that impl lives inside metrique.
- **Descriptor types are `#[non_exhaustive]` with hidden constructors.** `EntryDescriptor`, `FieldDescriptor`, `SourceDescriptor`, `SourceExtractor`, and `SourceRegistration` all carry `#[non_exhaustive]`. Construction goes through `#[doc(hidden)] pub const fn __metrique_private_new(..)` methods on each struct; the ugly name is deliberate. The metrique macro is the only intended caller. When hand-written `DescribeEntry` arrives, that PR adds a cleaner public constructor surface (positional `new` or a builder; decided then) without forcing the initial release to lock the shape in.
- **Per-source registration cost.** The metrique macro emits one link-time registration static per `source(T)` declaration per descriptor, whether the tag type overrides `SourceTag::register_descriptor` or not. Cost is one `&'static EntryDescriptor` pointer plus `linkme`-compatible plumbing. Alternatives that would make this truly zero (two-trait split + autoref specialization; sink-invoked registration macros) were rejected as too magical or ceremony-heavy for the bytes saved.

## Why this combination of pieces

Each of descriptors, sources, field tags, and `no_write` is necessary; together they are the smallest set that covers the requirements.

- **Descriptors alone** give sinks a structural view but do not solve caller-thread capture. Context would still have to ride in an `EntryConfig` from a sink wrapper, which brings back the privileged-sink problem. Descriptors also do not let users turn fields on or off per sink; every descriptor-aware sink would see every field.
- **Sources alone** give typed context extraction but no way to describe the rest of the entry. A schema-registering sink still has to walk `Entry::write` and fingerprint, which reintroduces optional-field and Flex explosion.
- **Field tags alone** give per-sink opt-in but no way to enumerate all possible fields (so optional-field schema explosion is still present) and no way to pull structural context out of a closed entry.
- **`no_write` alone** is not a separate feature; it exists so that source-bearing fields can survive close without polluting normal emission. Without `no_write`, users would have to choose between "source is visible as payload" (sometimes wrong) and "source is not available to the sink" (always wrong).

The combination is minimal in another sense: it reuses existing metrique abstractions (`Entry::write`, `CloseValue`, `BoxEntry`, `EntryConfig`) without changing any of them. Descriptors describe the existing closed shape; sources are plain fields with a typed extractor; field tags are opaque markers the macro records; `no_write` is the one new lifecycle annotation. Nothing forces existing users to change, and descriptor-unaware sinks never touch any of it.

None of the four pieces is redundant: removing any one of them forces callers back to a mechanism the design was built to replace.

## Evolution path: hand-written `Entry` impls

`#[metrics(...)]` is the only supported way to generate a descriptor in the initial release. Users who write `impl Entry for MyType { fn write(...) { ... } }` by hand keep working on format-level sinks (EMF, JSON) but return `None` from the erased `descriptor()`; descriptor-aware sinks skip them with a rate-limited log.

Explicit support for hand-written opt-in is a follow-up, not part of the initial release. The rough shape we expect:

```rust
// Sketched, not shipped.
pub trait DescribeEntry: Entry {
    const DESCRIPTOR: &'static EntryDescriptor;
}
```

A `DescribeEntry` impl would populate the same `EntryDescriptor` the macro produces. The metrique macro becomes one implementor of a public surface, not the only implementor. The follow-up PR would need to:

- Promote the hidden `__metrique_private_new` constructors on `EntryDescriptor`, `FieldDescriptor`, `FieldShape`, `KnownShape`, `StringShape`, `SourceDescriptor`, and `SourceRegistration` to a user-facing `pub const fn new(..)` surface (or a builder), so hand-written users can build descriptors in `const` context without relying on names we reserve for macro use.
- Define how users populate `source_extractors` by hand (typed function pointer with a `TypeId` key is the leading candidate).
- Decide whether `ResolvedFieldTag` gets public `const` constructors or a `tags![..]` macro.

None of those decisions constrain the initial release. The descriptor types are `#[non_exhaustive]`; adding `pub const fn` constructors later is additive.

Runtime `Entry::write` fingerprinting is explicitly not on the roadmap as a fallback. It contradicts the design's central argument that optional-field and Flex explosion is structural, not observable.

## Startup-time discovery mechanism

The `SourceTag` trait is a single trait with two responsibilities: declare the typed `Snapshot` the tag produces, and optionally override `register_descriptor`, a hook called once per descriptor declaring `source(Self)`, before `main`, via link-time registration emitted by the metrique macro.

```rust
pub trait SourceTag: Any + Send + Sync + 'static {
    type Snapshot: Any + Send;
    fn register_descriptor(_registration: SourceRegistration) {}
}
```

The macro emits one registration per `source(T)` declaration per descriptor, whether `T` overrides the hook or not. That is a small, bounded binary cost (one `&'static` pointer per declaration plus `linkme` plumbing metrique uses internally).

### Why binary-wide discovery needs any new surface at all

Without some link-time aggregation, sinks cannot detect "I am attached but no matching entries exist in this binary" until they observe live traffic. "The first entry through has no matching tag" does not discriminate misconfiguration from normal startup idle. Binary-wide discovery has to be visible to the sink before the first event is processed, which requires pre-main or link-time machinery that only something generating code in the user's crate can emit. The metrique macro is the only such thing in the design.

### Why one trait with a defaulted hook rather than a two-trait split

A two-trait shape (pure marker `SourceTag` + opt-in `DiscoverableSourceTag`) would let sinks that do not want discovery avoid the per-source registration cost entirely. We considered it and rejected it.

Making the macro emit `<T as DiscoverableSourceTag>::register_descriptor(..)` only when `T: DiscoverableSourceTag` requires one of:

- Autoref-based specialization to dispatch to an inherent no-op method when the trait impl is missing. Works on stable Rust and is used elsewhere in the ecosystem, but adds macro-expansion complexity that is hard to explain and easy to misread when users `cargo expand`.
- A sink-owned registration macro invoked at some well-known site. Brings back user ceremony and cross-crate convention contracts.
- Requiring every `source(T)` to imply `T: DiscoverableSourceTag`, which collapses the split back to one trait.

None of those is worth it for the savings. The per-source registration cost is one `&'static` pointer per declaration (8 bytes plus the `linkme` slot). For a realistic service with tens of source declarations, that is under a kilobyte of rodata. The single-trait design is simpler to explain and leaves room for additional optional hooks in the future without further trait splits.

### Why an associated `Snapshot` type on `SourceTag`

Without it, `desc.source::<C>(..)` has to return `Option<Box<dyn Any + Send>>` and force the sink to downcast manually to whatever snapshot type it knows the tag produces. That moves the type contract out of the trait and into every sink call site. With `type Snapshot`, the call is `Option<C::Snapshot>` and the sink is strongly typed end-to-end. No runtime cost.

### Why `SourceRegistration` instead of `&'static EntryDescriptor` directly

Passing a newtype-wrapped struct lets metrique add fields to the registration payload later (source-declared priority, cross-tag linking, future metadata) without breaking every `impl SourceTag`. The struct is `#[non_exhaustive]`; construction is metrique-internal today. When hand-written `DescribeEntry` lands, that PR adds a `pub const fn new(..)` constructor.

### Why not a typed distributed slice on the trait

A shape we rejected:

```rust
pub trait SourceTag: Any + Send + Sync + 'static {
    const SLICE: &'static linkme::DistributedSlice<[&'static EntryDescriptor]>;
}
```

This puts `linkme` in metrique's public API. Every consumer of the trait inherits a transitive dependency on `linkme`'s type system. Swapping the backing mechanism (stable Rust distributed slices when they land, an alternative crate, a cfg-gated alternative) would be a breaking change for every sink. The method-on-trait shape keeps the mechanism entirely inside metrique's macro expansion and each sink's own impl.

### Why not a convention-named registration macro

A shape we rejected: metrique's macro emits, per source, a call to a declarative macro named by the tag's crate, e.g. `<TagCrate>::__metrique_register_descriptor!(&DESC, ...)`. The sink crate would have to export a macro with that specific name.

Rejected because it forces every sink crate to define a macro with a specific name (magical by convention), cannot be enforced by the type system (missing macros produce macro-resolution errors, not trait-bound errors), and duplicates the trait-bound plus trait-method shape for no gain.

### Why not move registration to the descriptor pointer itself

A shape we rejected: the `EntryDescriptor` carries a list of function pointers (one per declared source) that register it. The metrique macro populates the list from the declared `source(T)` entries, and the descriptor's own static initializer runs the registrations.

Rejected because it runs registration when the descriptor is first touched, not at program startup. That means sinks cannot detect an empty registry reliably: until an entry actually emits, its descriptor is untouched, its registration has not fired, and the registry looks empty even if code for the struct exists. The link-time path avoids this: registrations happen whether or not the struct is ever instantiated, as long as its code is in the binary.

### False-positive and false-negative enumeration

Startup-time discovery reports "no entries registered for tag T" when a sink inspects its registry and finds it empty.

**False negatives (registry empty, user thinks it should not be):**

- Multi-binary workspace where the tagged entry lives in a different binary from the sink. The binary containing the sink genuinely has no registrations.
- Target where link-time registration is unavailable (WASM without feature flags, exotic embedded targets). The sink should cfg-gate its `register_descriptor` override so registration is simply unsupported on those targets.
- Dynamically loaded libraries carrying the entries. Registrations from `dlopen`ed code may not reach the main binary's aggregation point.
- Struct defined in a module that `rustc` DCEs entirely (e.g. feature-gated off). No registration is emitted. The warn is technically correct; the struct is not in the binary.

**False positives (registry non-empty, user thinks it should be):**

- A dependency ships its own tagged entries. The registry has entries the user did not author.
- Test-only tagged entries. Less common because `cargo test` binaries are separate from production binaries.

Each sink names its own FP/FN profile and exposes a per-sink opt-out when the false-positive rate warrants it.

## Field tag resolution: full rules

Each `(field, tag)` pair resolves to one of `unspecified`, `present`, `absent`.

Resolution:

```text
default_field_tag(T):
  sets the struct-scope default for T to present

default_field_tag(skip(T)):
  sets the struct-scope default for T to absent

field_tag(T):
  explicit present for this field, overriding any struct default

field_tag(skip(T)):
  explicit absent for this field, overriding any struct default

flatten:
  child's explicit present/absent decisions on each tag are preserved;
  parent's struct-scope defaults fill only tags the child left unspecified.
```

Worked examples:

**Parent default, no child override.**

```rust
#[metrics(default_field_tag(audit::Export))]
struct Parent {
    request_id: String,
    // request_id resolves to present for audit::Export
}
```

**Parent default, child override.**

```rust
#[metrics(default_field_tag(audit::Export))]
struct Parent {
    #[metrics(field_tag(skip(audit::Export)))]
    debug_blob: String,
    // debug_blob resolves to absent for audit::Export
}
```

**Flatten: child default wins for its own fields, parent fills unspecified.**

```rust
#[metrics(default_field_tag(skip(audit::Export)))]
struct Child {
    internal_id: String,             // absent (child default)
    #[metrics(field_tag(audit::Export))]
    correlation_id: String,          // present (child explicit)
}

#[metrics(default_field_tag(audit::Export))]
struct Parent {
    op: &'static str,                // present (parent default)

    #[metrics(flatten)]
    child: Child,
    // child.internal_id       -> absent  (child default beats parent default)
    // child.correlation_id    -> present (child explicit)
}
```

**Conflicting field-level attributes are rejected.**

```rust
#[metrics(field_tag(audit::Export), field_tag(skip(audit::Export)))]
request_id: String,
// -> error
```

## Validation catalogue

Three phases. The macro catches structural errors it can see without understanding tag identity; the sink catches descriptor-local errors on first use; sinks that opt into startup-time discovery also catch binary-wide misconfigurations before traffic arrives.

| Case | Phase |
| --- | --- |
| duplicate `source(T)` on the same entry | macro (compile) |
| duplicate field-level `field_tag(T)` and `field_tag(skip(T))` | macro (compile) |
| conflicting `default_field_tag(T)` and `default_field_tag(skip(T))` on a struct | macro (compile) |
| `no_write` and `flatten` on the same field | macro (compile) |
| `T` in `source(T)` does not implement `SourceTag` | macro (compile, trait bound) |
| field tagged with a sink tag on an unsuitable `FieldShape` | sink first-use |
| value with `FieldShape::Opaque` selected for a sink tag | sink first-use |
| entry declares a tag requiring a source the entry does not provide | sink first-use |
| sink attached in a binary with no registered entries for its source tag | sink startup (opt-in) |

Sink-specific diagnostics (InternString on a non-string, etc.) depend on the sink's wire format and are not the macro's concern.

## Alternatives considered

### A: Flex-only solution with a "this field is a flex map" flag

Proposed shape: extend `EntryWriter` with `flex_value(name, value)` so sinks can tell "this value is part of a dynamic map" without inferring from observed emissions. Units and optional handling left alone.

Rejected as the whole answer because it solves only one of the three problems (camp 3). It does not give sinks a view of all possible fields, does not help with optional-field explosion, and does not enable context hoisting.

It is not rejected as a data point: the descriptor's `FieldShape::Flex` is essentially the static, descriptor-carried form of this idea.

### B: Sinks derive their own trait over metrique types

Proposed shape: sinks define a parallel derive (e.g. dial9's `#[derive(TraceEvent)]`) that users stack alongside `#[metrics]`, and the sink consumes the derive's output directly.

Rejected because:

- It duplicates everything the metrique macro already decides: close lifecycle, flatten, field naming, value formatting, optionals, tags, units.
- It fails the heterogeneous-queue requirement. After `BoxEntry` erasure, the sink has no way to recover the derived trait without a parallel object-safe trait plus a dial9-owned box type. Even if object safety is solved, `BoxEntrySink::append_any` does not carry the extra bound.
- It forces every user `Value` impl to have a parallel sink-specific impl.

### C: Compile-time per-sink wire plan inside metrique

Proposed shape: metrique generates, per entry, a per-sink wire plan keyed on the sink's tags, so the sink's flush-thread code has zero dispatch overhead.

Rejected for this pass because it does not unlock any new functionality; it is a performance optimisation on top of the descriptor. Left open as a future extension; the descriptor is a strict subset of the data a static plan would need.

### D: `D9Meta` / `Dial9Meta` as flatten-only sugar

Proposed shape: users declare a context struct and attach `flatten`. The sink walks the flattened fields and recognises the context by convention.

Rejected because it conflates source semantics with field emission. Some source fields do not belong in normal emission at all; the user should have a clean way to say "retain for the sink, do not emit." That needs an orthogonal attribute, not a flatten tweak.

Flatten is still supported as a secondary path for users who want the context visible in normal emission too.

### E: Sink wrapper captures context

Proposed shape: caller-thread context is captured by a sink-supplied wrapper (`TokioContextSink`-style) that injects an `EntryConfig` value, and the sink reads the config.

Rejected as the primary path because:

- It is easy to forget in manual composition and creates a privileged wrapper around otherwise peer sinks.
- It forces the sink to be in the caller-thread path. Some compositions (e.g. `BackgroundQueue` only) cannot have their sink see the caller thread at all.
- A field with a `CloseValue` impl is a better home for capture: it runs in the right place naturally, participates in the normal lifecycle, and survives erasure through `inner_any`.

### F: Reuse `#[metrics(ignore)]`

Proposed shape: use the existing `ignore` attribute for source fields.

Rejected because `ignore` means "exclude this field from metrics machinery." Source fields have to stay in the closed entry so the sink can read them through the extractor. The two attributes have different semantics and should have different names.

### G: Units in field names

Proposed shape: surface units by mutating emitted names (e.g. `latency_Microseconds`). No metrique changes needed; every sink sees the unit inline.

Rejected for this pass because it bakes a convention into the name itself, which downstream consumers have to parse back out. Keeping units structural in the descriptor lets each sink decide: names for sinks that want them there, a separate annotation for sinks that do not. The descriptor is a strict superset of the capability.

Individual sinks are free to render the unit into the name on their side if they prefer.

### H: Units as sink-specific field types

Proposed shape: sink wire formats add `U64Microseconds`, `U64Bytes`, etc., and metrique maps into those on encoding.

Rejected because it scales poorly: every new unit requires a new wire type, and `Unit::Custom` cannot be represented at all. A generic annotation mechanism plus `Option<Unit>` in the descriptor handles the same cases without wire churn.

### I: Flex keys always interned

Proposed shape: treat Flex keys as always interned into a sink's string pool.

Rejected. Flex keys are user-controlled and may be high-cardinality. Interning should be an opt-in field tag (`InternString` or similar), not a default.

### J: Runtime schema discovery (the original PR #346 direction)

Proposed shape: sink learns the schema by walking `Entry::write` on each emission and fingerprinting observed `(name, field_type)` sequences, with an LRU cache.

Rejected as the primary mechanism because it structurally cannot solve optional-field schema explosion or unbounded Flex key sets. A realistic entry with several optional fields and a Flex map can produce many thousands of fingerprints. The cache ends up thrashing or bloating.

Runtime discovery is not precluded by the descriptor design: a sink that wants to pay the fingerprint cost for hand-written entries can still walk `Entry::write` itself. This design does not provide that path, and the "Hand-written `Entry` impls" section above explains why we chose not to.

## Feasibility checks

- `BoxEntry::inner()` already returns `&(dyn Any + Send + 'static)`, so typed source extraction through `inner_any` is possible without new unsafe casts.
- The metrique macro already inspects each field's `CloseValue::Closed` type, which is what the descriptor's `FieldShape` needs.
- `FieldDescriptor`, `EntryDescriptor`, and `SourceDescriptor` are all `'static`, so they can live as `const`s in the generated impl.
- Adding one method (`descriptor()`) to the erased entry trait is a one-time surface change; after that, nothing on the hot path changes for descriptor-unaware sinks.

# Entry descriptors and field tags: review-only companion

**This document is deleted as part of design sign-off. Anything that survives review lives in `entry-descriptors.md`.**

The permanent doc covers what the system is and how to use it. This doc covers why this shape was picked, what was rejected, and the deeper resolution/validation rules that reviewers want to see but that do not belong in an API reference.

## The problem

Sinks that consume metrique entries fall into three rough camps:

1. **Field-at-a-time renderers.** EMF, JSON, and most text formats. They are happy with `Entry::write` and do not need anything more. The design must not impose cost on them.
2. **Schema-registering sinks.** Binary wire formats that want to pre-register a stable schema per event type, then emit compact payloads that reference the schema by id. Examples: dial9's `dial9-trace-format`, custom internal columnar formats.
3. **Per-field opt-in sinks.** Sinks that only want a subset of fields in their wire format (e.g. a slim trace payload with a request id and a KPI or two, not the full wide event).

Camps 2 and 3 have problems in common that `Entry::write` alone cannot solve:

- **No "all possible fields" view.** A sink that only sees live emissions cannot enumerate optional fields, `Flex` maps, or enum-shaped entries until it has observed enough traffic to cover them. For optional fields, a realistic entry with `K` optional fields can appear in up to `2^K` observed shapes, and the sink has no way to collapse that into one schema without introspecting the type.
- **No static per-field opt-in**. Users today write all-or-nothing emission. Controlling which fields reach a given sink requires either splitting entry types per sink (terrible ergonomics) or sink-specific field-value newtypes (worse ergonomics).

## Core requirements

Hard constraints.

1. Sinks can enumerate an entry's complete emitted shape, including optional fields and dynamic-key maps, without observing live emissions.
2. Sinks can declare per-field opt-in via tags that users apply to their entries without sink-specific newtypes on field values.
3. Works after `BoxEntry` erasure in a heterogeneous queue.
4. Zero runtime cost on sinks that do not use any of this.
5. No changes to `Entry`, `Value`, or `CloseValue` semantics. Adding a defaulted method to `Entry` is fine; breaking existing impls is not.

Strong preferences:

- Source declarations (for the later, deferred source system) are struct-level, so user structs do not need per-sink wiring.
- Units stay first-class in the descriptor, so sinks can surface them however fits their wire format.
- `FieldShape` variants and struct fields can grow in a minor version without breaking consumers who match on the enum or read the struct.
- Future non-goals (hand-written descriptors, source extraction, static wire plans) can slot in without breaking the initial API.

## Non-goals

Explicitly out of scope for this design. Each has a clear evolution path; none is a blocker for the initial work.

- **Typed source extraction.** The earlier draft of this design shipped a `SourceTag` trait with `type Snapshot` and a `desc.source::<C>()` API for pulling typed structural data out of a closed entry. Deferred to the appendix in the keeper doc. The initial scope is descriptor + field tags + `no_write`; sinks that need structural context read it by walking the descriptor for fields marked with a sink-specific tag. A second consumer (OTEL, a richer dial9 integration, other) is the natural trigger to reopen this.
- **Binary-wide source discovery at startup.** Paired with the source system above. Deferred.
- **`linkme`-backed pre-main registration.** Not needed without the source system. Deferred.
- **Hand-written `Entry` impls opted into descriptors.** A type with `impl Entry for MyType {}` but no `#[metrics]` attribute returns `None` from `descriptor()`. Descriptor-aware sinks skip it. Sketched evolution path: a `DescribeEntry` trait users implement by hand, promoted from hidden macro-only constructors to a public constructor surface at that time.
- **User-defined `Value` types introspectable as non-`Opaque`.** Today, `impl Value for MyType` lowers to `FieldShape::Opaque`. Users who want macro-known shape use `#[metrics(value)]` newtypes. A parallel `DescribeValue` trait is sketched but not shipped.
- **Distribution-shaped fields (`Histogram<T>`, `SharedHistogram<T>`, user distribution types).** Lower to `FieldShape::Opaque` in this release. EMF and JSON continue to render them normally. Evolution path: add `FieldShape::Distribution(KnownShape)` once `DescribeValue` lands.
- **Nested containers beyond one level.** `FieldShape::List` and `FieldShape::Flex.value` accept `Known(_)` or `Optional(Known(_))` only. Deeper combinations (`Vec<Vec<T>>`, `Vec<Flex<..>>`, `Flex<(String, Vec<T>)>`, `Option<Option<T>>`) lower to `Opaque`. The descriptor enum already represents arbitrary nesting; the macro's syntactic recognition is what is restricted.
- **Per-variant descriptors for entry enums.** `DescriptorRef` already supports `Shared(Arc)`, which leaves the door open for a variant-dispatched `Entry::descriptor()`. Not implemented.
- **A compile-time generated per-sink wire plan.** The descriptor-plus-`Entry::write` path is enough to unlock the functional requirements. Static plans are strictly additive on top.
- **Ad-hoc field-level `#[metrics(source(T))]` on arbitrary field types.** Part of the deferred source system.

## Tradeoffs worth reviewer attention

- **Closed-shape descriptor, not Rust-shape descriptor.** The descriptor describes what the entry emits after `CloseValue`, not the raw Rust field types. This matches what sinks need but means descriptor emission depends on the macro's understanding of each field's closed shape. Opaque user `Value` impls fall through to `FieldShape::Opaque`.
- **Tag identity is opaque to the macro.** The macro records tag paths and forwards them. It does not know which tags are "the audit tag" or "the dial9 tag" and cannot enforce sink-specific rules. Diagnostics live in the sink at first use.
- **`Flex` lowers to `map<string, T>` only.** Current metrique `Flex` is `Flex<(String, T)>`. The descriptor reflects that exactly. Heterogeneous or multi-level dynamic maps would need a richer shape language.
- **Descriptor lookup through `Entry::descriptor()`.** A defaulted method on the existing `Entry` trait. `BoxEntry` forwards to it. Object-safe. The alternative (a separate `ErasedEntry` trait) was rejected as unnecessary trait surface; putting the method on `Entry` is what rcoh's review suggested.
- **`DescriptorRef` is cheap-to-clone rather than `&'static` only.** The macro path stays free (returns `Static`). The `Shared(Arc)` variant exists so future enum-per-variant descriptors and hand-written `DescribeEntry` impls are not blocked. Consumers cache on `DescriptorId` rather than pointer-compare.
- **Descriptor types are `#[non_exhaustive]` enums; descriptor structs have private fields with accessor methods.** Accessor methods are the forward-compat path for structs (per rcoh's review); `#[non_exhaustive]` is the forward-compat path for enums.

## Why this combination of pieces

Descriptors and field tags are the minimum set.

- **Descriptors alone** let sinks enumerate shape but do not let users control which fields a sink emits. All-or-nothing. Per-field filtering is a real requirement (see camp 3 in the problem statement).
- **Field tags alone** let users mark per-field opt-in but give sinks no way to enumerate the full set of possible fields. Optional-field schema explosion remains.
- **`no_write`** is a narrow primitive: it lets a field survive close without emitting through `EntryWriter`. Without it, users would have to choose between "data is visible as payload" (sometimes wrong) and "data is not available to the sink at all" (always wrong). Low-surface, high-leverage.

The source system (deferred, in the appendix) adds a fourth piece: typed structural extraction from the closed value. It is orthogonal to the other three. Shipping without it costs the initial dial9 integration some validation sharpness (no "sink attached, no matching entries in binary" check) and forces dial9 to read context by walking the descriptor for fields marked with a dial9-specific tag instead of extracting a typed snapshot. Both are acceptable trade-offs for the initial release; see the dial9 review doc for the sink-side impact.

## Deferred: typed source extraction

The design went through several rounds considering how to let sinks hoist structural context out of closed entries. The current decision is to defer; this section records the landing zone for when a second consumer pushes us back to it.

### Proposed shape

```rust
#[metrics(source(audit::RequestContext))]
struct RequestAudit { /* fields */ }

pub trait SourceTag: Any + Send + Sync + 'static {
    type Snapshot: Any + Send;
    fn register_descriptor(_registration: SourceRegistration) {}
}

#[non_exhaustive]
pub struct SourceRegistration { pub descriptor: &'static EntryDescriptor }

impl EntryDescriptor {
    pub fn source<C: SourceTag>(
        &self,
        entry: &(dyn Any + Send + 'static),
    ) -> Option<C::Snapshot>;
}
```

Sinks read typed snapshots at emission time via `desc.source::<C>(entry.inner_any())`. The `register_descriptor` hook, if overridden, lets the sink populate a binary-wide registry before `main` (via a `linkme`-backed static emitted by the metrique macro per `source(T)` declaration).

### Why this shape

- Trait method (not a typed distributed slice associated with the trait) keeps the link-time mechanism (`linkme`, `ctor`, whatever) out of metrique's public API.
- Single trait with a defaulted hook (rather than a split `SourceTag` + `DiscoverableSourceTag`) keeps the API small at the cost of one link-time registration slot per declaration whether the hook is overridden or not. The only way to make the "zero cost when defaulted" claim honest was autoref-based specialisation, which we rejected as genuinely magical.
- `type Snapshot` on the trait (rather than having `desc.source::<C>()` return `Option<Box<dyn Any>>`) gives sinks typed extraction end-to-end.

### Why it is deferred

- The initial dial9 integration can meet its requirements by reading context from fields tagged with a dial9-specific marker. No typed extraction needed.
- Adding the source system commits every `source(T)` declaration in a user's binary to a per-declaration registration static. Small but non-zero. Not worth paying until there is a second consumer.
- The initial release ships descriptor types with private fields and accessor methods. Promoting the hidden `__metrique_private_new` constructors to public `pub const fn new` (required for hand-written `DescribeEntry`, which the source system design depended on) is also deferred.

### What unblocks re-opening this

- A second real consumer (OTEL implementation, richer dial9 integration, other) for typed extraction. Two independent consumers justify the surface area.
- Clarity on whether the `linkme`-backed startup hook is the right long-term mechanism or whether a future stable Rust feature should be the basis.

### Rejected alternatives within the deferred source design

These are captured for when the design re-opens, not for the current review.

- **Typed distributed slice on the trait** (`const SLICE: &'static linkme::DistributedSlice<...>`). Puts `linkme` in metrique's public API.
- **Convention-named registration macro** the sink's crate must export. Magical by convention, not type-system-enforced.
- **Descriptor-initialized registration** (the descriptor's own static initializer runs registrations when first touched). Registrations only fire when the struct is used, so "sink attached, no matching entries" cannot be detected at startup.
- **Autoref specialisation** to make `DiscoverableSourceTag` optional. Works, opaque.
- **Cargo feature for opt-out**. Infectious across workspace.
- **User-invoked `assert_foo_compatible!` macro**. Opt-in compile-time check that users forget to invoke.

## Hand-written `Entry` impls

`#[metrics(...)]` is the only supported way to generate a descriptor in the initial release. Users who write `impl Entry for MyType { fn write(...) { ... } }` by hand keep working on format-level sinks (EMF, JSON) but return `None` from `descriptor()`; descriptor-aware sinks skip them with a rate-limited log.

Explicit support for hand-written opt-in is a follow-up. The rough shape:

```rust
// Sketched, not shipped.
pub trait DescribeEntry: Entry {
    const DESCRIPTOR: &'static EntryDescriptor;
}
```

A `DescribeEntry` impl would populate the same `EntryDescriptor` the macro produces. The metrique macro becomes one implementor of a public surface, not the only implementor. The follow-up PR would need to:

- Promote the hidden `__metrique_private_new` constructors on `EntryDescriptor`, `FieldDescriptor`, `FieldShape`, `KnownShape`, `StringShape`, and (if re-introduced) `SourceDescriptor` / `SourceExtractor` / `SourceRegistration` to a user-facing `pub const fn new(..)` surface (or a builder), so hand-written users can build descriptors in `const` context without relying on names we reserve for macro use.
- Decide whether `ResolvedFieldTag` gets public `const` constructors or a `tags![..]` macro.

Runtime `Entry::write` fingerprinting is explicitly not on the roadmap as a fallback. It contradicts the design's central argument that optional-field and `Flex` explosion is structural, not observable.

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

**Flatten on `Option<SubEntry>` propagates optionality.**

If `SubEntry { baz: Option<usize> }` is `#[metrics(flatten)]`ed through an `Option<SubEntry>`, the descriptor lists `baz: Optional(Known(U64))`. `Optional` wraps the emit-or-not decision; it is not re-stacked when the inner type is already optional. Genuinely double-optional types (`Option<Option<T>>`) lower to `FieldShape::Opaque`, consistent with the one-level nesting restriction.

**Conflicting field-level attributes are rejected.**

```rust
#[metrics(field_tag(audit::Export), field_tag(skip(audit::Export)))]
request_id: String,
// -> error
```

## Validation catalogue

| Case | Phase |
| --- | --- |
| duplicate field-level `field_tag(T)` and `field_tag(skip(T))` | macro (compile) |
| conflicting `default_field_tag(T)` and `default_field_tag(skip(T))` on a struct | macro (compile) |
| `no_write` and `flatten` on the same field | macro (compile) |
| field tagged with a sink tag on an unsuitable `FieldShape` | sink first-use |
| value with `FieldShape::Opaque` selected for a sink tag | sink first-use |

Sink-specific diagnostics (InternString on a non-string, etc.) depend on the sink's wire format and are not the macro's concern.

## Rejected alternatives (outside the deferred source system)

### A: `Flex`-only "this field is flexing" flag

Proposed shape: extend `EntryWriter` with `flex_value(name, value)` so sinks can tell "this value is part of a dynamic map" without inferring from observed emissions.

Rejected as the whole answer because it solves only one of the schema-stability problems (Flex). It does not give sinks a view of all possible fields and does not help with optional-field explosion. The descriptor path is a strict superset.

### B: Sinks derive their own trait over metrique types

Proposed shape: sinks define a parallel derive (e.g. dial9's `#[derive(TraceEvent)]`) that users stack alongside `#[metrics]`, and the sink consumes the derive's output directly.

Rejected because:

- It duplicates everything the metrique macro already decides: close lifecycle, flatten, field naming, value formatting, optionals, tags, units.
- It fails the heterogeneous-queue requirement. After `BoxEntry` erasure, the sink has no way to recover the derived trait without a parallel object-safe trait plus a dial9-owned box type.
- Either every user `Value` impl needs a parallel sink-specific impl (maintenance cost forever), or a blanket `TraceField for impl Value` collapses the compile-time shape knowledge back to runtime dispatch through a differently-named trait.

### C: Compile-time per-sink wire plan inside metrique

Proposed shape: metrique generates, per entry, a per-sink wire plan keyed on the sink's tags.

Rejected for this pass because it does not unlock new functionality; it is a performance optimisation on top of the descriptor. Left open as a future extension.

### D: Units in field names

Proposed shape: surface units by mutating emitted names (e.g. `latency_Microseconds`).

Rejected because it bakes a convention into the name itself, which downstream consumers have to parse back out. Keeping units structural in the descriptor lets each sink decide.

### E: Units as sink-specific field types

Proposed shape: sink wire formats add `U64Microseconds`, `U64Bytes`, etc., and metrique maps into those on encoding.

Rejected because it scales poorly: every new unit requires a new wire type, and `Unit::Custom` cannot be represented at all.

### F: Runtime schema discovery (the original PR direction)

Proposed shape: sink learns the schema by walking `Entry::write` on each emission and fingerprinting observed `(name, field_type)` sequences, with an LRU cache.

Rejected as the primary mechanism because it structurally cannot solve optional-field schema explosion or unbounded `Flex` key sets. A realistic entry with several optional fields and a `Flex` map can produce many thousands of fingerprints. The cache ends up thrashing or bloating.

Runtime discovery is not precluded by the descriptor design: a sink that wants to pay the fingerprint cost for hand-written entries can still walk `Entry::write` itself. This design does not provide that path.

## Feasibility checks

- `BoxEntry::inner()` already returns `&(dyn Any + Send + 'static)`, so descriptor-based sinks can reach the concrete closed entry for sink-specific needs (including the deferred source extraction) without new unsafe casts.
- The metrique macro already inspects each field's `CloseValue::Closed` type, which is what the descriptor's `FieldShape` needs.
- `EntryDescriptor`, `FieldDescriptor`, and `DescriptorRef` are all in scope of the existing `metrique-writer-core` crate.
- Adding `descriptor()` as a defaulted method on `Entry` is a SemVer minor change. External `impl Entry` blocks that do not override the method continue to compile and return `None`, which is the intended default.

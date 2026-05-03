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
- Future non-goals (hand-written descriptors, optional sources, static wire plans) can slot in without breaking the initial API.

## Tradeoffs worth reviewer attention

- **Closed-shape descriptor, not Rust-shape descriptor.** The descriptor describes what the entry emits after `CloseValue`, not the raw Rust field types. This matches what sinks need but means descriptor emission depends on the macro's understanding of each field's closed shape. Opaque user `Value` impls fall through to `FieldShape::Opaque`, and sinks that selected opaque fields into one of their tags have to either accept runtime-unknown encoding or reject the entry.
- **Tag identity is opaque to the macro.** The macro records tag paths and forwards them. It does not know which tags are "the audit tag" or "the dial9 tag," and it cannot by itself enforce sink-specific rules like "this tag requires a matching source." Those diagnostics live either in a sink's own derive helper or as a runtime report from the sink. The tradeoff is flexibility (no hardcoded sink list) versus worse default diagnostics.
- **Source extraction runs on the closed value.** Sources cannot observe mid-request state; they see whatever the closed entry has. This is the right model for a tracing sink (caller-thread capture happens in the field's constructor, flush-thread extraction reads the closed snapshot), but it means "capture something at close time" and "capture something at construction time" look the same on the wire. If a sink ever needs to observe pre-close state, it needs a different primitive.
- **Flex lowers to `map<string, T>` only.** Current metrique Flex is `Flex<(String, T)>`. The descriptor reflects that exactly. Heterogeneous or multi-level dynamic maps would need a richer shape language; the design deliberately does not pay that cost now.
- **Descriptor lookup through the erased vtable.** We extend the erased entry trait object with one new method (`descriptor()`), returning `Option<&'static EntryDescriptor>`. That is a one-time surface change to the trait object; after that, `BoxEntry` size is unchanged and descriptor-unaware sinks never call the new method.

## Why this combination of pieces

Each of descriptors, sources, field tags, and `no_emit` is necessary; together they are the smallest set that covers the requirements.

- **Descriptors alone** give sinks a structural view but do not solve caller-thread capture. Context would still have to ride in an `EntryConfig` from a sink wrapper, which brings back the privileged-sink problem. Descriptors also do not let users turn fields on or off per sink; every descriptor-aware sink would see every field.
- **Sources alone** give typed context extraction but no way to describe the rest of the entry. A schema-registering sink still has to walk `Entry::write` and fingerprint, which reintroduces optional-field and Flex explosion.
- **Field tags alone** give per-sink opt-in but no way to enumerate all possible fields (so optional-field schema explosion is still present) and no way to pull structural context out of a closed entry.
- **`no_emit` alone** is not a separate feature; it exists so that source-bearing fields can survive close without polluting normal emission. Without `no_emit`, users would have to choose between "source is visible as payload" (sometimes wrong) and "source is not available to the sink" (always wrong).

The combination is minimal in another sense: it reuses existing metrique abstractions (`Entry::write`, `CloseValue`, `BoxEntry`, `EntryConfig`) without changing any of them. Descriptors describe the existing closed shape; sources are plain fields with a typed extractor; field tags are opaque markers the macro records; `no_emit` is the one new lifecycle annotation. Nothing forces existing users to change, and descriptor-unaware sinks never touch any of it.

None of the four pieces is redundant: removing any one of them forces callers back to a mechanism the design was built to replace.

## Hand-written `Entry` impls

`#[metrics(...)]` is a convenience. Users can and do write `impl Entry for MyType { fn write(...) { ... } }` directly, without any derive. The descriptor system has to have a story for them.

### Today

Hand-written `Entry` impls keep working unchanged on format-level sinks (EMF, JSON, anything that consumes `Entry::write`). Nothing the descriptor system introduces breaks that path.

Descriptor-aware sinks see `descriptor() == None` on those entries. The default behaviour is skip with a rate-limited warn, keyed on `inner_any().type_id()` so each concrete hand-written type is reported at most a handful of times.

### Opt-in path: manual `DescribeEntry`

A user with a hand-rolled `Entry` can opt back in by implementing a second trait. Rough shape:

```rust
pub trait DescribeEntry: Entry {
    const DESCRIPTOR: &'static EntryDescriptor;

    // Type-erased source extraction. Given a type-id, return a boxed snapshot
    // of the appropriate Snapshot type for that source.
    fn extract_source(&self, tag: TypeId) -> Option<Box<dyn Any + Send>> {
        let _ = tag;
        None
    }
}

// Blanket for descriptor lookup via the erased entry vtable:
impl<T: Entry + DescribeEntry + Send + 'static> ErasedEntry for T {
    fn descriptor(&self) -> Option<&'static EntryDescriptor> {
        Some(T::DESCRIPTOR)
    }
    // ...
}
```

A user implementation would look roughly like:

```rust
struct MyThing { request_id: String, latency_us: u64, /* ... */ }

impl Entry for MyThing { fn write<'a>(&'a self, w: &mut impl EntryWriter<'a>) { /* ... */ } }

impl DescribeEntry for MyThing {
    const DESCRIPTOR: &'static EntryDescriptor = &EntryDescriptor {
        fields: &[
            FieldDescriptor {
                name: "request_id",
                tags: tags![ present(dial9::InTrace) ],
                shape: FieldShape::Known(KnownShape::String),
                unit: None,
            },
            FieldDescriptor {
                name: "latency",
                tags: tags![ present(dial9::InTrace) ],
                shape: FieldShape::Known(KnownShape::U64),
                unit: Some(Unit::Microsecond),
            },
        ],
        sources: &[ SourceDescriptor { tag: tag_of::<dial9::Dial9>() } ],
        source_extractors: &[ /* pointer to extract_source-style function */ ],
    };

    fn extract_source(&self, tag: TypeId) -> Option<Box<dyn Any + Send>> {
        if tag == TypeId::of::<dial9::Dial9>() {
            Some(Box::new(dial9::Dial9ContextSnapshot { /* ... */ }))
        } else {
            None
        }
    }
}
```

With that impl in place, a hand-written entry participates in every descriptor-aware sink exactly as a macro-derived one does.

### Design questions this raises

Manual implementation is the actual load-bearing case, not derive sugar. Two constraints on the descriptor API follow from "must be constructible by hand in `const` context":

1. **`ResolvedFieldTag` must have `const` constructors.** A user writing the tag array has to be able to say `present::<dial9::InTrace>()` or equivalent in a const. The macro type holding the resolved tags cannot hide behind private variants that only the macro can construct.
2. **Source extraction must be expressible without macro-generated code.** The approach above (type-erased `extract_source` on the trait, typed extractors stored in the descriptor) is one way; a typed-function-pointer-per-source with a `TypeId` key is another. The review doc does not commit; both work. What matters is that whichever we pick, a hand-written user can populate it.

These are the shape of the public API metrique will need. The macro becomes one implementor of the same public surface, not the only implementor.

### Non-goals for hand-written impls

- Auto-generated `DescribeEntry` from an `Entry` impl: not on the roadmap. The whole point of hand-written impls is that the user is declaring the shape themselves.
- A runtime `Entry::write` fingerprinter as a fallback: not on the roadmap. It contradicts the design's central argument (that optional-field and Flex explosion is structural, not observable).

### Interaction with `#[metrics]`

Hand-written `DescribeEntry` and macro-derived `DescribeEntry` coexist in the same pipeline with no extra glue. A heterogeneous `BoxEntrySink` can carry both. A future extension could let users attach `#[metrics]` to a type that has a custom `Entry` impl to fill in the descriptor half automatically, but that is strictly sugar; the manual path is complete on its own.

## Startup-time discovery mechanism

The `SourceTag` trait carries an optional hook, `register_descriptor(desc: &'static EntryDescriptor)`, that fires once per distinct descriptor declaring `source(Self)`. The default impl is a no-op; sinks that want binary-wide discovery override it.

Several design questions drove the choice of this shape.

### Why the trait has a hook at all

Without a hook, sinks cannot detect "I am attached but no matching entries exist in this binary" until they observe live traffic, which has the "first entry doesn't have this tag, but the second one might" ambiguity. Binary-wide discovery requires some compile/link-time aggregation that only exists if the metrique macro emits it. Making that aggregation part of `SourceTag` is the minimal surface: one trait method, default no-op, used only by sinks that care.

### Why a trait method instead of a typed distributed slice

The shape we rejected:

```rust
pub trait SourceTag: Any + Send + Sync + 'static {
    const SLICE: &'static linkme::DistributedSlice<[&'static EntryDescriptor]>;
}
```

This puts `linkme` in metrique's public API. Any consumer of `SourceTag` has a transitive dependency on `linkme`'s type system. If `linkme` ever needs to be swapped for a future mechanism (stable Rust distributed slices, a different registration crate, etc.) every sink would have to update.

The trait-method shape keeps `linkme` (or `ctor`, or whatever else) entirely inside metrique's and each sink's implementations. The public contract is a plain `fn`. Sinks and users can't tell what metrique's macro uses internally.

### Why a method instead of a plain marker trait

The shape we also considered:

```rust
pub trait SourceTag: Any + Send + Sync + 'static {}
```

and then have sinks opt into discovery via a separate registration macro the user invokes per-struct. This reintroduces user ceremony, which the design rejects elsewhere, and it forces sinks that want discovery to own a public registration macro.

The hook-on-trait path routes discovery through a single mechanism: the metrique macro emits registration for every `source(T)` declaration unconditionally; the trait method controls whether that registration is a no-op or does work.

### Why the hook is defaulted rather than required

A defaulted hook means every `SourceTag` impl is `impl SourceTag for T {}`, with no boilerplate, unless the sink actively wants discovery. Requiring an override would force every sink to write the method, including sinks that have no reason to care. The default-is-no-op shape is consistent with how metrique's other optional-override mechanisms work.

### False-positive and false-negative enumeration

Startup-time discovery reports "no entries registered for tag T" when it observes an empty registry at sink construction. The failure modes:

**False negatives (registry empty, user thinks it should not be):**

- Multi-binary workspace where the tagged entry lives in a different binary from the sink. The binary containing the sink genuinely has no registrations; the warn is technically correct but may surprise users thinking about their project holistically.
- Exotic linker configurations that strip pre-main registration sections. Typical `cargo build` on tier-1 targets does not hit this.
- Dynamically loaded libraries carrying the entries. Registrations from `dlopen`ed code may not reach the main binary's aggregation point.
- Struct defined in a module that `rustc` DCEs entirely (e.g. feature-gated off). The macro never expands on the struct; no registration is emitted. Warn is correct in this case: the struct is not in the binary.

**False positives (registry non-empty, user thinks it should be):**

- A dependency ships its own tagged entries. The registry has entries from the dep even though the user added none of their own. Usually this is fine, since users who use dial9 generally want any dial9 telemetry to flow, but it can mask misconfigurations in the user's own code.
- Test-only tagged entries in the same binary as production code that does not use them. Less common (test binaries and production binaries are separate in typical Cargo setups).

Each sink should name its own FP/FN profile and expose an opt-out when the false-positive rate warrants it.

### Why not use a metrique-provided registration macro

Shape we considered: metrique ships `metrique::__register_source!` that sinks invoke from their own code to wire up a registry. The metrique macro, per source, emits `<TagCrate>::__metrique_register_descriptor!(&DESC, ...)`, delegating to a convention-named macro the sink crate must export.

Rejected because it:

- Forces every sink crate to define a macro with a specific name (magical by convention).
- Cannot be enforced by the type system; missing macros produce ugly macro-resolution errors rather than trait bound errors.
- Duplicates the trait-bound + trait-method shape for no gain.

### Why not move registration to the descriptor pointer itself

Shape we considered: the `EntryDescriptor` carries a list of function pointers (one per declared source) that register it. The metrique macro populates the list from the declared `source(T)` entries, and the descriptor's own static initializer runs the registrations.

Rejected because it runs registration when the descriptor is first touched, not at program startup. That means sinks can't detect an empty registry reliably: until an entry actually emits, its descriptor is untouched, its registration has not fired, and the registry looks empty even if code for the struct exists.

The `ctor`/`linkme`-backed pre-main path avoids this: registrations happen whether or not the struct is ever instantiated, as long as its code is in the binary.

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
| `no_emit` and `flatten` on the same field | macro (compile) |
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

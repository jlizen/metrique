# Entry descriptors: implementation plan

**This document is deleted as part of design sign-off. Keeper is `entry-descriptors.md`; alternatives analysis is `entry-descriptors-review.md`.**

Status: nothing here is implemented; this plan captures what the work looks like, in what order, in which files, and which design decisions each piece ties to.

## Sequencing

Work lives in four tracks with explicit dependencies. Tracks run in parallel where the graph permits.

### Track M-A: descriptor, source, and field-tag types

Prerequisite for everything else.

- A1. Define `EntryDescriptor`, `FieldDescriptor`, `FieldShape`, `KnownShape`, `StringShape`, `SourceDescriptor`, `SourceExtractor`, `ResolvedFieldTag`, `Extractable<C>`, `SourceTag`, `DiscoverableSourceTag`, and `SourceRegistration` in `metrique-writer-core`. All structs/enums are `#[non_exhaustive]` with `pub const fn` constructors. Ties to: keeper "The descriptor model", "Sources and extractors", "Opting into binary-wide discovery".
- A2. Public re-exports from `metrique` crate.

### Track M-B: erased entry vtable hook

Prerequisite for any descriptor-aware sink.

- B1. Add `fn descriptor(&self) -> Option<&'static EntryDescriptor>` to the object-safe dyn-trait backing `BoxEntry` (`metrique-core`). Default returns `None`. This is a SemVer minor (additive method on a trait used internally behind `BoxEntry`). Ties to: keeper "Descriptor lookup"; review "Tradeoffs → Descriptor lookup through the erased vtable".
- B2. Update `BoxEntry::descriptor()` wrapper to forward through the dyn-trait.

### Track M-C: macro attributes and descriptor emission

Depends on M-A and M-B.

- C1. `metrique-macro/src/lib.rs`: accept `default_field_tag(T)`, `default_field_tag(skip(T))`, `field_tag(T)`, `field_tag(skip(T))`, `source(T)`, `no_write`. Parse and validate at expansion time. Ties to: keeper "Field tags", "Sources and extractors", "`no_write`".
- C2. `metrique-macro/src/structs.rs`: generate the `static EntryDescriptor` constant for macro-derived entries. Field order matches `Entry::write` order (declaration order), fields emit with resolved tags and computed `FieldShape`. Ties to: keeper "The descriptor model".
- C3. Generate `impl Extractable<C> for ClosedT` per declared `source(C)`. The generated impl reads the relevant field on the closed entry and produces the snapshot type declared by the source tag. Ties to: keeper "Sources and extractors".
- C4. Generate per-source link-time registration. For each `source(T)` declaration, emit:

  ```rust
  #[linkme::distributed_slice(metrique::__SOURCE_REGISTRATIONS)]
  static __REG: fn() = || {
      <T as metrique::DiscoverableSourceTag>::register_descriptor(
          metrique::SourceRegistration::new(&DESCRIPTOR),
      );
  };
  ```

  This compiles only when `T: DiscoverableSourceTag`. Sinks that implement only the marker `SourceTag` get a trait-bound compile error from the macro expansion unless the user is also using a discovery-aware sink for that tag, which is the intended behaviour. Ties to: keeper "Opting into binary-wide discovery"; review "Why split into two traits".

  The metrique runtime iterates `__SOURCE_REGISTRATIONS` once at `std::sync::Once`-guarded startup, calling each function pointer. Registrations then run through their `DiscoverableSourceTag` impls. `linkme` is metrique-internal; not part of the public API.
- C5. `metrique-macro/src/entry_impl.rs`: `Entry::write` output is consistent with the descriptor's field order; `no_write` fields are omitted from the write path but retained through close.
- C6. Macro-level diagnostics for the intrinsic validation rules (duplicate `source(T)`, conflicting `field_tag(T)` vs `field_tag(skip(T))`, conflicting `default_field_tag` declarations, `source(T)` where `T: !SourceTag`). Ties to: keeper "Validation → Compile-time".

### Track M-D: hand-written DescribeEntry

Optional but visible to external users. Can proceed after M-A and M-B.

- D1. Define `pub trait DescribeEntry: Entry` with a `const DESCRIPTOR: &'static EntryDescriptor` and an `extract_source(&self, tag: TypeId) -> Option<Box<dyn Any + Send>>` method. Ties to: review "Hand-written `Entry` impls".
- D2. Blanket impl wiring so any `T: Entry + DescribeEntry + Send + 'static` contributes its descriptor through the erased entry vtable.
- D3. Documentation and example (see "Hand-written DescribeEntry example" below).
- D4. Tests confirming hand-written entries interoperate with macro-derived entries in a heterogeneous `BoxEntrySink`.

## Hand-written DescribeEntry example

Canonical example going into `metrique/examples/` and referenced from crate docs.

```rust
use std::any::{Any, TypeId};
use metrique::{
    CloseValue, Entry, EntryWriter,
    descriptor::{
        DescribeEntry, EntryDescriptor, FieldDescriptor, FieldShape, KnownShape,
        ResolvedFieldTag, SourceDescriptor, SourceExtractor,
    },
    Unit,
};

// User's hand-written entry type.
pub struct RequestMetrics {
    pub request_id: String,
    pub latency_us: u64,
    pub audit_ctx: audit::AuditContext,
}

impl CloseValue for RequestMetrics {
    type Closed = Self;
    fn close(self) -> Self { self }
}

impl Entry for RequestMetrics {
    fn write<'a>(&'a self, w: &mut impl EntryWriter<'a>) {
        w.value("request_id", &self.request_id);
        w.value("latency", &self.latency_us);
        // audit_ctx intentionally not written; it provides audit::Audit via DescribeEntry.
    }
}

impl DescribeEntry for RequestMetrics {
    const DESCRIPTOR: &'static EntryDescriptor = &EntryDescriptor::new_const(
        &[
            FieldDescriptor::new_const(
                "request_id",
                &[ResolvedFieldTag::present_const::<audit::Export>()],
                FieldShape::Known(KnownShape::String),
                None,
            ),
            FieldDescriptor::new_const(
                "latency",
                &[ResolvedFieldTag::present_const::<audit::Export>()],
                FieldShape::Known(KnownShape::U64),
                Some(Unit::Microsecond),
            ),
        ],
        &[SourceDescriptor::new_const::<audit::Audit>()],
        &[SourceExtractor::new_const::<audit::Audit>(
            |entry, tag| {
                if tag == TypeId::of::<audit::Audit>() {
                    let me = entry.downcast_ref::<RequestMetrics>()?;
                    Some(Box::new(me.audit_ctx.snapshot()))
                } else {
                    None
                }
            },
        )],
    );

    fn extract_source(&self, tag: TypeId) -> Option<Box<dyn Any + Send>> {
        if tag == TypeId::of::<audit::Audit>() {
            Some(Box::new(self.audit_ctx.snapshot()))
        } else {
            None
        }
    }
}
```

Two things worth noting:

- `ResolvedFieldTag::present_const::<T>()` is a `const fn` that produces a tag entry the descriptor expects. Its internals are an implementation detail; it takes a `TypeId::of::<T>()` and wraps it in whatever shape `ResolvedFieldTag` has.
- `FieldDescriptor::new_const(...)`, `EntryDescriptor::new_const(...)`, etc., are the `pub const fn` constructors that exist specifically so hand-written `DescribeEntry` paths can build descriptors. The types are `#[non_exhaustive]` so their field set can grow without breaking this example.

## New public APIs at the boundary

Designed here; the canonical signatures live in the keeper and in the generated rustdoc.

### In `metrique-writer-core`

```rust
#[non_exhaustive]
pub struct EntryDescriptor { /* ... */ }
impl EntryDescriptor {
    pub const fn new_const(
        fields: &'static [FieldDescriptor],
        sources: &'static [SourceDescriptor],
        source_extractors: &'static [SourceExtractor],
    ) -> Self;
}

#[non_exhaustive]
pub struct FieldDescriptor { /* ... */ }
impl FieldDescriptor {
    pub const fn new_const(
        name: &'static str,
        tags: &'static [ResolvedFieldTag],
        shape: FieldShape,
        unit: Option<Unit>,
    ) -> Self;
}

#[non_exhaustive]
pub enum FieldShape { Known(KnownShape), Optional(&'static FieldShape), Flex { .. }, Opaque }

#[non_exhaustive]
pub enum KnownShape { Bool, I64, U64, F64, String, Bytes /* future variants */ }

#[non_exhaustive]
pub enum StringShape { String /* future variants */ }

pub trait Extractable<C> {
    type Snapshot;
    fn snapshot(&self) -> Self::Snapshot;
}

pub trait SourceTag: Any + Send + Sync + 'static {}

pub trait DiscoverableSourceTag: SourceTag {
    fn register_descriptor(registration: SourceRegistration<'static>);
}

#[non_exhaustive]
pub struct SourceRegistration<'a> { pub descriptor: &'a EntryDescriptor }
impl SourceRegistration<'static> {
    pub const fn new(desc: &'static EntryDescriptor) -> Self;
}

pub trait DescribeEntry: Entry {
    const DESCRIPTOR: &'static EntryDescriptor;
    fn extract_source(&self, tag: TypeId) -> Option<Box<dyn Any + Send>> { None }
}
```

### In `metrique-core` (erased entry)

```rust
// Object-safe dyn-trait method:
fn descriptor(&self) -> Option<&'static EntryDescriptor> { None }
```

### Macro attributes

```
#[metrics(default_field_tag(T))]
#[metrics(default_field_tag(skip(T)))]
#[metrics(field_tag(T))]
#[metrics(field_tag(skip(T)))]
#[metrics(source(T))]
#[metrics(no_write)]
```

## Testing plan

- T1. Descriptor round-trip: a representative struct with optionals, Flex, units, and tags. Assert the generated descriptor matches the expected shape.
- T2. Field-tag resolution: every resolution rule from the review's "Field tag resolution: full rules" table, including flatten inheritance.
- T3. `no_write` semantics: field is closed and retained, source extraction sees it, `Entry::write` does not emit it.
- T4. Source extraction round-trip: macro-derived + hand-written both extract through the descriptor.
- T5. `SourceTag` vs `DiscoverableSourceTag`: macro rejects `source(T)` where `T: !SourceTag`; macro compiles fine with `T: SourceTag` and emits registration only when `T: DiscoverableSourceTag`.
- T6. Heterogeneous `BoxEntrySink` carrying both macro-derived and `DescribeEntry`-derived entries produces the expected descriptors for each.
- T7. UI tests (trybuild) for diagnostics on the intrinsic compile-time errors.

## Risks and mitigations

- **`linkme` unavailable on a target.** Metrique's internal use of `linkme` for the source-registration slice is cfg'd on tier-1 targets. On unsupported targets (`wasm32` without feature flags, exotic embedded), the macro can emit the registration call site but the slice exists in a stub form that never iterates. The `DiscoverableSourceTag` contract is "implementations may be called; it is not guaranteed"; sinks that rely on the call have their own target-cfg gating.
- **Scope creep into macro diagnostics.** The intrinsic checks in C6 are narrow and well-defined. Sink-specific diagnostics (InternString on non-string, etc.) are outside M-A/M-C's scope and live in the sink crate's validation.
- **API surface stability before first release.** Every descriptor struct is `#[non_exhaustive]` with `const` constructors. A pre-1.0 release of the descriptor API means we can still iterate; post-1.0, the `#[non_exhaustive]` guarantees evolution without breakage.

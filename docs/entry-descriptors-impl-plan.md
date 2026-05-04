# Entry descriptors: implementation plan

**This document is deleted as part of design sign-off. Keeper is `entry-descriptors.md`; alternatives analysis and rejected shapes are in `entry-descriptors-review.md`.**

Status: nothing here is implemented; this plan captures what the work looks like, in what order, in which files, and which design decisions each piece ties to.

## Sequencing

Three tracks with explicit dependencies. Tracks run in parallel where the graph permits.

### Track M-A: descriptor, source, and field-tag types

Prerequisite for everything else.

- A1. Define `EntryDescriptor`, `FieldDescriptor`, `FieldShape`, `KnownShape`, `StringShape`, `SourceDescriptor`, `SourceExtractor`, `ResolvedFieldTag`, `SourceTag`, and `SourceRegistration` in `metrique-writer-core`. All structs and enums are `#[non_exhaustive]`. Each struct carries a `#[doc(hidden)] pub const fn __metrique_private_new(..)` constructor matching its field order; the macro uses these, the ugly name keeps users away. No public constructor surface ships initially; a cleaner surface arrives with `DescribeEntry`. Ties to: keeper "The descriptor model", "Sources and extractors".
- A2. Public re-exports from the `metrique` crate.

### Track M-B: erased entry vtable hook

Prerequisite for any descriptor-aware sink.

- B1. Add `fn descriptor(&self) -> Option<&'static EntryDescriptor>` to the object-safe dyn-trait backing `BoxEntry` (`metrique-core`). Default returns `None`. SemVer minor (additive method on the internal trait used behind `BoxEntry`). Ties to: keeper "Descriptor lookup"; review "Tradeoffs → Descriptor lookup through the erased vtable".
- B2. Update `BoxEntry::descriptor()` wrapper to forward through the dyn-trait.

### Track M-C: macro attributes and descriptor emission

Depends on M-A and M-B.

- C1. `metrique-macro/src/lib.rs`: accept `default_field_tag(T)`, `default_field_tag(skip(T))`, `field_tag(T)`, `field_tag(skip(T))`, `source(T)`, `no_write`. Parse and validate at expansion time. Ties to: keeper "Field tags", "Sources and extractors", "`no_write`".
- C2. `metrique-macro/src/structs.rs`: generate the `static EntryDescriptor` constant for macro-derived entries. Field order matches `Entry::write` order (declaration order), fields emit with resolved tags and computed `FieldShape`. Recognize `Vec<T>`, `[T]`, and `&[T]` syntactically and lower to `FieldShape::List(inner)` when `T`'s closed shape is `Known(_)` or `Optional(Known(_))` (one layer of optional nesting). Recognize metrique `Flex<(String, T)>` similarly: `Flex { value: Known(_) | Optional(Known(_)) }`. Deeper nesting (nested lists, map-of-list, list-of-map, double-optional) lowers to `FieldShape::Opaque` with a note. Ties to: keeper "The descriptor model", "Opaque trapdoor".
- C3. Generate a `SourceExtractor` per declared `source(C)` in the entry's descriptor. The extractor is a function that reads the relevant field on the closed entry and produces the tag's `SourceTag::Snapshot`. Construction is macro-internal; the stored function pointer does not have a public constructor in the initial release. Ties to: keeper "Sources and extractors", "Looking sources up via the descriptor".
- C4. Generate per-source link-time registration. For each `source(T)` declaration, emit a `linkme`-compatible static that invokes `<T as SourceTag>::register_descriptor(SourceRegistration { descriptor: &DESCRIPTOR })` before `main`. Metrique's internal `linkme` usage is scoped to cfg'd-supported targets; on unsupported targets the registration is compiled out. Ties to: keeper "The `SourceTag` trait"; review "Startup-time discovery mechanism".
- C5. `metrique-macro/src/entry_impl.rs`: `Entry::write` output is consistent with the descriptor's field order; `no_write` fields are omitted from the write path but retained through close.
- C6. Macro-level diagnostics for intrinsic validation: duplicate `source(T)`, conflicting `field_tag(T)` vs `field_tag(skip(T))`, conflicting `default_field_tag` declarations, `source(T)` where `T: !SourceTag`, `no_write + flatten` on the same field. Ties to: keeper "Validation → Compile-time".

Parallelism within Track M-C: C1-C2 are prerequisites for C3-C5. C6 depends on C1. C3 and C4 can proceed in parallel. C5 depends on C1.

## New public APIs at the boundary

Designed here; the canonical signatures live in the keeper and in the generated rustdoc.

### In `metrique-writer-core`

```rust
#[non_exhaustive]
pub struct EntryDescriptor { /* ... */ }

#[non_exhaustive]
pub struct FieldDescriptor { /* ... */ }

#[non_exhaustive]
pub enum FieldShape {
    Known(KnownShape),
    Optional(&'static FieldShape),
    Flex { key: StringShape, value: &'static FieldShape },
    List(&'static FieldShape),
    Opaque,
}

#[non_exhaustive]
pub enum KnownShape { Bool, I64, U64, F64, String, Bytes /* future */ }

#[non_exhaustive]
pub enum StringShape { String /* future */ }

#[non_exhaustive]
pub struct SourceDescriptor { pub tag: TypeId /* ... */ }

#[non_exhaustive]
pub struct SourceExtractor {
    pub tag: TypeId,
    // Internal extractor function; no public constructor in the initial release.
}

pub trait SourceTag: Any + Send + Sync + 'static {
    type Snapshot: Any + Send;
    fn register_descriptor(_registration: SourceRegistration) {}
}

#[non_exhaustive]
pub struct SourceRegistration { pub descriptor: &'static EntryDescriptor }

// Descriptor-side typed extraction
impl EntryDescriptor {
    pub fn source<C: SourceTag>(
        &self,
        entry: &(dyn Any + Send + 'static),
    ) -> Option<C::Snapshot>;
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
- T4. Source extraction round-trip: `desc.source::<C>(entry.inner_any())` returns the typed `C::Snapshot`.
- T5. Startup-time registration: a `SourceTag` with an overridden `register_descriptor` hook sees its registry populated on program startup.
- T6. UI tests (trybuild) for diagnostics on the intrinsic compile-time errors.

## Risks and mitigations

- **Macro-generated extractor construction must preserve `const`-context rules.** The `SourceExtractor`'s stored function pointer is built by macro expansion in `const` context (it lives in a `static` descriptor). If `const fn` coverage on closures or function pointers shifts, the macro may need an intermediate `fn`-item rather than a closure. Mitigation: emit a named `fn` per source rather than relying on closure `const`-ness.
- **`linkme` unavailable on a target.** Metrique's internal use of `linkme` for the source-registration slice is cfg'd on tier-1 targets. On unsupported targets (`wasm32` without feature flags, exotic embedded), the macro emits the registration call site but the static is compiled out. The `SourceTag::register_descriptor` contract is "implementations may be called; it is not guaranteed"; sinks that rely on the call have their own target-cfg gating.
- **Scope creep into sink-specific diagnostics.** The intrinsic checks in C6 are narrow and well-defined. Sink-specific diagnostics (InternString on non-string, etc.) are outside M-A/M-C's scope and live in the sink crate's validation.
- **API surface stability before first release.** Every descriptor struct is `#[non_exhaustive]`. A pre-1.0 release of the descriptor API means we can still iterate on field sets; post-1.0, the `#[non_exhaustive]` + hidden `__metrique_private_new` constructor pattern guarantees evolution without breakage for the macro path. A nicer public constructor surface (positional `new`, builder, or both) lands with `DescribeEntry` when hand-written users start building descriptors.
- **Hidden constructor `const fn` verification.** The macro emits `static DESCRIPTOR: EntryDescriptor = EntryDescriptor::__metrique_private_new(...)` plus chained calls for nested structs. This requires every hidden constructor to be `const fn` and for the stable Rust version we pin to support the specific patterns used (references to arrays, chained function calls in `static` initialisers). Mitigation: implement the constructors with plain positional args, verify with a trybuild smoke test during M-A, and pin the MSRV if needed.

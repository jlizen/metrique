# Entry descriptors and field tags: implementation plan

**This document is deleted as part of design sign-off. Keeper is `entry-descriptors.md`; alternatives and deferred work are in `entry-descriptors-review.md`; changelog is `entry-descriptors-changelog.md`.**

Status: nothing here is implemented. This plan captures what the work looks like, in what order, in which files, and which design decisions each piece ties to.

## Sequencing

Three tracks. Tracks run in parallel where the graph permits.

### Track M-A: descriptor + field tag types

Prerequisite for everything else.

- A1. Define `EntryDescriptor`, `FieldDescriptor`, `TimestampDescriptor`, `FieldShape`, `KnownShape`, `StringShape`, `ShapeRef`, `ResolvedFieldTag`, `FieldTagState`, `DescriptorRef`, `DescriptorId` in `metrique-writer-core`. Descriptor structs have private fields with accessor methods (`.name()`, `.fields()`, `.timestamp()`, `.tags()`, `.shape()`, `.unit()`, `.tag_id()`, `.state()`, `.as_ref()`, `.id()`). All accessors return borrows tied to `&self`, not `&'static`. Enums are `#[non_exhaustive]`. Each struct carries a `#[doc(hidden)] pub const fn __metrique_private_new(..)` constructor matching its field order; the macro uses these, the ugly name keeps users away. Ties to: keeper "The descriptor model", "Forward compatibility".
- A2. `KnownShape` enumerates `Bool`, `U8`, `U16`, `U32`, `U64`, `I8`, `I16`, `I32`, `I64`, `F32`, `F64`, `String`, `Bytes`. Leaves room for future variants (Duration subtypes, timestamps) via `#[non_exhaustive]`.
- A3. `DescriptorRef<'a>` is opaque, backed internally by `&'static EntryDescriptor` today. Internal representation is implementation detail; a future release can add an `Arc`-backed variant for enum-per-variant or hand-written descriptors without an API break.
- A4. `DescriptorId` is opaque, `Copy + Eq + Hash`. In-process stable only; documented explicitly. Derivation (pointer-based for `&'static`, pointer-of-Arc-based for future `Arc` variant) is an implementation detail.
- A5. Public re-exports from the `metrique` crate.

### Track M-B: `Entry::descriptor()`

Prerequisite for any descriptor-aware sink.

- B1. Add `fn descriptor(&self) -> Option<DescriptorRef<'_>> { None }` to the `Entry` trait with a default no-op body. SemVer minor. External `impl Entry` blocks continue to compile unchanged. Ties to: keeper "Descriptor lookup"; review "Tradeoffs → Descriptor lookup through `Entry::descriptor()`".
- B2. Update `BoxEntry` to forward `descriptor()` through its dyn trait object (the method is on the same trait, so this is a matter of ensuring object-safety is preserved and no surface is lost through the boxed wrapper).

### Track M-C: macro attributes and descriptor emission

Depends on M-A and M-B.

- C1. `metrique-macro/src/lib.rs`: accept `default_field_tag(T)`, `default_field_tag(skip(T))`, `field_tag(T)`, `field_tag(skip(T))`. Parse and validate at expansion time. (Note: `source(T)` and `no_write` are not in the initial scope; see the keeper's "Appendix: possible evolution, typed source extraction" and the review's "Deferred: typed source extraction".)
- C2. `metrique-macro/src/structs.rs`: generate the `static EntryDescriptor` constant for macro-derived entries. Field order matches `Entry::write` order (declaration order); timestamp fields and `#[metrics(ignore)]` fields are excluded from `fields()`. Recognise `Vec<T>`, `[T]`, and `&[T]` syntactically and lower to `FieldShape::List(inner)` when `T`'s closed shape is `Known(_)` or `Optional(Known(_))` (one layer of optional nesting). Recognise metrique `Flex<(String, T)>` similarly: `Flex { value: Known(_) | Optional(Known(_)) }`. Recognise `#[metrics(value)]` newtypes and lower to the wrapped scalar's shape when macro-known; user-typed inner fields go to `Opaque`. Deeper nesting lowers to `FieldShape::Opaque` with a note. Ties to: keeper "The descriptor model", "Shape mapping", "Opaque trapdoor", "Interaction with existing `#[metrics(..)]` attributes".
- C3. `metrique-macro/src/structs.rs`: apply the field tag resolution rules from the review's "Field tag resolution: full rules" section. Field-level `field_tag(..)` wins over child-struct default wins over flatten-site default wins over parent default. `skip(T)` inverts. `flatten` site tags propagate to flattened children.
- C4. `metrique-macro/src/entry_impl.rs`: generate `impl Entry::descriptor()` returning `Some(DescriptorRef::from_static(&DESCRIPTOR))`. `Entry::write` output preserves the descriptor-order == write-order contract (see D2 below for the enforcement mechanism).
- C5. `metrique-macro/src/structs.rs`: emit `EntryDescriptor::name()` as the raw Rust struct identifier. A future `#[metrics(entry_name = "...")]` attribute can override; not in V1 scope.
- C6. Macro-level diagnostics for intrinsic validation: conflicting `field_tag(T)` vs `field_tag(skip(T))` on the same field, conflicting `default_field_tag` declarations. Ties to: keeper "Validation → Compile-time".

Parallelism within Track M-C: C1 is a prerequisite for C2-C6. C2 is a prerequisite for C3-C4. C5 and C6 can run with C2-C4.

### Track M-D: validation and testing infrastructure

- D1. Unit test that asserts `EntryDescriptor::fields()` order matches `Entry::write` value-callback order for every test-suite metrique struct. Runs in CI on every change. Catches macro drift.
- D2. Debug-mode runtime check: a metrique-internal `EntryWriter` wrapper that in debug builds records `value(..)` callback order and cross-references against the descriptor's `fields()`. Panics with `debug_assert!` if they diverge. Provided as an optional wrapper in metrique's test-harness helpers; dial9 and other descriptor-aware sinks wrap their inner `EntryWriter` with it during development to catch bugs in their own adapters.
- D3. UI tests (`trybuild`) for the intrinsic compile-time errors from C6.

## New public APIs at the boundary

The shape reviewers are agreeing to. Exact signatures may shift during implementation.

### In `metrique-writer-core`

```rust
pub struct EntryDescriptor { /* private fields */ }
impl EntryDescriptor {
    pub fn name(&self) -> &str;
    pub fn fields(&self) -> &[FieldDescriptor];
    pub fn timestamp(&self) -> Option<TimestampDescriptor>;
}

pub struct FieldDescriptor { /* private fields */ }
impl FieldDescriptor {
    pub fn name(&self) -> &str;
    pub fn tags(&self) -> &[ResolvedFieldTag];
    pub fn shape(&self) -> FieldShape;
    pub fn unit(&self) -> Option<Unit>;
}

pub struct TimestampDescriptor { /* private fields */ }
impl TimestampDescriptor {
    pub fn name(&self) -> &str;
}

#[non_exhaustive]
pub enum FieldShape {
    Known(KnownShape),
    Optional(ShapeRef<'_>),
    Flex { key: StringShape, value: ShapeRef<'_> },
    List(ShapeRef<'_>),
    Opaque,
}

pub struct ShapeRef<'a> { /* private */ }
impl<'a> ShapeRef<'a> {
    pub fn as_ref(&self) -> &FieldShape;
}

#[non_exhaustive]
pub enum KnownShape {
    Bool, U8, U16, U32, U64, I8, I16, I32, I64, F32, F64, String, Bytes,
    // future: Duration subtypes, timestamp variants, etc.
}

#[non_exhaustive]
pub enum StringShape { String /* future */ }

pub struct ResolvedFieldTag { /* private */ }
impl ResolvedFieldTag {
    pub fn tag_id(&self) -> TypeId;
    pub fn state(&self) -> FieldTagState;
}

#[non_exhaustive]
pub enum FieldTagState { Present, Absent }

pub struct DescriptorRef<'a> { /* private */ }
impl<'a> DescriptorRef<'a> {
    pub fn as_ref(&self) -> &EntryDescriptor;
    pub fn id(&self) -> DescriptorId;
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct DescriptorId(/* opaque */);
```

### On the `Entry` trait

```rust
pub trait Entry {
    // existing methods ...
    fn descriptor(&self) -> Option<DescriptorRef<'_>> { None }
}
```

### Macro attributes

```
#[metrics(default_field_tag(T))]
#[metrics(default_field_tag(skip(T)))]
#[metrics(field_tag(T))]
#[metrics(field_tag(skip(T)))]
```

Note: `#[metrics(source(T))]` and `#[metrics(no_write)]` are deferred; see the keeper's appendix.

## Testing plan

- T1. Descriptor round-trip: a representative struct with scalars, optionals (including `Vec<Option<T>>` and `Flex<(String, Option<T>)>`), `Flex`, lists, units, and tags. Assert the generated descriptor matches the expected shape. Include negative cases: `Vec<Vec<T>>`, `Flex<(String, Vec<T>)>`, and `Option<Option<T>>` all lower to `FieldShape::Opaque`.
- T2. Field-tag resolution: every rule from the review's "Field tag resolution: full rules" table, including flatten inheritance, flatten-through-`Option<SubEntry>`, and flatten-site tag propagation.
- T3. `Entry::write` order == descriptor field order: for every test-suite metrique struct, walk `Entry::write` via a test wrapper that records `(name, value)` callback order, assert order matches `fields()`.
- T4. `Entry::descriptor()` round-trip: macro-derived entries return `Some(DescriptorRef::_)`; a hand-written entry with no override returns `None`; `BoxEntry` forwards correctly.
- T5. `DescriptorId` stability: two calls to `descriptor()` on the same entry return `DescriptorRef`s whose ids compare equal. Two different entry types produce distinct ids.
- T6. Accessor-based forward compatibility: a consumer written against the initial accessor set still compiles after a (simulated) new private field is added to a descriptor struct.
- T7. UI tests (trybuild) for diagnostics on the intrinsic compile-time errors.
- T8. Interaction with existing metrique attributes: structs using `rename_all`, `#[metrics(name = "...")]`, `#[metrics(name_exact)]`, `#[metrics(prefix)]`, `#[metrics(timestamp)]`, `#[metrics(ignore)]`, `#[metrics(subfield)]`, `flatten`, `flatten_entry`, `#[metrics(value)]` newtypes. Assert each interaction produces the documented descriptor shape.

## Risks and mitigations

- **Macro-generated descriptor construction must preserve `const`-context rules.** Descriptors are built as `static` constants; every helper constructor must be `const fn`. Mitigation: plain positional args on `__metrique_private_new`, trybuild smoke test during M-A, pin MSRV if a particular `const fn` pattern shifts in stable Rust.
- **Scope creep into the deferred source system.** The boundary between "what ships now" and "what's in the appendix" must not leak into the macro or the public API. Mitigation: no `SourceTag` trait, no `register_descriptor` hook, no `source(T)` or `no_write` attribute parsing in this round. If a test case requires it, the answer is "add it to the deferred-scope follow-up PR."
- **Dial9 integration validation loses sharpness.** Without the source system's link-time discovery, dial9 cannot detect "sink attached, no matching structs in the binary" at startup. Mitigation: dial9 falls back to first-use per-descriptor validation and documents the limitation. When the source system re-opens, dial9 can layer the startup check on top without breaking its initial API.
- **API surface stability before first release.** Descriptor enums are `#[non_exhaustive]`; descriptor structs have private fields with accessor methods; accessor lifetimes are `&self`-tied. Pre-1.0 iteration remains possible on field sets. Post-1.0, all three mechanisms guarantee additive evolution without breakage for the macro path.
- **Write-order contract drift.** If the macro accidentally emits `Entry::write` callbacks in a different order from the descriptor, descriptor-aware sinks produce garbled output. Mitigation: T3 CI test, D2 debug-mode runtime check.

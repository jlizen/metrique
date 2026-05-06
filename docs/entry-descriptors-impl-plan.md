# Entry descriptors and field tags: implementation plan

**This document is deleted as part of design sign-off. Keeper is `entry-descriptors.md`; alternatives and deferred work are in `entry-descriptors-review.md`.**

Status: nothing here is implemented; this plan captures what the work looks like, in what order, in which files, and which design decisions each piece ties to.

## Sequencing

Three tracks. Tracks run in parallel where the graph permits.

### Track M-A: descriptor + field tag types

Prerequisite for everything else.

- A1. Define `EntryDescriptor`, `FieldDescriptor`, `FieldShape`, `KnownShape`, `StringShape`, `ResolvedFieldTag`, `DescriptorRef`, `DescriptorId` in `metrique-writer-core`. Descriptor structs have private fields with accessor methods (`.name()`, `.fields()`, `.tags()`, `.shape()`, `.unit()`). Enums are `#[non_exhaustive]`. Each struct carries a `#[doc(hidden)] pub const fn __metrique_private_new(..)` constructor matching its field order; the macro uses these, the ugly name keeps users away. Ties to: keeper "The descriptor model", "Forward compatibility".
- A2. Public re-exports from the `metrique` crate.

### Track M-B: `Entry::descriptor()`

Prerequisite for any descriptor-aware sink.

- B1. Add `fn descriptor(&self) -> Option<DescriptorRef> { None }` to the `Entry` trait with a default no-op body. SemVer minor. External `impl Entry` blocks continue to compile unchanged. Ties to: keeper "Descriptor lookup"; review "Tradeoffs → Descriptor lookup through `Entry::descriptor()`".
- B2. Update `BoxEntry` to forward `descriptor()` through its dyn trait object (the method is on the same trait, so this is a matter of ensuring object-safety is preserved and no surface is lost through the boxed wrapper).
- B3. `DescriptorRef::Static(&'static EntryDescriptor)` and `DescriptorRef::Shared(Arc<EntryDescriptor>)` variants. `DescriptorId` is derived from the pointer address in both cases (stable across clones of the same `Arc`; stable across calls for the `&'static` case).

### Track M-C: macro attributes and descriptor emission

Depends on M-A and M-B.

- C1. `metrique-macro/src/lib.rs`: accept `default_field_tag(T)`, `default_field_tag(skip(T))`, `field_tag(T)`, `field_tag(skip(T))`, `no_write`. Parse and validate at expansion time. (Note: `source(T)` and `#[metrics(source(...))]` are not in the initial scope; see the keeper's "Appendix: possible evolution, typed source extraction" and the review's "Deferred: typed source extraction".)
- C2. `metrique-macro/src/structs.rs`: generate the `static EntryDescriptor` constant for macro-derived entries. Field order matches `Entry::write` order (declaration order), fields emit with resolved tags and computed `FieldShape`. Recognise `Vec<T>`, `[T]`, and `&[T]` syntactically and lower to `FieldShape::List(inner)` when `T`'s closed shape is `Known(_)` or `Optional(Known(_))` (one layer of optional nesting). Recognise metrique `Flex<(String, T)>` similarly: `Flex { value: Known(_) | Optional(Known(_)) }`. Deeper nesting lowers to `FieldShape::Opaque` with a note. Ties to: keeper "The descriptor model", "Shape mapping", "Opaque trapdoor".
- C3. `metrique-macro/src/entry_impl.rs`: generate `impl Entry::descriptor()` returning `Some(DescriptorRef::Static(&DESCRIPTOR))`. `Entry::write` output is consistent with the descriptor's field order; `no_write` fields are omitted from the write path but retained through close.
- C4. Macro-level diagnostics for intrinsic validation: conflicting `field_tag(T)` vs `field_tag(skip(T))` on the same field, conflicting `default_field_tag` declarations, `no_write + flatten` on the same field. Ties to: keeper "Validation → Compile-time".

Parallelism within Track M-C: C1-C2 are prerequisites for C3-C4. C4 depends on C1.

## New public APIs at the boundary

The shape reviewers are agreeing to. Exact signatures may shift during implementation.

### In `metrique-writer-core`

```rust
pub struct EntryDescriptor { /* private fields */ }
impl EntryDescriptor {
    pub fn name(&self) -> Option<&'static str>;
    pub fn fields(&self) -> &'static [FieldDescriptor];
}

pub struct FieldDescriptor { /* private fields */ }
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
pub enum KnownShape { Bool, I64, U64, F64, String, Bytes /* future */ }

#[non_exhaustive]
pub enum StringShape { String /* future */ }

pub struct DescriptorRef(/* private */);
impl DescriptorRef {
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

    fn descriptor(&self) -> Option<DescriptorRef> { None }
}
```

### Macro attributes

```
#[metrics(default_field_tag(T))]
#[metrics(default_field_tag(skip(T)))]
#[metrics(field_tag(T))]
#[metrics(field_tag(skip(T)))]
#[metrics(no_write)]
```

## Testing plan

- T1. Descriptor round-trip: a representative struct with scalars, optionals (including `Vec<Option<T>>` and `Flex<(String, Option<T>)>`), `Flex`, lists, units, and tags. Assert the generated descriptor matches the expected shape. Include negative cases: `Vec<Vec<T>>`, `Flex<(String, Vec<T>)>`, and `Option<Option<T>>` all lower to `FieldShape::Opaque`.
- T2. Field-tag resolution: every rule from the review's "Field tag resolution: full rules" table, including flatten inheritance and flatten-through-`Option<SubEntry>`.
- T3. `no_write` semantics: field is closed and retained, accessible to consumers holding the closed value, `Entry::write` does not emit it.
- T4. `Entry::descriptor()` round-trip: macro-derived entries return `Some(DescriptorRef::Static(_))`; a hand-written entry with no override returns `None`; `BoxEntry` forwards correctly.
- T5. `DescriptorId` stability: two calls to `descriptor()` on the same entry return `DescriptorRef`s whose ids compare equal.
- T6. Accessor-based forward compatibility: a consumer written against the initial accessor set still compiles after a (simulated) new private field is added to a descriptor struct.
- T7. UI tests (trybuild) for diagnostics on the intrinsic compile-time errors.

## Risks and mitigations

- **Macro-generated descriptor construction must preserve `const`-context rules.** Descriptors are built as `static` constants; every helper constructor must be `const fn`. Mitigation: plain positional args on `__metrique_private_new`, trybuild smoke test during M-A, pin MSRV if a particular `const fn` pattern shifts in stable Rust.
- **Scope creep into the deferred source system.** The boundary between "what ships now" and "what's in the appendix" must not leak in the macro or the public API. Mitigation: no `SourceTag` trait, no `register_descriptor` hook, no `source(T)` attribute parsing in this round. If a test case requires it, the answer is "add it to the deferred-scope follow-up PR."
- **Dial9 integration validation loses sharpness.** Without the source system's link-time discovery, dial9 cannot detect "sink attached, no matching structs in the binary" at startup. Mitigation: dial9 falls back to first-use per-descriptor validation and documents the limitation. When the source system re-opens, dial9 can layer the startup check on top without breaking its initial API.
- **API surface stability before first release.** Descriptor enums are `#[non_exhaustive]`; descriptor structs have private fields with accessor methods. Pre-1.0 iteration remains possible on both. Post-1.0, both mechanisms guarantee additive evolution without breakage for the macro path.

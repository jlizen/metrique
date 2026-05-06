# Entry descriptors: changelog

**This document is deleted as part of PR sign-off. Keeper is `entry-descriptors.md`; alternatives analysis is in `entry-descriptors-review.md`.**

This summarises what changed across rounds of review and why.

## Headline change in round 2 (current)

Scoped down and polished in response to PR reviewer feedback.

### Scope reductions

- **Source system removed from V1.** `SourceTag` trait, `register_descriptor` hook, `SourceRegistration`, `desc.source::<C>()` typed extraction, and `linkme`-backed binary-wide registration all moved to the keeper's "Appendix: possible evolution, typed source extraction." Rationale: the reviewer flagged the surface as "a lot of new traits" for an N=1 consumer (dial9). Ship the minimum that unblocks dial9 (descriptor + field tags); reopen the source system when a second consumer (OTEL, a richer dial9 integration) materialises.
- **`no_write` attribute dropped.** It only paid for itself in combination with the deferred source system; no V1 consumer uses it. Adding it back later is a non-breaking minor change.
- **`DescribeEntry` (hand-written entry) support stays deferred** for the reasons in round 1; no change this round.
- **`#[metrics(source(T))]` attribute is not parsed by the V1 macro.** The attribute comes back with the source-system appendix.

### API shape changes driven by the PR reviewer

- **Descriptor accessors return `&self`-tied borrows** (not `&'static`). Metrique internally stores `&'static` in the initial release but the accessor surface does not expose that lifetime. The reviewer pointed out that `#[non_exhaustive]` alone does not give forward-compat for lifetime changes; narrowing the accessor lifetime up front provides the equivalent guarantee without introducing accessor bloat.
- **Nested `FieldShape` references wrap in `ShapeRef<'_>`**: opaque, `&self`-tied. Lets metrique change internal storage of nested shapes (e.g. to an `Arc`-backed representation for future enum-per-variant descriptors) without breaking consumers.
- **`DescriptorRef<'_>` is opaque**, backed by `&'static EntryDescriptor` today. Future `Arc`-backed variants (needed for enum-per-variant or hand-written descriptors) slot in without API churn.
- **`DescriptorId` is opaque, stable in-process only.** Documented. Cross-process stability left for a future content-hash accessor.
- **`Entry::descriptor()` is a defaulted method on the existing `Entry` trait** (not a separate `ErasedEntry`). The reviewer explicitly asked for this.
- **`EntryDescriptor::name()` exists as a canonical-name accessor.** Returns the raw Rust struct name in V1; a future `#[metrics(entry_name = "...")]` attribute can override.

### Shape coverage

- **`KnownShape` expanded** to the full primitive scalar set (`U8/U16/U32/U64/I8/I16/I32/I64/F32/F64/Bool/String/Bytes`). `#[metrics(value)]` newtypes lower to their wrapped scalar's shape when macro-known.
- **`EntryDescriptor::timestamp() -> Option<TimestampDescriptor>`** exposes `#[metrics(timestamp)]` fields separately from `fields()`. `fields()` excludes timestamps so the descriptor-order == `Entry::write` callback order contract stays clean (timestamps emit via `EntryWriter::timestamp`, not `::value`).
- **`#[metrics(ignore)]` fields excluded** from the descriptor entirely.
- **Subfield structs (`#[metrics(subfield)]` / `subfield_owned`)** don't emit descriptors of their own; their fields appear in the parent via the flatten flow.

### Field tag resolution

Pinned down explicitly. From most-specific to least-specific:

1. Field-level `field_tag(T)` on the child wins.
2. Struct-level `default_field_tag(T)` on the child struct wins over a flatten-site tag.
3. `field_tag(T)` on a flatten site propagates to flattened children as a default.
4. Parent `default_field_tag(T)` fills unspecified.

Rule (3) is the new explicit case that makes the dial9 pattern (`#[metrics(flatten, field_tag(skip(Emit)))] dial9: Dial9Context`) work correctly: the `skip(Emit)` propagates to `Dial9Context`'s fields as their default, so context fields don't end up double-tagged.

### Other additions

- **`ResolvedFieldTag`** defined as an opaque struct with `tag_id()` + `state()` accessors. `FieldTagState::Present | Absent`.
- **`Entry::write` emission order == descriptor field order** as a contract. Macro guarantees by construction; CI test enforces; debug-mode runtime check panics on mismatch.
- **Glossary** at the top of the keeper (reviewer asked for this).
- **English annotations** on the at-a-glance example (reviewer asked for this).
- **Binary cost honestly stated**: per-entry-type `.rodata` footprint, no runtime alloc, no `linkme` in V1.
- **Interaction-with-existing-metrique-attributes section** documents how `rename_all`, `name` / `name_exact`, `prefix`, `timestamp`, `ignore`, `subfield`, `flatten` / `flatten_entry`, `#[metrics(value)]` compose with the descriptor.

## Headline change in round 1

Initial design: entry descriptor + source system + field tags + `no_write` + hand-written `DescribeEntry` support. Addressed the problems of optional-field schema explosion, unbounded `Flex` keys, per-field opt-in, and typed context extraction through one layered API.

The PR reviewer's feedback ("a lot of new traits ... go as simple as possible then expand when N=2") drove round 2's scope-down.

### What round 1 introduced and still stands

- Entry descriptor with ordered fields, tags, optionality, lists, dynamic-key maps, units.
- `FieldShape` enum with `Known / Optional / Flex / List / Opaque` variants.
- Field tag system (`default_field_tag`, `field_tag`, `skip(T)` argument form).
- `Entry::descriptor()` lookup through the erased trait object.
- `#[non_exhaustive]` on descriptor enums for additive-variant forward compat.
- Descriptor structs with private fields + accessor methods for forward compat on struct growth.

### What round 1 deferred

- Runtime `Entry::write` shape fingerprinting as a fallback for hand-written entries (rejected; contradicts the design's structural argument).
- Compile-time `#[derive(TraceEvent)]` on metrique structs (rejected due to `BoxEntry` erasure).
- Per-sink compile-time wire plans (deferred as a strictly-additive performance optimisation on top of the descriptor).
- Heterogeneous values inside `Flex` (deferred until a concrete consumer needs them).
- Nested container recognition beyond one optional layer (deferred; additive macro change when needed).
- `FieldShape::Distribution` for `Histogram<T>` (deferred pending a `DescribeValue` extension).
- Hand-written `DescribeEntry` (deferred; promotes hidden `__metrique_private_new` constructors to a public surface when it lands).

// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! An atomically swappable shared value where each cloned handle captures a
//! snapshot on first read.

use std::sync::{Arc, OnceLock};

use metrique_core::{CloseValue, CloseValueRef};

/// Shared core for [`State`] and [`StateRef`].
struct StateInner<T> {
    swap: Arc<arc_swap::ArcSwap<T>>,
    snap: OnceLock<Arc<T>>,
}

impl<T> StateInner<T> {
    fn new(val: T) -> Self {
        Self {
            swap: Arc::new(arc_swap::ArcSwap::from_pointee(val)),
            snap: OnceLock::new(),
        }
    }

    fn store(&self, val: Arc<T>) {
        self.swap.store(val);
    }

    fn snapshot(&self) -> Arc<T> {
        self.snap.get_or_init(|| self.swap.load_full()).clone()
    }

    fn latest(&self) -> LatestRef<T> {
        LatestRef(self.swap.load())
    }
}

impl<T> Clone for StateInner<T> {
    fn clone(&self) -> Self {
        Self {
            swap: self.swap.clone(),
            snap: OnceLock::new(),
        }
    }
}

impl<T: std::fmt::Debug> StateInner<T> {
    fn debug_fmt(&self, f: &mut std::fmt::Formatter<'_>, name: &str) -> std::fmt::Result {
        let mut d = f.debug_struct(name);
        d.field("current", &*self.swap.load());
        if let Some(snap) = self.snap.get() {
            d.field("snapshot", snap);
        }
        d.finish()
    }
}

/// A cheap, short-lived reference returned by [`State::latest`] and
/// [`StateRef::latest`].
///
/// Always reads the latest value (bypasses the snapshot). This means
/// that the guarded value might differ from metrics emitted by its related
/// [`State`] or [`StateRef`].
///
/// Derefs to `T` without cloning. Not `Send`; for cross-task use, call
/// [`snapshot`](State::snapshot) instead.
pub struct LatestRef<T>(arc_swap::Guard<Arc<T>>);

impl<T> std::ops::Deref for LatestRef<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// An atomically swappable shared value where each cloned handle captures a
/// snapshot on first read.
///
/// In services with shared state that changes at runtime (feature flags,
/// config reloads, routing tables), request handlers need to both read the
/// current value and emit metrics reflecting what they saw. `State`
/// ensures the value captured for metrics matches the value used during
/// processing, even if a background task swaps in a new value mid-request.
///
/// `State` requires `T: Clone` so it can extract the value from the
/// internal `Arc` for closing. If your `T` is not `Clone` but implements
/// [`CloseValueRef`] (i.e. both `CloseValue for T` and `CloseValue for &T`),
/// use [`StateRef`] instead.
///
/// # Usage
///
/// Put a `State<T>` in your metrics struct. Background tasks call
/// [`store`](State::store) to swap in new values; each request
/// [`clone`](Clone::clone)s a handle, and the snapshot is captured
/// automatically when the metric is closed for emission. You only
/// need to call [`snapshot`](State::snapshot) explicitly if your
/// request handler needs the value for its own logic (e.g. branching
/// on a feature flag); calling `snapshot` early also pins the captured
/// value to that point rather than emission time.
///
/// ```rust,ignore
/// // Background task refreshes config on a loop.
/// // Each request clones the handle; emitted metrics reflect the
/// // config that was current when the request first read it.
/// #[metrics(rename_all = "PascalCase")]
/// struct RequestMetrics {
///     operation: &'static str,
///     #[metrics(flatten)]
///     app_config: State<AppConfig>,
/// }
/// ```
///
/// See the [global-state example] for a complete working version.
///
/// [global-state example]: https://github.com/awslabs/metrique/blob/main/metrique/examples/global-state.rs
///
/// # How it works
///
/// All clones of a `State` share the same underlying value. Each clone
/// has its own snapshot slot: the first call to [`snapshot`](State::snapshot)
/// captures the current value, and all subsequent calls on that handle
/// return the same `Arc<T>`. Calling [`clone`](Clone::clone) produces a
/// fresh handle with an empty snapshot slot.
///
/// The typical pattern is: keep one long-lived `State` for background
/// writers to [`store`](State::store) into, and [`clone`](Clone::clone)
/// a handle per request so each request gets its own snapshot.
///
/// For hot paths that need the latest value (bypassing the snapshot and
/// `Arc` clone), use [`latest`](State::latest).
///
/// ```
/// use std::sync::Arc;
/// use metrique_util::State;
///
/// let shared = State::new(String::from("v1"));
///
/// // Clone for a per-request handle.
/// let request = shared.clone();
///
/// // First snapshot captures "v1".
/// assert_eq!(*request.snapshot(), "v1");
///
/// // Background task updates the shared state.
/// shared.store(Arc::new(String::from("v2")));
///
/// // The request handle still sees "v1".
/// assert_eq!(*request.snapshot(), "v1");
///
/// // latest() always sees the current value.
/// assert_eq!(*request.latest(), "v2");
///
/// // A new clone captures the updated value.
/// let next_request = shared.clone();
/// assert_eq!(*next_request.snapshot(), "v2");
/// ```
pub struct State<T>(StateInner<T>);

impl<T> State<T> {
    /// Create a new `State` from an initial value.
    pub fn new(val: T) -> Self {
        Self(StateInner::new(val))
    }

    /// Atomically replace the shared value.
    ///
    /// All handles (including this one) will see the new value on their
    /// next [`snapshot`](State::snapshot), unless they have already
    /// captured one. A handle that has already called `snapshot` is
    /// unaffected; its captured value is immutable.
    pub fn store(&self, val: Arc<T>) {
        self.0.store(val);
    }

    /// Capture and return a snapshot of the current value.
    ///
    /// The first call captures the value; subsequent calls return the
    /// same `Arc<T>`.
    pub fn snapshot(&self) -> Arc<T> {
        self.0.snapshot()
    }

    /// Get a cheap guard for the latest shared value, bypassing the snapshot.
    /// The returned value may differ from what [`snapshot`](State::snapshot)
    /// returns (and from what metrics will emit on close).
    ///
    /// Use [`snapshot`](State::snapshot) for a `Send` handle or when you
    /// need snapshot consistency.
    ///
    /// Returns a [`LatestRef`] that derefs to `T`.
    pub fn latest(&self) -> LatestRef<T> {
        self.0.latest()
    }
}

impl<T> Clone for State<T> {
    /// Clone produces a fresh handle to the same shared value, without a
    /// captured snapshot.
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for State<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.debug_fmt(f, "State")
    }
}

#[diagnostic::do_not_recommend]
impl<T> CloseValue for State<T>
where
    T: Clone + CloseValue,
{
    type Closed = T::Closed;

    fn close(self) -> Self::Closed {
        Arc::unwrap_or_clone(self.snapshot()).close()
    }
}

#[diagnostic::do_not_recommend]
impl<T> CloseValue for &'_ State<T>
where
    T: Clone + CloseValue,
{
    type Closed = T::Closed;

    fn close(self) -> Self::Closed {
        Arc::unwrap_or_clone(self.snapshot()).close()
    }
}

/// Like [`State`], but closes by reference instead of cloning.
///
/// Use this when `T` is not `Clone` but implements [`CloseValueRef`]
/// (i.e. both `CloseValue for T` and `CloseValue for &T`). This is
/// common for `#[metrics(subfield)]` structs containing non-Clone fields
/// like [`OnceLock`](std::sync::OnceLock).
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::OnceLock;
///
/// #[metrics(subfield)]
/// struct Environment {
///     feature_flag: bool,
///     // Populated later; emits None if still empty at close time.
///     resolved_region: OnceLock<&'static str>,
/// }
///
/// #[metrics(rename_all = "PascalCase")]
/// struct RequestMetrics {
///     #[metrics(flatten)]
///     env: StateRef<Environment>,
/// }
/// ```
///
/// See [`State`] for full documentation on snapshot semantics.
pub struct StateRef<T>(StateInner<T>);

impl<T> StateRef<T> {
    /// Create a new `StateRef` from an initial value.
    pub fn new(val: T) -> Self {
        Self(StateInner::new(val))
    }

    /// Atomically replace the shared value. Existing snapshots are unaffected.
    ///
    /// See [`State::store`] for details.
    pub fn store(&self, val: Arc<T>) {
        self.0.store(val);
    }

    /// Capture and return a snapshot of the current value.
    /// The first call pins the snapshot; subsequent calls return the same `Arc<T>`.
    ///
    /// See [`State::snapshot`] for details.
    pub fn snapshot(&self) -> Arc<T> {
        self.0.snapshot()
    }

    /// Get a cheap guard for the latest shared value, bypassing the snapshot.
    /// The returned value may differ from what metrics will emit on close.
    ///
    /// See [`State::latest`] for details.
    pub fn latest(&self) -> LatestRef<T> {
        self.0.latest()
    }
}

impl<T> Clone for StateRef<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for StateRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.debug_fmt(f, "StateRef")
    }
}

#[diagnostic::do_not_recommend]
impl<T> CloseValue for StateRef<T>
where
    T: CloseValueRef,
{
    type Closed = T::Closed;

    fn close(self) -> Self::Closed {
        self.snapshot().close()
    }
}

#[diagnostic::do_not_recommend]
impl<T> CloseValue for &'_ StateRef<T>
where
    T: CloseValueRef,
{
    type Closed = T::Closed;

    fn close(self) -> Self::Closed {
        self.snapshot().close()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use metrique_core::CloseValue;

    use super::{State, StateRef};

    #[derive(Clone, Debug)]
    struct Closeable;
    impl CloseValue for Closeable {
        type Closed = u64;
        fn close(self) -> u64 {
            42
        }
    }

    #[test]
    fn close_ref() {
        let x = State::new(Closeable);
        assert_eq!((&x).close(), 42);
    }

    #[test]
    fn close_owned() {
        let x = State::new(Closeable);
        assert_eq!(x.close(), 42);
    }

    #[test]
    fn first_snapshot_captures() {
        let x = State::new(42u64);
        assert_eq!(*x.snapshot(), 42);
        x.store(Arc::new(100));
        assert_eq!(*x.snapshot(), 42);
    }

    #[test]
    fn store_before_snapshot() {
        let x = State::new(42u64);
        x.store(Arc::new(100));
        assert_eq!(*x.snapshot(), 100);
    }

    #[test]
    fn store_after_snapshot_updates_shared() {
        let x = State::new(42u64);
        x.snapshot();
        x.store(Arc::new(100));
        assert_eq!(*x.snapshot(), 42);
        assert_eq!(*x.clone().snapshot(), 100);
    }

    #[test]
    fn latest_sees_current() {
        let x = State::new(42u64);
        x.store(Arc::new(100));
        assert_eq!(*x.latest(), 100);
    }

    #[test]
    fn clone_gets_fresh_snapshot() {
        let x = State::new(42u64);
        x.snapshot();

        let writer = x.clone();
        writer.store(Arc::new(100));

        let reader = x.clone();
        assert_eq!(*reader.snapshot(), 100);
        assert_eq!(*x.snapshot(), 42);
    }

    #[test]
    fn clone_shares_swap() {
        let x = State::new(42u64);
        let cloned = x.clone();
        x.store(Arc::new(100));
        assert_eq!(*x.latest(), 100);
        assert_eq!(*cloned.latest(), 100);
    }

    #[test]
    fn debug_without_snapshot() {
        let x = State::new(42u64);
        let dbg = format!("{:?}", x);
        assert!(dbg.contains("42"));
        assert!(!dbg.contains("snapshot"));
    }

    #[test]
    fn debug_with_snapshot() {
        let x = State::new(42u64);
        x.snapshot();
        let dbg = format!("{:?}", x);
        assert!(dbg.contains("snapshot"));
    }

    #[derive(Debug)]
    struct NotClone(u64);
    impl CloseValue for NotClone {
        type Closed = u64;
        fn close(self) -> u64 {
            self.0
        }
    }
    impl CloseValue for &'_ NotClone {
        type Closed = u64;
        fn close(self) -> u64 {
            self.0
        }
    }

    #[test]
    fn state_ref_close_owned() {
        let x = StateRef::new(NotClone(99));
        assert_eq!(x.close(), 99);
    }

    #[test]
    fn state_ref_close_ref() {
        let x = StateRef::new(NotClone(99));
        assert_eq!((&x).close(), 99);
    }

    #[test]
    fn state_ref_snapshot() {
        let x = StateRef::new(NotClone(1));
        assert_eq!(x.snapshot().0, 1);
        x.store(Arc::new(NotClone(2)));
        // Snapshot is pinned.
        assert_eq!(x.snapshot().0, 1);
        // Fresh clone sees new value.
        assert_eq!(x.clone().snapshot().0, 2);
    }
}

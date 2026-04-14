// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Deferred sink attachment with bounded entry buffering.
//!
//! See [`new()`] for details.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crossbeam_queue::ArrayQueue;
use metrique_writer_core::{
    entry::BoxEntry,
    sink::{BoxEntrySink, EntrySink, FlushWait},
};

struct Inner {
    buffer: ArrayQueue<BoxEntry>,
    sink: OnceLock<BoxEntrySink>,
    cancelled: AtomicBool,
    /// Number of append() calls currently in the buffering path (between the
    /// sink.get() check and the force_push completion). resolve() waits for
    /// this to reach zero before draining, preventing stranded entries.
    buffering: AtomicUsize,
}

struct PendingSink(Arc<Inner>);

impl EntrySink<BoxEntry> for PendingSink {
    fn append(&self, entry: BoxEntry) {
        if let Some(sink) = self.0.sink.get() {
            sink.append(entry);
        } else if !self.0.cancelled.load(Ordering::Acquire) {
            self.0.buffering.fetch_add(1, Ordering::AcqRel);
            // Re-check after incrementing: if the sink was set between our
            // first check and the increment, forward directly instead.
            if let Some(sink) = self.0.sink.get() {
                self.0.buffering.fetch_sub(1, Ordering::AcqRel);
                sink.append(entry);
            } else {
                // force_push: drop oldest when full, consistent with BackgroundQueue
                self.0.buffer.force_push(entry);
                self.0.buffering.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }

    fn flush_async(&self) -> FlushWait {
        if let Some(sink) = self.0.sink.get() {
            EntrySink::<BoxEntry>::flush_async(sink)
        } else {
            FlushWait::ready()
        }
    }
}

/// Handle for resolving a pending sink created by [`new()`].
///
/// Call [`resolve`](PendingSinkResolver::resolve) to drain buffered entries into the
/// real sink and switch to direct forwarding. If this handle is dropped without
/// calling `resolve`, the pending sink becomes a no-op and buffered entries are
/// discarded.
#[must_use = "if dropped without calling resolve(), buffered entries will be discarded"]
pub struct PendingSinkResolver(Option<Arc<Inner>>);

impl PendingSinkResolver {
    /// Drain all buffered entries into `sink` and switch to direct forwarding.
    ///
    /// After this call, new entries appended to the associated [`BoxEntrySink`] will
    /// go directly to `sink`. This method consumes the resolver.
    pub fn resolve(mut self, sink: BoxEntrySink) {
        if let Some(inner) = self.0.take() {
            // Set the sink so new appends go directly to it.
            let Ok(()) = inner.sink.set(sink) else {
                return;
            };
            // Wait for any in-flight buffering appenders to finish pushing.
            while inner.buffering.load(Ordering::Acquire) != 0 {
                std::hint::spin_loop();
            }
            // Drain all buffered entries.
            let sink = inner.sink.get().unwrap();
            while let Some(entry) = inner.buffer.pop() {
                sink.append(entry);
            }
        }
    }
}

impl Drop for PendingSinkResolver {
    fn drop(&mut self) {
        if let Some(inner) = self.0.take() {
            inner.cancelled.store(true, Ordering::Release);
        }
    }
}

/// Creates a `(BoxEntrySink, PendingSinkResolver)` pair for deferred sink attachment.
///
/// The returned sink can be used immediately. While the resolver has not yet been
/// called, entries are buffered in a bounded ring buffer of the given `capacity`.
/// When the buffer is full, the oldest entry is dropped (consistent with
/// `BackgroundQueue` backpressure behavior).
///
/// Call [`PendingSinkResolver::resolve`] to drain the buffer into the real sink and
/// switch to direct forwarding. If the resolver is dropped without calling `resolve`,
/// buffered entries are discarded and the sink becomes a no-op.
///
/// The hot path (`append` after resolution) is a single atomic load with no locking.
///
/// # Panics
///
/// Panics if `capacity` is 0.
///
/// # Example
///
/// ```
/// use metrique_util::pending_sink;
///
/// let (sink, resolver) = pending_sink::new(1024);
///
/// // Entries are buffered while the real sink initializes
/// // sink.append_any(my_entry);
///
/// // Later, once the real sink is ready:
/// // resolver.resolve(real_sink);
/// // Buffered entries are drained and future entries go directly to real_sink.
///
/// // Or, if initialization fails, just drop the resolver:
/// drop(resolver);
/// // The sink becomes a no-op; buffered entries are discarded.
/// ```
pub fn new(capacity: usize) -> (BoxEntrySink, PendingSinkResolver) {
    assert!(capacity > 0, "pending sink capacity must be greater than 0");
    let inner = Arc::new(Inner {
        buffer: ArrayQueue::new(capacity),
        sink: OnceLock::new(),
        cancelled: AtomicBool::new(false),
        buffering: AtomicUsize::new(0),
    });
    let sink = BoxEntrySink::new(PendingSink(Arc::clone(&inner)));
    let resolver = PendingSinkResolver(Some(inner));
    (sink, resolver)
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrique_writer_core::sink::AnyEntrySink;
    use std::sync::{Arc, Mutex};

    // A simple Entry for testing
    struct TestEntry(u64);
    impl metrique_writer_core::Entry for TestEntry {
        fn write<'a>(&'a self, writer: &mut impl metrique_writer_core::EntryWriter<'a>) {
            writer.value("value", &self.0);
        }
    }

    struct CollectorSink {
        appended: Arc<Mutex<Vec<u64>>>,
        flushes: Arc<Mutex<u64>>,
    }

    impl EntrySink<BoxEntry> for CollectorSink {
        fn append(&self, _entry: BoxEntry) {
            self.appended.lock().unwrap().push(1);
        }
        fn flush_async(&self) -> FlushWait {
            *self.flushes.lock().unwrap() += 1;
            FlushWait::ready()
        }
    }

    fn collector() -> (BoxEntrySink, Arc<Mutex<Vec<u64>>>, Arc<Mutex<u64>>) {
        let appended = Arc::new(Mutex::new(Vec::new()));
        let flushes = Arc::new(Mutex::new(0u64));
        let sink = BoxEntrySink::new(CollectorSink {
            appended: Arc::clone(&appended),
            flushes: Arc::clone(&flushes),
        });
        (sink, appended, flushes)
    }

    #[test]
    fn resolve_drains_buffered_entries() {
        let (sink, resolver) = new(16);

        sink.append_any(TestEntry(1));
        sink.append_any(TestEntry(2));
        sink.append_any(TestEntry(3));

        let (real_sink, appended, _) = collector();
        resolver.resolve(real_sink);

        assert_eq!(appended.lock().unwrap().len(), 3);
    }

    #[test]
    fn entries_forward_after_resolve() {
        let (sink, resolver) = new(16);
        let (real_sink, appended, _) = collector();
        resolver.resolve(real_sink);

        sink.append_any(TestEntry(1));
        sink.append_any(TestEntry(2));

        assert_eq!(appended.lock().unwrap().len(), 2);
    }

    #[test]
    fn flush_forwards_after_resolve() {
        let (sink, resolver) = new(16);
        let (real_sink, _, flushes) = collector();
        resolver.resolve(real_sink);

        let _ = AnyEntrySink::flush_async(&sink);
        assert_eq!(*flushes.lock().unwrap(), 1);
    }

    #[test]
    fn flush_is_noop_while_pending() {
        let (sink, _resolver) = new(16);
        // Should not panic; returns ready immediately
        let _ = AnyEntrySink::flush_async(&sink);
    }

    #[test]
    fn drop_oldest_when_buffer_full() {
        let (sink, resolver) = new(2);

        sink.append_any(TestEntry(1));
        sink.append_any(TestEntry(2));
        sink.append_any(TestEntry(3)); // evicts TestEntry(1)

        let (real_sink, appended, _) = collector();
        resolver.resolve(real_sink);

        // Only 2 entries survive (the buffer capacity)
        assert_eq!(appended.lock().unwrap().len(), 2);
    }

    #[test]
    fn drop_resolver_cancels_and_discards_buffer() {
        let (sink, resolver) = new(16);

        sink.append_any(TestEntry(1));
        sink.append_any(TestEntry(2));

        drop(resolver);

        // After cancellation, new entries are silently discarded
        sink.append_any(TestEntry(3));
        let _ = AnyEntrySink::flush_async(&sink);
    }

    #[test]
    fn resolve_drains_then_forwards_new_entries() {
        let (sink, resolver) = new(16);

        sink.append_any(TestEntry(1));
        sink.append_any(TestEntry(2));

        let (real_sink, appended, _) = collector();
        resolver.resolve(real_sink);

        sink.append_any(TestEntry(3));
        assert_eq!(appended.lock().unwrap().len(), 3);
    }

    #[test]
    fn pending_sink_is_clone_safe() {
        let (sink, resolver) = new(16);
        let sink2 = sink.clone();

        sink.append_any(TestEntry(1));
        sink2.append_any(TestEntry(2));

        let (real_sink, appended, _) = collector();
        resolver.resolve(real_sink);

        assert_eq!(appended.lock().unwrap().len(), 2);

        sink.append_any(TestEntry(3));
        sink2.append_any(TestEntry(4));
        assert_eq!(appended.lock().unwrap().len(), 4);
    }

    #[test]
    #[should_panic(expected = "pending sink capacity must be greater than 0")]
    fn zero_capacity_panics() {
        let _ = new(0);
    }

    #[test]
    fn stress_no_entry_loss_on_concurrent_resolve() {
        use std::sync::Barrier;

        for _ in 0..1000 {
            let n = 100;
            let threads = 4;
            let expected = n * threads;

            let (sink, resolver) = new(expected + 1024);
            let barrier = Arc::new(Barrier::new(threads + 1));

            let (real_sink, appended, _) = collector();

            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    let sink = sink.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        for i in 0..n {
                            sink.append_any(TestEntry(i as u64));
                        }
                    })
                })
                .collect();

            barrier.wait();
            resolver.resolve(real_sink);

            for h in handles {
                h.join().unwrap();
            }

            let got = appended.lock().unwrap().len();
            assert_eq!(got, expected, "lost {} entries", expected - got);
        }
    }
}

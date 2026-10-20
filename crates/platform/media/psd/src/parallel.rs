//! Fork-join decoding of independent channel payloads.
//!
//! A decode call owns the pool it uses: it is spawned when the caller decides
//! the work is large enough to pay for it and dropped at the end of the call.
//! No thread outlives the decode and no pool is cached between calls.

use forkunion::{ThreadPool, Topology};

/// Runs `work` once per item, spreading the items over a fork-join pool.
///
/// Returns `false` when no pool could be created, before any item was touched,
/// and the caller decodes on the calling thread instead. Only the schedule
/// differs: the items produce the same bytes either way.
pub(crate) fn for_each<T: Send>(items: &mut [T], work: impl Fn(&mut T) + Sync) -> bool {
    let Ok(topology) = Topology::new() else {
        return false;
    };
    let threads = topology.logical_cores_count().min(items.len()).max(1);
    let Ok(mut pool) = ThreadPool::try_spawn(&topology, threads) else {
        return false;
    };
    pool.for_slices_mut(items, |_thread, chunk| {
        for item in chunk {
            work(item);
        }
    });
    true
}

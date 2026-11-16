//! Module: mirante_core::parallel
//! Purpose: Splits per-pixel work across CPU cores with scoped threads.
//! Created: 2026-11-16
//! Architecture: Colour transforms and adjustments are embarrassingly parallel;
//! this one helper serves them all so no crate needs a thread-pool dependency.

use std::thread;

/// Below this many items the thread start-up costs more than it saves.
const MIN_ITEMS_PER_THREAD: usize = 64 * 1024;

/// Runs `work` over disjoint chunks of `items` on all available cores.
pub fn for_each_chunk<T: Send>(items: &mut [T], work: impl Fn(&mut [T]) + Sync) {
    for_each_indexed_chunk(items, |_, part| work(part));
}

/// Like `for_each_chunk`, but also passes the index of each chunk's first item.
pub fn for_each_indexed_chunk<T: Send>(items: &mut [T], work: impl Fn(usize, &mut [T]) + Sync) {
    let cores = thread::available_parallelism().map_or(1, usize::from);
    let threads = (items.len() / MIN_ITEMS_PER_THREAD).clamp(1, cores);

    if threads == 1 {
        return work(0, items);
    }
    let chunk = items.len().div_ceil(threads);
    let work = &work;
    thread::scope(|scope| {
        for (index, part) in items.chunks_mut(chunk).enumerate() {
            scope.spawn(move || work(index * chunk, part));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touches_every_item_once() {
        let mut items = vec![1_u32; 300_000];
        for_each_chunk(&mut items, |part| part.iter_mut().for_each(|v| *v += 1));
        assert!(items.iter().all(|&v| v == 2));
    }

    #[test]
    fn indexed_chunks_know_their_position() {
        let mut items = vec![0_usize; 300_000];
        for_each_indexed_chunk(&mut items, |start, part| {
            part.iter_mut().enumerate().for_each(|(offset, v)| *v = start + offset);
        });
        assert!(items.iter().enumerate().all(|(index, &v)| v == index));
    }
}

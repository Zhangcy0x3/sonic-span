use std::cell::UnsafeCell;
use std::fmt::Display;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, PartialEq, Eq)]
#[repr(align(64))]
/// A simple wrapper to ensure the atomic variables are cache-line aligned to prevent false sharing between threads
struct CachePadded<T>(T);

impl CachePadded<AtomicUsize> {
    fn load(&self, order: Ordering) -> usize {
        self.0.load(order)
    }

    fn store(&self, val: usize, order: Ordering) {
        self.0.store(val, order);
    }
}

#[derive(Debug)]
pub struct AudioRingBuffer {
    /// A simple lock-free ring buffer for audio frames, used for buffering audio data between the audio source/sink and the core library
    buffer: UnsafeCell<Vec<f32>>,
    /// The capacity of the buffer in number of audio frames (not bytes)
    capacity: usize,
    /// The absolute number of frames written to the buffer, updated by the core library when it writes data from the audio source
    w: CachePadded<AtomicUsize>,
    /// The absolute number of frames read from the buffer, updated by the core library when it reads data for the audio sink
    r: CachePadded<AtomicUsize>,
}

// SAFETY: AudioRingBuffer is a single-producer single-consumer (SPSC) lock-free
// ring buffer.  Only the writer touches `write_pos` and only the reader touches
// `read_pos`; both atomics use Acquire/Release ordering to form a correct
// happens-before relationship.  The underlying `Vec<f32>` is accessed only at
// indices guarded by those atomics, so sharing a `&AudioRingBuffer` across
// threads is sound as long as at most one writer and one reader exist.
unsafe impl Sync for AudioRingBuffer {}
unsafe impl Send for AudioRingBuffer {}

impl Display for AudioRingBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AudioRingBuffer {{ capacity: {}, w: {}, r: {} }}",
            self.capacity,
            self.w.0.load(Ordering::Relaxed),
            self.r.0.load(Ordering::Relaxed)
        )
    }
}

impl AudioRingBuffer {
    pub fn new(capacity_frames: usize) -> Self {
        // Ensure capacity is a power of 2 for efficient wrapping
        let capacity = capacity_frames.next_power_of_two();
        Self {
            buffer: UnsafeCell::new(vec![0.0; capacity]),
            capacity: capacity,
            w: CachePadded(AtomicUsize::new(0)),
            r: CachePadded(AtomicUsize::new(0)),
        }
    }

    pub fn push(&self, data: &[f32]) -> usize {
        let w = self.w.load(Ordering::Relaxed);
        let r = self.r.load(Ordering::Acquire);
        let available_space = self.capacity - w.wrapping_sub(r);
        let to_write = available_space.min(data.len());
        if to_write == 0 {
            return 0; // Buffer is full, reject all data
        }
        let space_until_end = self.capacity - (w & (self.capacity - 1));
        if space_until_end >= to_write {
            // We can write in one contiguous block
            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    (*self.buffer.get())
                        .as_mut_ptr()
                        .add(w & (self.capacity - 1)),
                    to_write,
                );
            }
        } else {
            // We need to wrap around the end of the buffer
            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    (*self.buffer.get())
                        .as_mut_ptr()
                        .add(w & (self.capacity - 1)),
                    space_until_end,
                );
                std::ptr::copy_nonoverlapping(
                    data.as_ptr().add(space_until_end),
                    (*self.buffer.get()).as_mut_ptr(),
                    to_write - space_until_end,
                );
            }
        }
        self.w.store(w.wrapping_add(to_write), Ordering::Release);
        to_write
    }

    pub fn pop(&self, output: &mut [f32]) -> usize {
        let w = self.w.load(Ordering::Acquire);
        let r = self.r.load(Ordering::Relaxed);
        let available_data = w.wrapping_sub(r);
        let to_read = available_data.min(output.len());
        let space_until_end = self.capacity - (r & (self.capacity - 1));
        if space_until_end >= to_read {
            // We can read in one contiguous block
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (*self.buffer.get()).as_ptr().add(r & (self.capacity - 1)),
                    output.as_mut_ptr(),
                    to_read,
                );
            }
        } else {
            // We need to wrap around the end of the buffer
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (*self.buffer.get()).as_ptr().add(r & (self.capacity - 1)),
                    output.as_mut_ptr(),
                    space_until_end,
                );
                std::ptr::copy_nonoverlapping(
                    (*self.buffer.get()).as_ptr(),
                    output.as_mut_ptr().add(space_until_end),
                    to_read - space_until_end,
                );
            }
        }
        self.r.store(r.wrapping_add(to_read), Ordering::Release);
        to_read
    }
}

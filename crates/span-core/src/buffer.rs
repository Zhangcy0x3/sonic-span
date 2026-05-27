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
    /// The current write position in the buffer, updated by the audio source when it writes new data
    write_pos: CachePadded<AtomicUsize>,
    /// The current read position in the buffer, updated by the core library when it reads data to send over the network
    read_pos: CachePadded<AtomicUsize>,
}

// SAFETY: AudioRingBuffer is a single-producer single-consumer (SPSC) lock-free
// ring buffer.  Only the writer touches `write_pos` and only the reader touches
// `read_pos`; both atomics use Acquire/Release ordering to form a correct
// happens-before relationship.  The underlying `Vec<f32>` is accessed only at
// indices guarded by those atomics, so sharing a `&AudioRingBuffer` across
// threads is sound as long as at most one writer and one reader exist.
unsafe impl Sync for AudioRingBuffer {}

impl Display for AudioRingBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AudioRingBuffer {{ capacity: {}, write_pos: {}, read_pos: {} }}",
            self.capacity,
            self.write_pos.0.load(Ordering::Relaxed),
            self.read_pos.0.load(Ordering::Relaxed)
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
            write_pos: CachePadded(AtomicUsize::new(0)),
            read_pos: CachePadded(AtomicUsize::new(0)),
        }
    }

    pub fn push(&self, data: &[f32]) -> usize {
        let mut written = 0;
        for sample in data.iter() {
            let write_pos = self.write_pos.load(Ordering::Relaxed);
            let next_write_pos = (write_pos + 1) & (self.capacity - 1);
            if next_write_pos == self.read_pos.load(Ordering::Acquire) {
                // Buffer is full, stop writing
                break;
            }
            unsafe {
                *(*self.buffer.get()).as_mut_ptr().add(write_pos) = *sample;
            }
            self.write_pos.store(next_write_pos, Ordering::Release);
            written += 1;
        }
        written
    }

    pub fn pop(&self, output: &mut [f32]) -> usize {
        let mut read = 0;
        for sample in output.iter_mut() {
            let read_pos = self.read_pos.load(Ordering::Relaxed);
            if read_pos == self.write_pos.load(Ordering::Acquire) {
                // Buffer is empty, stop reading
                break;
            }
            *sample = unsafe { *(*self.buffer.get()).as_ptr().add(read_pos) };
            let next_read_pos = (read_pos + 1) & (self.capacity - 1);
            self.read_pos.store(next_read_pos, Ordering::Release);
            read += 1;
        }
        read
    }
}

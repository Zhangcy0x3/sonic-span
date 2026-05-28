use crate::buffer::AudioRingBuffer;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// A deterministic but varied audio-like signal for testing.
fn generate_test_signal(length: usize, seed: f32) -> Vec<f32> {
    (0..length)
        .map(|i| {
            let t = i as f32;
            // Mix of sine + triangle + linear ramp to cover all amplitude ranges
            let sine = (t * 0.01 + seed).sin();
            let tri = ((t * 0.007 + seed) % 2.0 - 1.0).abs() * 2.0 - 1.0;
            let ramp = (t % 1000.0) / 500.0 - 1.0;
            (sine * 0.5 + tri * 0.3 + ramp * 0.2).clamp(-1.0, 1.0)
        })
        .collect()
}

// ── Correctness: Basic Operations ────────────────────────────────────────────

#[test]
fn new_buffer_is_empty() {
    let buf = AudioRingBuffer::new(1024);
    let mut out = vec![0.0f32; 128];
    let read = buf.pop(&mut out);
    assert_eq!(read, 0, "new buffer should have nothing to read");
}

#[test]
fn push_then_pop_exact() {
    let buf = AudioRingBuffer::new(256);
    let input = generate_test_signal(100, 0.0);
    let written = buf.push(&input);
    assert_eq!(written, 100);

    let mut output = vec![0.0f32; 100];
    let read = buf.pop(&mut output);
    assert_eq!(read, 100);
    assert_eq!(output, input, "data must survive round-trip unchanged");
}

#[test]
fn partial_read() {
    let buf = AudioRingBuffer::new(256);
    let input = generate_test_signal(100, 1.0);
    buf.push(&input);

    let mut output = vec![0.0f32; 40];
    let read = buf.pop(&mut output);
    assert_eq!(read, 40);
    assert_eq!(output[..], input[..40]);

    // Remaining 60 should still be readable
    let mut output2 = vec![0.0f32; 60];
    let read2 = buf.pop(&mut output2);
    assert_eq!(read2, 60);
    assert_eq!(output2[..], input[40..]);
}

#[test]
fn partial_write() {
    let buf = AudioRingBuffer::new(256);
    let input = generate_test_signal(200, 2.0);
    // Write first 100
    let w1 = buf.push(&input[..100]);
    assert_eq!(w1, 100);
    // Write next 100
    let w2 = buf.push(&input[100..]);
    assert_eq!(w2, 100);

    let mut output = vec![0.0f32; 200];
    let read = buf.pop(&mut output);
    assert_eq!(read, 200);
    assert_eq!(output, input);
}

#[test]
fn interleaved_push_pop() {
    let buf = AudioRingBuffer::new(256);
    let signal = generate_test_signal(300, 3.0);

    for chunk in signal.chunks(17) {
        buf.push(chunk);
        let mut out = vec![0.0f32; 17];
        let r = buf.pop(&mut out);
        assert_eq!(r, chunk.len());
        assert_eq!(&out[..r], chunk);
    }
}

// ── Correctness: Wrap-Around ─────────────────────────────────────────────────

#[test]
fn wrap_around_sequential() {
    let buf = AudioRingBuffer::new(64); // becomes 64 (next_power_of_two)
    let signal = generate_test_signal(50, 4.0);

    // Fill to near capacity
    buf.push(&signal);
    let mut drain = vec![0.0f32; 40];
    buf.pop(&mut drain);

    // Write more, causing wrap-around
    let more = generate_test_signal(50, 5.0);
    buf.push(&more);

    // Remaining 10 from first push + 40 from second = 50 available (capacity-1 max)
    // Actually: wrote 50, read 40 = 10 remaining. Then wrote 50 but only 64-10-1=53 fit.
    // So 10 + 50 = 60, but cap-1=63, so all 50 fit → 60 readable
    let mut out = vec![0.0f32; 60];
    let read = buf.pop(&mut out);
    assert_eq!(read, 60);
    assert_eq!(&out[..10], &signal[40..]);
    assert_eq!(&out[10..], &more[..]);
}

#[test]
fn wrap_around_with_overwrite_protection() {
    // Buffer size 64, capacity 64
    let buf = AudioRingBuffer::new(64);

    // Write 64 samples (max capacity)
    let signal = generate_test_signal(64, 6.0);
    let written = buf.push(&signal);
    assert_eq!(written, 64);

    // Try to write more — should be rejected
    let extra = generate_test_signal(10, 7.0);
    let overflow = buf.push(&extra);
    assert_eq!(overflow, 0, "must reject writes when full");

    // Read all, then write should succeed
    let mut drain = vec![0.0f32; 64];
    let read = buf.pop(&mut drain);
    assert_eq!(read, 64);
    assert_eq!(drain, signal);

    let written2 = buf.push(&extra);
    assert_eq!(written2, 10);
}

// ── Correctness: Concurrent SPSC ─────────────────────────────────────────────

#[test]
fn concurrent_spsc_ordered() {
    let buf = Arc::new(AudioRingBuffer::new(4096));
    let buf_producer = Arc::clone(&buf);
    let buf_consumer = Arc::clone(&buf);

    let total_samples: usize = 100_000;
    let chunk_size: usize = 64;

    let producer = thread::spawn(move || {
        let mut sent = 0usize;
        let mut seed = 0.0f32;
        while sent < total_samples {
            let remain = total_samples - sent;
            let n = remain.min(chunk_size);
            let chunk = generate_test_signal(n, seed);
            let pushed = buf_producer.push(&chunk);
            sent += pushed;
            seed += 1.0;
            if pushed < n {
                // Back-pressure: spin briefly
                thread::yield_now();
            }
        }
        sent
    });

    let consumer = thread::spawn(move || {
        let mut received = 0usize;
        let mut buf_local = vec![0.0f32; chunk_size];
        while received < total_samples {
            let n = buf_consumer.pop(&mut buf_local);
            received += n;
            if n == 0 {
                thread::yield_now();
            }
        }
        received
    });

    let sent = producer.join().unwrap();
    let recv = consumer.join().unwrap();
    assert_eq!(sent, total_samples);
    assert_eq!(recv, total_samples);
    assert_eq!(sent, recv);
}

#[test]
fn concurrent_spsc_no_duplicates_no_gaps() {
    let buf = Arc::new(AudioRingBuffer::new(2048));
    let buf_producer = Arc::clone(&buf);
    let buf_consumer = Arc::clone(&buf);

    let total: usize = 50_000;
    let chunk: usize = 32;

    // Pre-generate all sequential data to avoid partial-push resequencing issues
    let all_data: Vec<f32> = (0..total).map(|i| i as f32).collect();

    let producer = thread::spawn(move || {
        let mut offset = 0usize;
        while offset < total {
            let n = (total - offset).min(chunk);
            let pushed = buf_producer.push(&all_data[offset..offset + n]);
            offset += pushed;
            if pushed == 0 {
                thread::yield_now();
            }
        }
    });

    let consumer = thread::spawn(move || {
        let mut expected = 0u32;
        let mut local = vec![0.0f32; chunk];
        let mut received = 0usize;
        while received < total {
            let n = buf_consumer.pop(&mut local);
            for &sample in &local[..n] {
                assert_eq!(
                    sample, expected as f32,
                    "gap or duplicate at sample {}: expected {}, got {}",
                    received, expected, sample
                );
                expected += 1;
            }
            received += n;
        }
    });

    producer.join().unwrap();
    consumer.join().unwrap();
}

// ── Throughput ───────────────────────────────────────────────────────────────

#[test]
fn throughput_single_thread_bulk() {
    let buf = AudioRingBuffer::new(1 << 16); // 65536
    let chunk = 4096;
    let rounds = 1000;
    let signal = generate_test_signal(chunk, 0.0);

    let start = Instant::now();
    for _ in 0..rounds {
        buf.push(&signal);
        let mut out = vec![0.0f32; chunk];
        buf.pop(&mut out);
    }
    let elapsed = start.elapsed();
    let total_samples = chunk * rounds * 2; // push + pop
    let throughput = total_samples as f64 / elapsed.as_secs_f64();

    println!(
        "Single-thread throughput: {:.2} Msamples/s ({:.2} MB/s as f32)",
        throughput / 1e6,
        (throughput * 4.0) / 1e6
    );
    // No strict assertion; this is informational, but we sanity-check it's > 0
    assert!(throughput > 0.0);
}

#[test]
fn throughput_concurrent_spsc() {
    let buf = Arc::new(AudioRingBuffer::new(1 << 18)); // 262144
    let buf_producer = Arc::clone(&buf);
    let buf_consumer = Arc::clone(&buf);

    let total: usize = 1_000_000;
    let chunk: usize = 256;

    let start = Instant::now();

    let producer = thread::spawn(move || {
        let signal = generate_test_signal(chunk, 0.0);
        let mut sent = 0usize;
        while sent < total {
            let n = total - sent;
            let push_size = n.min(chunk);
            let pushed = buf_producer.push(&signal[..push_size]);
            sent += pushed;
            if pushed == 0 {
                thread::yield_now();
            }
        }
    });

    let consumer = thread::spawn(move || {
        let mut local = vec![0.0f32; chunk];
        let mut recv = 0usize;
        while recv < total {
            let n = buf_consumer.pop(&mut local);
            recv += n;
            if n == 0 {
                thread::yield_now();
            }
        }
    });

    producer.join().unwrap();
    consumer.join().unwrap();
    let elapsed = start.elapsed();

    let throughput = total as f64 / elapsed.as_secs_f64();
    println!(
        "Concurrent SPSC throughput: {:.2} Msamples/s ({:.2} MB/s as f32)",
        throughput / 1e6,
        (throughput * 4.0) / 1e6
    );
    assert!(throughput > 0.0);
}

// ── Latency ──────────────────────────────────────────────────────────────────

#[test]
fn single_sample_latency() {
    let buf = AudioRingBuffer::new(1024);
    let iterations = 100_000;

    let start = Instant::now();
    for i in 0..iterations {
        let sample = (i as f32).sin();
        buf.push(&[sample]);
        let mut out = [0.0f32; 1];
        buf.pop(&mut out);
    }
    let elapsed = start.elapsed();
    let avg_ns = elapsed.as_nanos() as f64 / iterations as f64;

    println!("Average push+pop latency (1 sample): {:.0} ns", avg_ns);
    assert!(
        avg_ns < 10_000.0,
        "single-sample round-trip should be well under 10 µs"
    );
}

// ── Edge Cases ───────────────────────────────────────────────────────────────

#[test]
fn zero_length_push() {
    let buf = AudioRingBuffer::new(64);
    let written = buf.push(&[]);
    assert_eq!(written, 0);
    let mut out = [0.0f32; 1];
    assert_eq!(buf.pop(&mut out), 0);
}

#[test]
fn zero_length_pop() {
    let buf = AudioRingBuffer::new(64);
    buf.push(&[1.0, 2.0, 3.0]);
    let read = buf.pop(&mut []);
    assert_eq!(read, 0);
    // Data should still be there
    let mut out = [0.0f32; 3];
    assert_eq!(buf.pop(&mut out), 3);
}

#[test]
fn capacity_is_power_of_two_rounded_up() {
    // Push enough to verify capacity works; edge: exactly power-of-2 input
    let buf = AudioRingBuffer::new(100); // rounds up to 128, usable = 128
    let signal = generate_test_signal(128, 9.0);
    let written = buf.push(&signal);
    assert_eq!(written, 128);

    // One more should fail
    assert_eq!(buf.push(&[0.5]), 0);

    let mut out = vec![0.0f32; 128];
    assert_eq!(buf.pop(&mut out), 128);
}

#[test]
fn minimum_capacity() {
    // next_power_of_two(1) = 1, usable = 1 → effectively unusable
    // next_power_of_two(2) = 2, usable = 2
    let buf = AudioRingBuffer::new(2);
    assert_eq!(buf.push(&[1.0]), 1);
    assert_eq!(buf.push(&[1.0]), 1);
    assert_eq!(buf.push(&[2.0]), 0, "buffer should be full after 2 sample");
    let mut out = [0.0f32; 2];
    assert_eq!(buf.pop(&mut out), 2);
    assert_eq!(out[0], 1.0);
    assert_eq!(out[1], 1.0);
}

#[test]
fn alternating_full_empty_cycles() {
    let buf = AudioRingBuffer::new(256); // capacity = 256, usable = 256
    let signal = generate_test_signal(256, 10.0);

    for cycle in 0..10 {
        // Fill
        let w = buf.push(&signal);
        assert_eq!(w, 256, "cycle {cycle}: should write all 256 samples");
        assert_eq!(buf.push(&[0.0]), 0, "cycle {cycle}: should be full");

        // Drain
        let mut out = vec![0.0f32; 256];
        let r = buf.pop(&mut out);
        assert_eq!(r, 256, "cycle {cycle}: should read all 256 samples");
        assert_eq!(out, signal, "cycle {cycle}: data mismatch");
        assert_eq!(
            buf.pop(&mut [0.0f32; 1]),
            0,
            "cycle {cycle}: should be empty"
        );
    }
}

#[test]
fn very_large_capacity() {
    let cap = 1 << 20; // 1,048,576 (power of 2, usable = cap - 1)
    let buf = AudioRingBuffer::new(cap);
    let signal = generate_test_signal(10_000, 11.0);

    let written = buf.push(&signal);
    assert_eq!(written, 10_000);

    let mut out = vec![0.0f32; 10_000];
    let read = buf.pop(&mut out);
    assert_eq!(read, 10_000);
    assert_eq!(out, signal);
}

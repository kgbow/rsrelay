use std::time::Instant;

use rsrelay_proto::{
    seq_diff, ChannelLayout, CodecType, FormatFlags, FrameDuration, PacketHeader, MAX_UDP_PAYLOAD,
    STANDARD_SAMPLE_RATE,
};

/// Fixed ring capacity in packets (`64 * 5ms = 320ms` max reordering window).
pub const JB_CAPACITY: usize = 64;

/// Maximum consecutive missing packets before transitioning back to initial `Buffering` state.
const MAX_CONSECUTIVE_MISSES: usize = 40;

#[derive(Clone, Copy)]
struct PacketSlot {
    header: PacketHeader,
    payload: [u8; MAX_UDP_PAYLOAD],
    occupied: bool,
}

impl Default for PacketSlot {
    fn default() -> Self {
        Self {
            header: PacketHeader {
                seq: 0,
                timestamp: 0,
                flags: FormatFlags::new(CodecType::Opus, ChannelLayout::Stereo, FrameDuration::Ms5),
                payload_len: 0,
            },
            payload: [0u8; MAX_UDP_PAYLOAD],
            occupied: false,
        }
    }
}

/// Statistics tracked by the jitter buffer for telemetry and adaptive tuning.
#[derive(Debug, Default, Clone, Copy)]
pub struct JitterStats {
    pub packets_received: u64,
    pub packets_late_or_dup: u64,
    pub packets_lost: u64,
    pub plc_frames: u64,
    pub fec_recovered_frames: u64,
    pub buffer_resets: u64,
    pub jitter_estimate_us: f64,
}

/// Zero-allocation outcome of popping the next frame deadline from `JitterBuffer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopOutcome {
    /// Normal packet ready to decode; payload has been copied into `out_payload[..payload_len]`.
    Ready {
        header: PacketHeader,
        payload_len: usize,
    },
    /// Sender transmitted a `FLAG_SILENCE` heartbeat packet (`payload_len == 0`).
    SilenceHeartbeat { header: PacketHeader },
    /// Expected packet is missing, but packet `expected_seq + 1` has already arrived
    /// and carries Opus LBRR FEC data (`has_fec`); its payload is in `out_payload[..fec_payload_len]`.
    MissingWithFec { fec_payload_len: usize },
    /// Expected packet is missing and no FEC packet is available; invoke PLC.
    MissingNoFec,
    /// Jitter buffer is warming up to `target_prefetch` packets.
    Buffering,
}

/// Fixed-capacity sequence-indexed adaptive jitter buffer.
pub struct JitterBuffer {
    slots: Box<[PacketSlot; JB_CAPACITY]>,
    expected_seq: u16,
    initialized: bool,
    buffering: bool,
    occupied_count: usize,
    target_prefetch: usize,
    consecutive_misses: usize,
    epoch: Instant,
    last_transit_us: Option<i64>,
    stats: JitterStats,
}

impl JitterBuffer {
    /// Create a new `JitterBuffer` with `target_prefetch` packets (e.g., `3` packets = `15ms` at `5ms/pkt`).
    pub fn new(target_prefetch: usize) -> Self {
        let target_prefetch = target_prefetch.clamp(1, JB_CAPACITY / 2);
        Self {
            slots: Box::new([PacketSlot::default(); JB_CAPACITY]),
            expected_seq: 0,
            initialized: false,
            buffering: true,
            occupied_count: 0,
            target_prefetch,
            consecutive_misses: 0,
            epoch: Instant::now(),
            last_transit_us: None,
            stats: JitterStats::default(),
        }
    }

    pub const fn stats(&self) -> &JitterStats {
        &self.stats
    }

    pub const fn occupied_count(&self) -> usize {
        self.occupied_count
    }

    pub const fn is_buffering(&self) -> bool {
        self.buffering
    }

    pub const fn target_prefetch(&self) -> usize {
        self.target_prefetch
    }

    /// Update the target prefetch depth (e.g., when the sender changes frame duration).
    pub fn set_target_prefetch(&mut self, target_prefetch: usize) {
        self.target_prefetch = target_prefetch.clamp(1, JB_CAPACITY / 2);
    }

    /// Reset the jitter buffer state (e.g., on stream restart or large discontinuity).
    pub fn reset(&mut self) {
        for slot in self.slots.iter_mut() {
            slot.occupied = false;
        }
        self.initialized = false;
        self.buffering = true;
        self.occupied_count = 0;
        self.consecutive_misses = 0;
        self.last_transit_us = None;
        self.stats.buffer_resets += 1;
    }

    /// Insert an incoming UDP audio packet into the sequence ring.
    pub fn push(&mut self, header: PacketHeader, payload: &[u8]) {
        if header.flags.is_reset {
            self.reset();
        }

        // Update RFC 3550 interarrival jitter estimate
        let arrival_us = self.epoch.elapsed().as_micros() as i64;
        let pkt_ts_us =
            (header.timestamp as i64 * 1_000_000) / (STANDARD_SAMPLE_RATE as i64);
        let transit_us = arrival_us.wrapping_sub(pkt_ts_us);
        if let Some(prev_transit) = self.last_transit_us {
            let d = (transit_us.wrapping_sub(prev_transit)).unsigned_abs() as f64;
            // Ignore large discontinuities (> 200ms) caused by DTX / silence pauses
            if d < 200_000.0 {
                self.stats.jitter_estimate_us += (d - self.stats.jitter_estimate_us) / 16.0;
            }
        }
        self.last_transit_us = Some(transit_us);

        if !self.initialized {
            self.expected_seq = header.seq;
            self.initialized = true;
            self.buffering = true;
        }

        let mut diff = seq_diff(header.seq, self.expected_seq);

        if diff < 0 {
            if self.buffering && (-diff as usize) <= self.target_prefetch {
                // Packet arrived slightly out-of-order before playout began; rewind expected_seq
                self.expected_seq = header.seq;
                diff = 0;
            } else {
                self.stats.packets_late_or_dup += 1;
                return;
            }
        }

        // Severe sequence jump exceeding window capacity: resynchronize
        if (diff as usize) >= JB_CAPACITY {
            self.reset();
            self.expected_seq = header.seq;
            self.initialized = true;
        }

        let idx = (header.seq as usize) % JB_CAPACITY;
        let slot = &mut self.slots[idx];

        // Ignore exact duplicate packets
        if slot.occupied && slot.header.seq == header.seq {
            self.stats.packets_late_or_dup += 1;
            return;
        }

        if !slot.occupied {
            self.occupied_count += 1;
        }

        let copy_len = payload.len().min(MAX_UDP_PAYLOAD);
        slot.header = header;
        slot.header.payload_len = copy_len as u16;
        slot.payload[..copy_len].copy_from_slice(&payload[..copy_len]);
        slot.occupied = true;
        self.stats.packets_received += 1;

        if self.buffering && self.occupied_count >= self.target_prefetch {
            self.buffering = false;
            self.consecutive_misses = 0;
        }
    }

    /// Pop the next scheduled frame in sequence order into `out_payload` without heap allocation.
    pub fn pop(&mut self, out_payload: &mut [u8; MAX_UDP_PAYLOAD]) -> PopOutcome {
        if !self.initialized || self.buffering {
            return PopOutcome::Buffering;
        }

        let idx = (self.expected_seq as usize) % JB_CAPACITY;
        if self.slots[idx].occupied && self.slots[idx].header.seq == self.expected_seq {
            let slot = &mut self.slots[idx];
            slot.occupied = false;
            self.occupied_count = self.occupied_count.saturating_sub(1);
            self.consecutive_misses = 0;
            self.expected_seq = self.expected_seq.wrapping_add(1);

            let header = slot.header;
            if header.flags.is_silence || header.payload_len == 0 {
                return PopOutcome::SilenceHeartbeat { header };
            }
            let len = (header.payload_len as usize).min(MAX_UDP_PAYLOAD);
            out_payload[..len].copy_from_slice(&slot.payload[..len]);
            return PopOutcome::Ready {
                header,
                payload_len: len,
            };
        }

        // Expected packet is missing!
        self.stats.packets_lost += 1;
        self.consecutive_misses += 1;
        let missed_seq = self.expected_seq;
        self.expected_seq = self.expected_seq.wrapping_add(1);

        if self.consecutive_misses >= MAX_CONSECUTIVE_MISSES {
            // Stream appears paused or disconnected; re-enter buffering state
            self.reset();
            return PopOutcome::Buffering;
        }

        // Look ahead 1 packet (`missed_seq + 1`) to see if Opus LBRR FEC can recover this frame
        let next_seq = missed_seq.wrapping_add(1);
        let next_idx = (next_seq as usize) % JB_CAPACITY;
        if self.slots[next_idx].occupied
            && self.slots[next_idx].header.seq == next_seq
            && self.slots[next_idx].header.flags.has_fec
        {
            let len = (self.slots[next_idx].header.payload_len as usize).min(MAX_UDP_PAYLOAD);
            out_payload[..len].copy_from_slice(&self.slots[next_idx].payload[..len]);
            self.stats.fec_recovered_frames += 1;
            return PopOutcome::MissingWithFec {
                fec_payload_len: len,
            };
        }

        self.stats.plc_frames += 1;
        PopOutcome::MissingNoFec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_header(seq: u16, has_fec: bool) -> PacketHeader {
        let mut flags = FormatFlags::new(CodecType::Opus, ChannelLayout::Stereo, FrameDuration::Ms5);
        flags.has_fec = has_fec;
        PacketHeader {
            seq,
            timestamp: (seq as u32) * 240,
            flags,
            payload_len: 4,
        }
    }

    #[test]
    fn test_reordering_and_fec_recovery() {
        let mut jb = JitterBuffer::new(2);
        let mut out_buf = [0u8; MAX_UDP_PAYLOAD];

        // Push out-of-order packets: 100, 102 (with FEC) (missing 101!)
        jb.push(dummy_header(100, false), &[1, 2, 3, 4]);
        assert_eq!(jb.pop(&mut out_buf), PopOutcome::Buffering);

        jb.push(dummy_header(102, true), &[9, 9, 9, 9]);
        // Now occupied_count == 2 >= target_prefetch(2), buffering completes

        // Pop 100 -> Ready
        match jb.pop(&mut out_buf) {
            PopOutcome::Ready {
                header,
                payload_len,
            } => {
                assert_eq!(header.seq, 100);
                assert_eq!(&out_buf[..payload_len], &[1, 2, 3, 4]);
            }
            other => panic!("expected Ready for seq 100, got {other:?}"),
        }

        // Pop 101 (missing, but 102 is present with has_fec = true) -> MissingWithFec
        match jb.pop(&mut out_buf) {
            PopOutcome::MissingWithFec { fec_payload_len } => {
                assert_eq!(&out_buf[..fec_payload_len], &[9, 9, 9, 9]);
            }
            other => panic!("expected MissingWithFec for seq 101, got {other:?}"),
        }

        // Pop 102 -> Ready
        match jb.pop(&mut out_buf) {
            PopOutcome::Ready { header, .. } => assert_eq!(header.seq, 102),
            other => panic!("expected Ready for seq 102, got {other:?}"),
        }
    }

    #[test]
    fn test_initial_out_of_order_and_duplicate_rejection() {
        let mut jb = JitterBuffer::new(2);
        let mut out_buf = [0u8; MAX_UDP_PAYLOAD];

        // Packet 51 arrives before 50 during initial buffering
        jb.push(dummy_header(51, false), &[5, 1]);
        jb.push(dummy_header(51, false), &[5, 1]); // duplicate should be ignored
        assert_eq!(jb.occupied_count(), 1);
        assert_eq!(jb.stats().packets_late_or_dup, 1);

        jb.push(dummy_header(50, false), &[5, 0]);
        assert_eq!(jb.occupied_count(), 2);

        match jb.pop(&mut out_buf) {
            PopOutcome::Ready { header, .. } => assert_eq!(header.seq, 50),
            other => panic!("expected seq 50 first, got {other:?}"),
        }
        match jb.pop(&mut out_buf) {
            PopOutcome::Ready { header, .. } => assert_eq!(header.seq, 51),
            other => panic!("expected seq 51 second, got {other:?}"),
        }
    }
}

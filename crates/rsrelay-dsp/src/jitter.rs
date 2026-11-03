use rsrelay_proto::{
    seq_diff, ChannelLayout, CodecType, FormatFlags, FrameDuration, PacketHeader, MAX_UDP_PAYLOAD,
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

/// Outcome of popping the next frame deadline from `JitterBuffer`.
pub enum PopOutcome {
    /// Normal packet ready to decode.
    Ready {
        header: PacketHeader,
        payload: Vec<u8>,
    },
    /// Sender transmitted a `FLAG_SILENCE` heartbeat packet (`payload_len == 0`).
    SilenceHeartbeat { header: PacketHeader },
    /// Expected packet is missing, but packet `expected_seq + 1` has already arrived
    /// and carries Opus LBRR FEC data (`has_fec`).
    MissingWithFec { next_payload: Vec<u8> },
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

    /// Reset the jitter buffer state (e.g., on stream restart or large discontinuity).
    pub fn reset(&mut self) {
        for slot in self.slots.iter_mut() {
            slot.occupied = false;
        }
        self.initialized = false;
        self.buffering = true;
        self.occupied_count = 0;
        self.consecutive_misses = 0;
        self.stats.buffer_resets += 1;
    }

    /// Insert an incoming UDP audio packet into the sequence ring.
    pub fn push(&mut self, header: PacketHeader, payload: &[u8]) {
        if header.flags.is_reset {
            self.reset();
        }

        if !self.initialized {
            self.expected_seq = header.seq;
            self.initialized = true;
            self.buffering = true;
        }

        let diff = seq_diff(header.seq, self.expected_seq);

        // Late or duplicate packet that has already been played out
        if diff < 0 {
            self.stats.packets_late_or_dup += 1;
            return;
        }

        // Severe sequence jump exceeding window capacity: resynchronize
        if (diff as usize) >= JB_CAPACITY {
            self.reset();
            self.expected_seq = header.seq;
            self.initialized = true;
        }

        let idx = (header.seq as usize) % JB_CAPACITY;
        let slot = &mut self.slots[idx];

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

    /// Pop the next scheduled frame in sequence order.
    pub fn pop(&mut self) -> PopOutcome {
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
            let len = header.payload_len as usize;
            return PopOutcome::Ready {
                header,
                payload: slot.payload[..len].to_vec(),
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
            let len = self.slots[next_idx].header.payload_len as usize;
            self.stats.fec_recovered_frames += 1;
            return PopOutcome::MissingWithFec {
                next_payload: self.slots[next_idx].payload[..len].to_vec(),
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

        // Push out-of-order packets: 100, 102 (with FEC), 103 (missing 101!)
        jb.push(dummy_header(100, false), &[1, 2, 3, 4]);
        assert!(matches!(jb.pop(), PopOutcome::Buffering));

        jb.push(dummy_header(102, true), &[9, 9, 9, 9]);
        // Now occupied_count == 2 >= target_prefetch(2), buffering completes

        // Pop 100 -> Ready
        match jb.pop() {
            PopOutcome::Ready { header, payload } => {
                assert_eq!(header.seq, 100);
                assert_eq!(payload, vec![1, 2, 3, 4]);
            }
            _ => panic!("expected Ready for seq 100"),
        }

        // Pop 101 (missing, but 102 is present with has_fec = true) -> MissingWithFec
        match jb.pop() {
            PopOutcome::MissingWithFec { next_payload } => {
                assert_eq!(next_payload, vec![9, 9, 9, 9]);
            }
            _ => panic!("expected MissingWithFec for seq 101"),
        }

        // Pop 102 -> Ready
        match jb.pop() {
            PopOutcome::Ready { header, .. } => assert_eq!(header.seq, 102),
            _ => panic!("expected Ready for seq 102"),
        }
    }
}

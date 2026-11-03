use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Protocol magic bytes: ASCII `"RS"` (`0x5253`).
pub const MAGIC: u16 = 0x5253;

/// Fixed UDP header length in bytes.
pub const HEADER_LEN: usize = 12;

/// Maximum safe UDP audio payload bytes without IP fragmentation on standard 1500 MTU links
/// (`1500 - 20 IPv4 - 8 UDP - 12 Header = 1460`).
pub const MAX_UDP_PAYLOAD: usize = 1460;

/// Maximum total UDP datagram size (`HEADER_LEN + MAX_UDP_PAYLOAD`).
pub const MAX_PACKET_SIZE: usize = HEADER_LEN + MAX_UDP_PAYLOAD;

/// Canonical pipeline sample rate (48,000 Hz) required by Opus and used across the network wire.
pub const STANDARD_SAMPLE_RATE: u32 = 48_000;

/// Current wire protocol version (stored in bits 10..13 of `format_flags`).
pub const PROTOCOL_VERSION: u8 = 1;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("packet buffer too short: expected at least {HEADER_LEN} bytes, got {0}")]
    BufferTooShort(usize),
    #[error("invalid packet magic: expected 0x{MAGIC:04X}, got 0x{0:04X}")]
    InvalidMagic(u16),
    #[error("payload length mismatch: header declared {declared} bytes, buffer has {actual}")]
    PayloadLengthMismatch { declared: usize, actual: usize },
    #[error("unsupported codec id: {0}")]
    UnsupportedCodec(u8),
    #[error("unsupported channel layout id: {0}")]
    UnsupportedChannels(u8),
    #[error("unsupported frame duration id: {0}")]
    UnsupportedFrameDuration(u8),
}

/// Audio wire encoding format (bits `0..2` of `format_flags`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodecType {
    /// Low-delay Opus compressed audio (supports in-band FEC & PLC).
    Opus = 0,
    /// Uncompressed 16-bit signed little-endian PCM (interleaved).
    PcmI16Le = 1,
    /// Uncompressed 32-bit IEEE float little-endian PCM (interleaved).
    PcmF32Le = 2,
}

impl CodecType {
    pub fn from_bits(bits: u8) -> Result<Self, ProtoError> {
        match bits & 0b11 {
            0 => Ok(Self::Opus),
            1 => Ok(Self::PcmI16Le),
            2 => Ok(Self::PcmF32Le),
            other => Err(ProtoError::UnsupportedCodec(other)),
        }
    }
}

/// Channel layout (bits `2..4` of `format_flags`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelLayout {
    Mono = 0,
    Stereo = 1,
}

impl ChannelLayout {
    pub const fn channels(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
        }
    }

    pub fn from_bits(bits: u8) -> Result<Self, ProtoError> {
        match bits & 0b11 {
            0 => Ok(Self::Mono),
            1 => Ok(Self::Stereo),
            other => Err(ProtoError::UnsupportedChannels(other)),
        }
    }
}

/// Audio frame duration at 48 kHz (bits `4..7` of `format_flags`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameDuration {
    /// 2.5 ms = 120 samples per channel at 48 kHz.
    Ms2_5 = 0,
    /// 5.0 ms = 240 samples per channel at 48 kHz.
    Ms5 = 1,
    /// 10.0 ms = 480 samples per channel at 48 kHz.
    Ms10 = 2,
    /// 20.0 ms = 960 samples per channel at 48 kHz.
    Ms20 = 3,
}

impl FrameDuration {
    /// Number of samples per channel in one frame at 48 kHz.
    pub const fn samples_per_channel(self) -> usize {
        match self {
            Self::Ms2_5 => 120,
            Self::Ms5 => 240,
            Self::Ms10 => 480,
            Self::Ms20 => 960,
        }
    }

    /// Frame duration in microseconds.
    pub const fn duration_us(self) -> u64 {
        match self {
            Self::Ms2_5 => 2_500,
            Self::Ms5 => 5_000,
            Self::Ms10 => 10_000,
            Self::Ms20 => 20_000,
        }
    }

    pub fn from_bits(bits: u8) -> Result<Self, ProtoError> {
        match bits & 0b111 {
            0 => Ok(Self::Ms2_5),
            1 => Ok(Self::Ms5),
            2 => Ok(Self::Ms10),
            3 => Ok(Self::Ms20),
            other => Err(ProtoError::UnsupportedFrameDuration(other)),
        }
    }
}

/// Unpacked representation of the 16-bit `format_flags` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatFlags {
    pub codec: CodecType,
    pub channels: ChannelLayout,
    pub frame_duration: FrameDuration,
    /// Silence heartbeat packet (`payload_len == 0`): keeps receiver PLL locked during WASAPI silence.
    pub is_silence: bool,
    /// Packet contains Opus LBRR in-band Forward Error Correction for the previous packet.
    pub has_fec: bool,
    /// Stream discontinuity / reset marker: receiver should flush its jitter buffer.
    pub is_reset: bool,
    /// Wire protocol version (`0..=7`).
    pub version: u8,
}

impl FormatFlags {
    pub const FLAG_SILENCE: u16 = 1 << 7;
    pub const FLAG_FEC: u16 = 1 << 8;
    pub const FLAG_RESET: u16 = 1 << 9;

    pub const fn new(
        codec: CodecType,
        channels: ChannelLayout,
        frame_duration: FrameDuration,
    ) -> Self {
        Self {
            codec,
            channels,
            frame_duration,
            is_silence: false,
            has_fec: false,
            is_reset: false,
            version: PROTOCOL_VERSION,
        }
    }

    /// Pack into a 16-bit unsigned integer for wire transmission.
    #[inline]
    pub const fn to_bits(self) -> u16 {
        let mut bits: u16 = 0;
        bits |= (self.codec as u16) & 0b11;
        bits |= ((self.channels as u16) & 0b11) << 2;
        bits |= ((self.frame_duration as u16) & 0b111) << 4;
        if self.is_silence {
            bits |= Self::FLAG_SILENCE;
        }
        if self.has_fec {
            bits |= Self::FLAG_FEC;
        }
        if self.is_reset {
            bits |= Self::FLAG_RESET;
        }
        bits |= ((self.version as u16) & 0b111) << 10;
        bits
    }

    /// Unpack from the 16-bit wire representation.
    #[inline]
    pub fn from_bits(bits: u16) -> Result<Self, ProtoError> {
        let codec = CodecType::from_bits((bits & 0b11) as u8)?;
        let channels = ChannelLayout::from_bits(((bits >> 2) & 0b11) as u8)?;
        let frame_duration = FrameDuration::from_bits(((bits >> 4) & 0b111) as u8)?;
        let is_silence = (bits & Self::FLAG_SILENCE) != 0;
        let has_fec = (bits & Self::FLAG_FEC) != 0;
        let is_reset = (bits & Self::FLAG_RESET) != 0;
        let version = ((bits >> 10) & 0b111) as u8;

        Ok(Self {
            codec,
            channels,
            frame_duration,
            is_silence,
            has_fec,
            is_reset,
            version,
        })
    }
}

/// 12-byte custom RTP-lite UDP audio packet header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    /// Wrapping packet sequence number.
    pub seq: u16,
    /// Monotonic sample timestamp in 48 kHz ticks.
    pub timestamp: u32,
    /// Unpacked codec, channel layout, frame duration, and control flags.
    pub flags: FormatFlags,
    /// Length of the audio payload immediately following this header.
    pub payload_len: u16,
}

impl PacketHeader {
    /// Serialize this header into a 12-byte array in network byte order (Big-Endian).
    #[inline]
    pub fn encode(&self, out: &mut [u8; HEADER_LEN]) {
        out[0..2].copy_from_slice(&MAGIC.to_be_bytes());
        out[2..4].copy_from_slice(&self.seq.to_be_bytes());
        out[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        out[8..10].copy_from_slice(&self.flags.to_bits().to_be_bytes());
        out[10..12].copy_from_slice(&self.payload_len.to_be_bytes());
    }

    /// Decode a 12-byte header from the start of a UDP datagram.
    #[inline]
    pub fn decode(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < HEADER_LEN {
            return Err(ProtoError::BufferTooShort(buf.len()));
        }
        let magic = u16::from_be_bytes([buf[0], buf[1]]);
        if magic != MAGIC {
            return Err(ProtoError::InvalidMagic(magic));
        }
        let seq = u16::from_be_bytes([buf[2], buf[3]]);
        let timestamp = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let raw_flags = u16::from_be_bytes([buf[8], buf[9]]);
        let payload_len = u16::from_be_bytes([buf[10], buf[11]]);
        let flags = FormatFlags::from_bits(raw_flags)?;

        Ok(Self {
            seq,
            timestamp,
            flags,
            payload_len,
        })
    }

    /// Decode both the header and return the validated payload slice from a UDP datagram.
    #[inline]
    pub fn decode_packet(datagram: &[u8]) -> Result<(Self, &[u8]), ProtoError> {
        let header = Self::decode(datagram)?;
        let declared = header.payload_len as usize;
        let actual = datagram.len() - HEADER_LEN;
        if actual < declared {
            return Err(ProtoError::PayloadLengthMismatch { declared, actual });
        }
        Ok((header, &datagram[HEADER_LEN..HEADER_LEN + declared]))
    }
}

/// RFC 1982 serial number arithmetic for `u16` sequence numbers.
///
/// Returns a signed difference `a - b` accounting for 16-bit wrap-around:
/// - `> 0`: `a` is newer than `b`
/// - `== 0`: `a` and `b` are identical
/// - `< 0`: `a` is older than `b`
#[inline]
pub const fn seq_diff(a: u16, b: u16) -> i16 {
    a.wrapping_sub(b) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_roundtrip() {
        let mut flags = FormatFlags::new(CodecType::Opus, ChannelLayout::Stereo, FrameDuration::Ms5);
        flags.has_fec = true;

        let hdr = PacketHeader {
            seq: 65534,
            timestamp: 12_345_678,
            flags,
            payload_len: 128,
        };

        let mut wire = [0u8; HEADER_LEN];
        hdr.encode(&mut wire);

        let decoded = PacketHeader::decode(&wire).expect("should decode valid header");
        assert_eq!(decoded, hdr);
        assert!(decoded.flags.has_fec);
        assert!(!decoded.flags.is_silence);
    }

    #[test]
    fn test_seq_diff_wraparound() {
        assert_eq!(seq_diff(10, 5), 5);
        assert_eq!(seq_diff(5, 10), -5);
        // Wrap-around across u16::MAX (65535 -> 0 -> 1)
        assert_eq!(seq_diff(0, 65535), 1);
        assert_eq!(seq_diff(2, 65534), 4);
        assert_eq!(seq_diff(65534, 2), -4);
    }
}

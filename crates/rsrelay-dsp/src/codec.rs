use opus::{Application, Bitrate, Channels, Decoder as OpusDecoder, Encoder as OpusEncoder};
use rsrelay_proto::{ChannelLayout, CodecType, MAX_UDP_PAYLOAD, STANDARD_SAMPLE_RATE};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("opus codec error: {0}")]
    Opus(#[from] opus::Error),
    #[error("payload buffer too small: need {needed} bytes, have {available}")]
    BufferTooSmall { needed: usize, available: usize },
    #[error("corrupted raw PCM payload length: {0} bytes")]
    InvalidPcmPayloadLength(usize),
}

/// Unified real-time audio encoder supporting Opus (`LowDelay` + In-Band FEC) and Raw PCM (`i16`/`f32`).
pub struct AudioEncoder {
    codec: CodecType,
    channels: ChannelLayout,
    opus: Option<OpusEncoder>,
}

impl AudioEncoder {
    pub fn new(
        codec: CodecType,
        channels: ChannelLayout,
        bitrate_bps: u32,
        enable_fec: bool,
    ) -> Result<Self, CodecError> {
        let opus = if codec == CodecType::Opus {
            let opus_ch = match channels {
                ChannelLayout::Mono => Channels::Mono,
                ChannelLayout::Stereo => Channels::Stereo,
            };
            let mut enc = OpusEncoder::new(STANDARD_SAMPLE_RATE, opus_ch, Application::LowDelay)?;
            enc.set_bitrate(Bitrate::Bits(bitrate_bps as i32))?;
            enc.set_inband_fec(enable_fec)?;
            if enable_fec {
                // Inform Opus of expected packet loss (~5%) so it embeds LBRR FEC data on transients
                enc.set_packet_loss_perc(5)?;
            }
            Some(enc)
        } else {
            None
        };

        Ok(Self {
            codec,
            channels,
            opus,
        })
    }

    pub const fn codec(&self) -> CodecType {
        self.codec
    }

    pub const fn channels(&self) -> ChannelLayout {
        self.channels
    }

    /// Encode an interleaved `f32` PCM frame into `out_payload`, returning the encoded byte length.
    ///
    /// Zero heap allocations on the hot path.
    pub fn encode(
        &mut self,
        pcm_interleaved: &[f32],
        out_payload: &mut [u8; MAX_UDP_PAYLOAD],
    ) -> Result<usize, CodecError> {
        match self.codec {
            CodecType::Opus => {
                let enc = self.opus.as_mut().expect("opus encoder initialized");
                let len = enc.encode_float(pcm_interleaved, out_payload)?;
                Ok(len)
            }
            CodecType::PcmI16Le => {
                let needed = pcm_interleaved.len() * 2;
                if needed > out_payload.len() {
                    return Err(CodecError::BufferTooSmall {
                        needed,
                        available: out_payload.len(),
                    });
                }
                for (i, &sample) in pcm_interleaved.iter().enumerate() {
                    let clamped = sample.clamp(-1.0, 1.0);
                    let scaled = (clamped * 32767.0) as i16;
                    let bytes = scaled.to_le_bytes();
                    out_payload[i * 2] = bytes[0];
                    out_payload[i * 2 + 1] = bytes[1];
                }
                Ok(needed)
            }
            CodecType::PcmF32Le => {
                let needed = pcm_interleaved.len() * 4;
                if needed > out_payload.len() {
                    return Err(CodecError::BufferTooSmall {
                        needed,
                        available: out_payload.len(),
                    });
                }
                for (i, &sample) in pcm_interleaved.iter().enumerate() {
                    let bytes = sample.clamp(-1.0, 1.0).to_le_bytes();
                    out_payload[i * 4..i * 4 + 4].copy_from_slice(&bytes);
                }
                Ok(needed)
            }
        }
    }
}

/// Unified real-time audio decoder supporting Opus (normal, LBRR FEC, and PLC) and Raw PCM (`i16`/`f32`).
pub struct AudioDecoder {
    codec: CodecType,
    channels: ChannelLayout,
    opus: Option<OpusDecoder>,
    /// Stores the last decoded frame for smooth fade-out PLC when using Raw PCM modes.
    last_pcm_frame: Vec<f32>,
}

impl AudioDecoder {
    pub fn new(codec: CodecType, channels: ChannelLayout) -> Result<Self, CodecError> {
        let opus_ch = match channels {
            ChannelLayout::Mono => Channels::Mono,
            ChannelLayout::Stereo => Channels::Stereo,
        };
        let opus = if codec == CodecType::Opus {
            Some(OpusDecoder::new(STANDARD_SAMPLE_RATE, opus_ch)?)
        } else {
            None
        };

        Ok(Self {
            codec,
            channels,
            opus,
            last_pcm_frame: Vec::with_capacity(960 * channels.channels()),
        })
    }

    /// Decode a received audio packet into `out_interleaved`.
    ///
    /// `out_interleaved.len()` must equal `samples_per_channel * channels`.
    pub fn decode(
        &mut self,
        payload: &[u8],
        out_interleaved: &mut [f32],
    ) -> Result<usize, CodecError> {
        let ch = self.channels.channels();
        match self.codec {
            CodecType::Opus => {
                let dec = self.opus.as_mut().expect("opus decoder initialized");
                let samples_per_ch = dec.decode_float(payload, out_interleaved, false)?;
                Ok(samples_per_ch * ch)
            }
            CodecType::PcmI16Le => {
                if payload.len() % 2 != 0 {
                    return Err(CodecError::InvalidPcmPayloadLength(payload.len()));
                }
                let count = (payload.len() / 2).min(out_interleaved.len());
                for i in 0..count {
                    let raw = i16::from_le_bytes([payload[i * 2], payload[i * 2 + 1]]);
                    out_interleaved[i] = (raw as f32) / 32768.0;
                }
                self.save_last_frame(&out_interleaved[..count]);
                Ok(count)
            }
            CodecType::PcmF32Le => {
                if payload.len() % 4 != 0 {
                    return Err(CodecError::InvalidPcmPayloadLength(payload.len()));
                }
                let count = (payload.len() / 4).min(out_interleaved.len());
                for i in 0..count {
                    let raw = f32::from_le_bytes([
                        payload[i * 4],
                        payload[i * 4 + 1],
                        payload[i * 4 + 2],
                        payload[i * 4 + 3],
                    ]);
                    out_interleaved[i] = raw;
                }
                self.save_last_frame(&out_interleaved[..count]);
                Ok(count)
            }
        }
    }

    /// Recover a lost packet using Opus In-Band Forward Error Correction (FEC) from `next_packet_payload`,
    /// or using Packet Loss Concealment (PLC) when `next_packet_payload` is `None`.
    pub fn conceal_loss(
        &mut self,
        next_packet_payload: Option<&[u8]>,
        out_interleaved: &mut [f32],
    ) -> Result<usize, CodecError> {
        let ch = self.channels.channels();
        match self.codec {
            CodecType::Opus => {
                let dec = self.opus.as_mut().expect("opus decoder initialized");
                let samples_per_ch = match next_packet_payload {
                    Some(next_pkt) => dec.decode_float(next_pkt, out_interleaved, true)?,
                    None => dec.decode_float(&[], out_interleaved, false)?,
                };
                Ok(samples_per_ch * ch)
            }
            CodecType::PcmI16Le | CodecType::PcmF32Le => {
                // Smooth exponential decay from last known PCM sample values to prevent click/pop
                let total = out_interleaved.len();
                if self.last_pcm_frame.len() >= ch {
                    let last_l = self.last_pcm_frame[self.last_pcm_frame.len() - ch];
                    let last_r = if ch > 1 {
                        self.last_pcm_frame[self.last_pcm_frame.len() - 1]
                    } else {
                        last_l
                    };
                    let frames = total / ch;
                    for f in 0..frames {
                        let decay = 1.0 - (f as f32 / frames as f32);
                        let attenuation = decay * decay * 0.5;
                        out_interleaved[f * ch] = last_l * attenuation;
                        if ch > 1 {
                            out_interleaved[f * ch + 1] = last_r * attenuation;
                        }
                    }
                    self.last_pcm_frame.fill(0.0);
                } else {
                    out_interleaved.fill(0.0);
                }
                Ok(total)
            }
        }
    }

    #[inline]
    fn save_last_frame(&mut self, samples: &[f32]) {
        self.last_pcm_frame.clear();
        self.last_pcm_frame.extend_from_slice(samples);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opus_encode_decode_and_plc() {
        let mut enc = AudioEncoder::new(CodecType::Opus, ChannelLayout::Stereo, 128_000, true)
            .expect("init opus encoder");
        let mut dec = AudioDecoder::new(CodecType::Opus, ChannelLayout::Stereo)
            .expect("init opus decoder");

        let input_pcm = vec![0.2f32; 240 * 2]; // 5ms stereo @ 48kHz
        let mut payload = [0u8; MAX_UDP_PAYLOAD];
        let encoded_len = enc.encode(&input_pcm, &mut payload).expect("encode opus");
        assert!(encoded_len > 0 && encoded_len < 200);

        let mut decoded_pcm = vec![0.0f32; 240 * 2];
        let samples = dec
            .decode(&payload[..encoded_len], &mut decoded_pcm)
            .expect("decode opus");
        assert_eq!(samples, 480);

        // Test PLC packet loss concealment
        let plc_samples = dec
            .conceal_loss(None, &mut decoded_pcm)
            .expect("opus PLC decode");
        assert_eq!(plc_samples, 480);
    }

    #[test]
    fn test_pcm_i16_roundtrip() {
        let mut enc = AudioEncoder::new(CodecType::PcmI16Le, ChannelLayout::Stereo, 0, false)
            .expect("init pcm encoder");
        let mut dec = AudioDecoder::new(CodecType::PcmI16Le, ChannelLayout::Stereo)
            .expect("init pcm decoder");

        let input_pcm = vec![0.5f32, -0.5, 0.25, -0.25];
        let mut payload = [0u8; MAX_UDP_PAYLOAD];
        let len = enc.encode(&input_pcm, &mut payload).expect("encode i16");
        assert_eq!(len, 8);

        let mut decoded = vec![0.0f32; 4];
        let count = dec.decode(&payload[..len], &mut decoded).expect("decode i16");
        assert_eq!(count, 4);
        assert!((decoded[0] - 0.5).abs() < 0.001);
        assert!((decoded[1] + 0.5).abs() < 0.001);
    }
}

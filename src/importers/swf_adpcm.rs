//! Flash/SWF ADPCM decoder (`AudioCompression::Adpcm`, format code 1 -- what
//! ffmpeg calls `adpcm_swf`).
//!
//! This is the single most common codec in Flash-era soundboards, and until
//! now the ripper skipped every one of them. Written from the Adobe SWF File
//! Format Specification v19 ("ADPCM sound data", pp. 186-188), which prints
//! the field layout and all four index tables verbatim, plus the standard
//! public-domain IMA step table the spec defers to (Jack Jansen's `adpcm.c`).
//! Deliberately NOT transliterated from ffmpeg's `adpcm.c`, which is LGPL --
//! this crate is MIT.
//!
//! Everything here is bounds-checked and non-panicking: it parses an
//! untrusted file on a worker thread, so a truncated or hostile payload must
//! yield a short buffer, never an index-out-of-bounds or a huge allocation.

use anyhow::{Result, bail};

/// Standard 89-entry IMA ADPCM step table. SWF uses it unmodified; the spec
/// points at Jansen's public-domain source rather than printing it.
const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

// Step-index adjustment, one table per code width. SWF19 prints these as
// `indexTable2/3/4/5`. They are indexed by the code's MAGNITUDE (sign bit
// cleared), which is `bits - 1` wide, hence 2^(bits-1) entries each.
//
// Note INDEX_TABLE_5[8] is 1, not 2 -- the 4-bit pattern does not simply
// scale up, and that single entry is the classic transcription bug.
const INDEX_TABLE_2: [i32; 2] = [-1, 2];
const INDEX_TABLE_3: [i32; 4] = [-1, -1, 2, 4];
const INDEX_TABLE_4: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];
const INDEX_TABLE_5: [i32; 16] = [-1, -1, -1, -1, -1, -1, -1, -1, 1, 2, 4, 6, 8, 10, 13, 16];

/// Indexed by `AdpcmCodeSize` (0..=3), i.e. `bits_per_code - 2`.
const INDEX_TABLES: [&[i32]; 4] = [&INDEX_TABLE_2, &INDEX_TABLE_3, &INDEX_TABLE_4, &INDEX_TABLE_5];

/// Sample frames per ADPCM block: one seed sample plus 4095 coded ones.
const BLOCK_FRAMES: usize = 4096;
/// Bits of per-channel block header: `InitialSample SI16` + `InitialIndex UB[6]`.
const HEADER_BITS: usize = 22;

/// MSB-first bit reader. SWF ADPCM is never byte-aligned after the leading
/// 2-bit code size -- block headers routinely straddle byte boundaries -- so
/// this deliberately has no re-align operation.
struct BitReader<'a> {
    data: &'a [u8],
    /// Position in *bits* from the start of `data`.
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[inline]
    fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    /// Reads `n` (<= 16) bits MSB-first, or `None` if the stream is exhausted.
    #[inline]
    fn read(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        if n as usize > self.bits_left() {
            return None;
        }
        // Walk byte by byte rather than bit by bit -- at most three iterations
        // for n <= 16, which matters when a single .swf holds millions of
        // samples across ~90 sounds.
        let mut value: u32 = 0;
        let mut remaining = n;
        while remaining > 0 {
            let bit_offset = (self.pos & 7) as u32;
            let available = 8 - bit_offset;
            let take = available.min(remaining);
            let byte = self.data[self.pos >> 3] as u32;
            let chunk = (byte >> (available - take)) & ((1u32 << take) - 1);
            value = (value << take) | chunk;
            self.pos += take as usize;
            remaining -= take;
        }
        Some(value)
    }

    #[inline]
    fn read_i16(&mut self) -> Option<i32> {
        self.read(16).map(|v| v as u16 as i16 as i32)
    }
}

/// Decodes one SWF ADPCM stream into interleaved little-endian signed 16-bit
/// PCM bytes, ready to hand straight to `swf_rip::write_wav`.
///
/// `data` must start at the 2-bit `AdpcmCodeSize` header. For a `DefineSound`
/// tag that is `sound.data[0]` -- unlike MP3, ADPCM has no `SeekSamples`
/// prefix to skip. For streaming audio, each `SoundStreamBlock` is its own
/// independent ADPCM stream and must be passed here separately.
///
/// `num_samples` is SWF's `SoundSampleCount` (frames, not samples, so it is
/// the same number for mono and stereo). When non-zero the output is trimmed
/// to it, since the last block is usually partial and decodes a little long.
/// Pass 0 when it isn't known, as it isn't for stream blocks.
pub fn decode(data: &[u8], is_stereo: bool, num_samples: u32) -> Result<Vec<u8>> {
    let channels = if is_stereo { 2usize } else { 1 };
    let mut reader = BitReader::new(data);

    let Some(code_size) = reader.read(2) else {
        bail!("ADPCM payload is empty");
    };
    let code_size = code_size as usize; // 0..=3, from a 2-bit field
    let bits = code_size as u32 + 2; // 2..=5
    let index_table = INDEX_TABLES[code_size];
    let sign_mask: u32 = 1 << (bits - 1);
    let first_magnitude_bit: u32 = 1 << (bits - 2);

    // Size the buffer from the payload, never from the file's own sample
    // count -- a hostile num_samples must not turn into a huge allocation.
    let estimated_frames = (data.len() * 8) / (bits as usize * channels) + BLOCK_FRAMES;
    let mut out: Vec<i16> = Vec::with_capacity(estimated_frames.min(1 << 22) * channels);

    let mut predictor = [0i32; 2];
    let mut index = [0i32; 2];

    'blocks: while reader.bits_left() >= HEADER_BITS * channels {
        // Block header, per channel, left then right: SI16 sample, UB[6] index.
        for c in 0..channels {
            let (Some(sample), Some(idx)) = (reader.read_i16(), reader.read(6)) else {
                break 'blocks;
            };
            predictor[c] = sample;
            index[c] = (idx as i32).clamp(0, 88);
            // The seed is itself an output sample -- SWF19 calls it
            // "identical to first sample in uncompressed sound". (Ruffle
            // drops it and emits 4095 frames per block; ffmpeg emits 4096,
            // which is what the spec says.)
            out.push(predictor[c] as i16);
        }

        for _ in 0..BLOCK_FRAMES - 1 {
            if reader.bits_left() < bits as usize * channels {
                // A frame is all channels or none, so stop here. This also
                // implies fewer than HEADER_BITS * channels bits remain, so
                // the outer loop would exit anyway.
                break 'blocks;
            }
            for c in 0..channels {
                let Some(code) = reader.read(bits) else {
                    break 'blocks;
                };

                // vpdiff = (magnitude + 0.5) * step / 2^(bits - 2), built by
                // halving `step` down the magnitude bits; the trailing add is
                // the "+ 0.5" rounding term, i.e. step >> (bits - 1). It is
                // NOT a fixed step >> 3 -- that constant is 4-bit-only.
                let mut step = STEP_TABLE[index[c] as usize];
                let mut vpdiff = 0i32;
                let mut bit = first_magnitude_bit;
                loop {
                    if code & bit != 0 {
                        vpdiff += step;
                    }
                    step >>= 1;
                    bit >>= 1;
                    if bit == 0 {
                        break;
                    }
                }
                vpdiff += step;

                // Codes are sign-magnitude, not two's complement.
                if code & sign_mask != 0 {
                    predictor[c] -= vpdiff;
                } else {
                    predictor[c] += vpdiff;
                }
                predictor[c] = predictor[c].clamp(-32768, 32767);
                index[c] = (index[c] + index_table[(code & !sign_mask) as usize]).clamp(0, 88);

                out.push(predictor[c] as i16);
            }
        }
    }

    // The final block is usually partial, so the payload can carry a few
    // frames beyond what the sound actually declares. Trim to the declared
    // count -- but never pad up to it: a file that claims more frames than it
    // encodes doesn't have them, and appending silence would both invent
    // audio and risk a click at the tail.
    if num_samples > 0 {
        out.truncate(num_samples as usize * channels);
    }

    let mut bytes = Vec::with_capacity(out.len() * 2);
    for sample in out {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MSB-first bit writer, the mirror of `BitReader`, so tests can
    /// hand-assemble ADPCM streams without shipping a binary fixture.
    struct BitWriter {
        data: Vec<u8>,
        bits: u32,
    }

    impl BitWriter {
        fn new() -> Self {
            Self { data: Vec::new(), bits: 0 }
        }

        fn write(&mut self, value: u32, n: u32) {
            for i in (0..n).rev() {
                if self.bits % 8 == 0 {
                    self.data.push(0);
                }
                let bit = ((value >> i) & 1) as u8;
                let last = self.data.len() - 1;
                self.data[last] |= bit << (7 - (self.bits % 8));
                self.bits += 1;
            }
        }

        fn write_i16(&mut self, value: i16) {
            self.write(value as u16 as u32, 16);
        }
    }

    fn samples(bytes: &[u8]) -> Vec<i16> {
        bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
    }

    /// The highest-value single test: an all-zero-code block must decode to
    /// 4096 copies of the seed. With index 0 the step is 7, and 7 >> 3 == 0,
    /// so every delta is zero and the index walks down and clamps at 0.
    /// Catches a dropped seed sample, wrong bit order, and a missing index
    /// clamp all at once.
    #[test]
    fn all_zero_codes_hold_the_seed_for_a_whole_block() {
        let mut w = BitWriter::new();
        w.write(2, 2); // AdpcmCodeSize 2 -> 4 bits per code
        w.write_i16(1234); // seed
        w.write(0, 6); // initial step index
        for _ in 0..BLOCK_FRAMES - 1 {
            w.write(0, 4);
        }

        let pcm = samples(&decode(&w.data, false, 0).unwrap());
        assert_eq!(pcm.len(), BLOCK_FRAMES, "a block is 4096 frames: seed + 4095 codes");
        assert!(pcm.iter().all(|&s| s == 1234), "zero codes must not move the predictor");
    }

    /// Known-vector check of the delta arithmetic, the rounding term, the
    /// sign-magnitude handling and the index table, hand-computed from
    /// SWF19's tables. Codes 1, 2, 4 walk the magnitude bits; code 9 is
    /// magnitude 1 with the sign bit set.
    #[test]
    fn decodes_a_hand_computed_4_bit_vector() {
        let mut w = BitWriter::new();
        w.write(2, 2); // 4 bits per code
        w.write_i16(100);
        w.write(0, 6);
        for code in [1u32, 2, 4, 9] {
            w.write(code, 4);
        }

        let pcm = samples(&decode(&w.data, false, 5).unwrap());
        // step 7 @ index 0: +1 -> 101, then +3 -> 104, then +7 -> 111
        // (index now 2, step 9), then magnitude 1 with sign set: -3 -> 108.
        assert_eq!(pcm, vec![100, 101, 104, 111, 108]);
    }

    /// Every code width must use its own index table and its own
    /// `step >> (bits - 1)` rounding term. Two successive max-magnitude
    /// codes expose both: the first delta pins the rounding term, and the
    /// second pins the index table, since each width's table moves the step
    /// index by a different amount (2, 4, 8, 16).
    #[test]
    fn each_code_width_uses_its_own_tables() {
        // (code_size, bits, largest positive code, expected samples)
        // Hand-computed from SWF19's tables starting at step index 0
        // (step 7); the index then lands on 2, 4, 8 and 16 respectively.
        let cases = [
            (0u32, 2u32, 0b1u32, [0i16, 10, 23]),
            (1, 3, 0b11, [0, 11, 29]),
            (2, 4, 0b111, [0, 11, 41]),
            (3, 5, 0b1111, [0, 11, 76]),
        ];

        for (code_size, bits, code, expected) in cases {
            let mut w = BitWriter::new();
            w.write(code_size, 2);
            w.write_i16(0);
            w.write(0, 6);
            w.write(code, bits);
            w.write(code, bits);

            let pcm = samples(&decode(&w.data, false, 3).unwrap());
            assert_eq!(pcm, expected.to_vec(), "code width {bits} decoded wrong");
        }
    }

    /// Stereo headers interleave per field (L sample, L index, R sample,
    /// R index) and codes alternate L, R. Getting either wrong swaps or
    /// smears the channels.
    #[test]
    fn stereo_interleaves_headers_and_codes() {
        let mut w = BitWriter::new();
        w.write(2, 2);
        w.write_i16(1000); // left seed
        w.write(0, 6);
        w.write_i16(-1000); // right seed
        w.write(0, 6);
        w.write(0, 4); // left code
        w.write(0, 4); // right code

        let pcm = samples(&decode(&w.data, true, 2).unwrap());
        assert_eq!(pcm, vec![1000, -1000, 1000, -1000]);
    }

    /// `SoundSampleCount` is authoritative: trim a long decode down to it.
    #[test]
    fn trims_output_to_the_declared_sample_count() {
        let mut w = BitWriter::new();
        w.write(2, 2);
        w.write_i16(5);
        w.write(0, 6);
        for _ in 0..100 {
            w.write(0, 4);
        }

        let pcm = samples(&decode(&w.data, false, 10).unwrap());
        assert_eq!(pcm.len(), 10);
    }

    /// A sample count larger than the payload must not pad (inventing
    /// silence) or drive a huge allocation.
    #[test]
    fn never_pads_beyond_the_decoded_payload() {
        let mut w = BitWriter::new();
        w.write(2, 2);
        w.write_i16(5);
        w.write(0, 6);
        w.write(0, 4);

        // A bogus count must decode to exactly what an unknown count does:
        // whatever the payload holds, and not one frame more. (The payload
        // holds one extra frame's worth of trailing byte padding, which is
        // exactly why `DefineSound` decoding trims to SoundSampleCount.)
        let unbounded = samples(&decode(&w.data, false, 0).unwrap());
        let bogus = samples(&decode(&w.data, false, u32::MAX).unwrap());
        assert_eq!(bogus, unbounded, "a bogus sample count must not add frames");
        assert!(bogus.len() < 8, "decoded {} frames from a 2-frame payload", bogus.len());
    }

    /// Truncated and garbage payloads are the untrusted-input case: short
    /// output is fine, a panic is not. Run in debug so overflow checks fire.
    #[test]
    fn truncated_and_garbage_payloads_do_not_panic() {
        assert!(decode(&[], false, 0).is_err(), "empty payload has no code size");

        // Every prefix of a valid stream.
        let mut w = BitWriter::new();
        w.write(3, 2);
        w.write_i16(-30000);
        w.write(63, 6); // max initial index, needs clamping to 88's table range
        for i in 0..200u32 {
            w.write(i % 32, 5);
        }
        for cut in 0..w.data.len() {
            let _ = decode(&w.data[..cut], false, 0);
            let _ = decode(&w.data[..cut], true, 0);
        }

        // Pseudo-random bytes across all four code sizes and both channel
        // counts -- exercises max-magnitude codes against max step indices.
        let mut seed = 0x12345678u32;
        let garbage: Vec<u8> = (0..4096)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8
            })
            .collect();
        for code_size in 0..4u8 {
            let mut bytes = garbage.clone();
            bytes[0] = (bytes[0] & 0x3F) | (code_size << 6);
            for stereo in [false, true] {
                let _ = decode(&bytes, stereo, 0);
            }
        }
    }

    /// The tables themselves, guarding against transcription slips -- the
    /// 5-bit table's leading 1 is the one people get wrong.
    #[test]
    fn index_tables_have_the_documented_shape() {
        for (i, table) in INDEX_TABLES.iter().enumerate() {
            assert_eq!(table.len(), 1 << (i + 1), "index table {i} is the wrong length");
        }
        assert_eq!(INDEX_TABLE_5[8], 1, "SWF19's 5-bit table starts its upper half at 1, not 2");
        assert_eq!(STEP_TABLE.len(), 89);
        assert_eq!(STEP_TABLE[88], 32767);
    }
}

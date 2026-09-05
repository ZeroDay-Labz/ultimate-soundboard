use pitch_shift::{RawState, Shifter, TOTAL_F32};

type State = Box<RawState>;

const HOP: usize = 128;

fn new_state() -> State {
    let v = vec![0.0f32; TOTAL_F32];
    v.try_into().expect("TOTAL_F32-sized state")
}

/// Duration-preserving pitch shift of one channel, `semitones` up/down.
/// Processes the whole buffer through the phase-vocoder shifter in fixed
/// 128-sample hops (its required interface); the tail is zero-padded to a
/// full hop and truncated back afterwards.
fn shift_channel(input: &[f32], semitones: f32, sample_rate: f32) -> Vec<f32> {
    let mut shifter = Shifter::new(new_state());
    let mut out = Vec::with_capacity(input.len());
    let mut chunk = [0.0f32; HOP];

    let mut pos = 0usize;
    while pos < input.len() {
        let end = (pos + HOP).min(input.len());
        let n = end - pos;
        chunk[..n].copy_from_slice(&input[pos..end]);
        if n < HOP {
            chunk[n..].fill(0.0);
        }
        let shifted = shifter.shift(&chunk, semitones, HOP, sample_rate);
        out.extend_from_slice(&shifted[..n]);
        pos = end;
    }

    out
}

/// Stereo duration-preserving pitch shift. `semitones == 0.0` is a no-op
/// (returns clones) so callers don't pay the FFT cost for untouched audio.
pub fn shift_stereo(
    left: &[f32],
    right: &[f32],
    semitones: f32,
    sample_rate: f32,
) -> (Vec<f32>, Vec<f32>) {
    if semitones.abs() < 0.01 {
        return (left.to_vec(), right.to_vec());
    }
    (
        shift_channel(left, semitones, sample_rate),
        shift_channel(right, semitones, sample_rate),
    )
}

/// Converts a linear pitch multiplier (0.25..4.0, 1.0 == unchanged) used by
/// `ButtonModel`/`TabModel` into semitones for the shifter.
pub fn ratio_to_semitones(ratio: f32) -> f32 {
    12.0 * ratio.max(0.001).log2()
}


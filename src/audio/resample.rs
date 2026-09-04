use anyhow::Result;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

/// Resamples planar stereo `(left, right)` from `from_rate` to `to_rate`
/// using a windowed-sinc resampler. Feeds the resampler in fixed-size
/// chunks (its required interface) and flushes any tail frames at the end.
pub fn resample_stereo(
    left: &[f32],
    right: &[f32],
    from_rate: u32,
    to_rate: u32,
) -> Result<(Vec<f32>, Vec<f32>)> {
    let ratio = to_rate as f64 / from_rate as f64;

    // Building the sinc filter table is the dominant cost of constructing
    // a resampler (roughly proportional to sinc_len * oversampling_factor),
    // and we build a fresh one per decode (see `engine.rs`'s decode cache
    // -- this only runs once per distinct file, not per play, but still
    // matters for how fast a prewarm queue or a first click clears).
    // 128/128 is a solid quality/speed tradeoff for a soundboard's short
    // clips -- well beyond audible transparency, at roughly a quarter of
    // the 256/256 setup cost.
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        oversampling_factor: 128,
        interpolation: SincInterpolationType::Linear,
        window: WindowFunction::BlackmanHarris2,
    };

    let chunk_size = 1024;
    let mut resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 2)?;

    let in_len = left.len();
    let mut out_left = Vec::with_capacity((in_len as f64 * ratio) as usize + chunk_size);
    let mut out_right = Vec::with_capacity((in_len as f64 * ratio) as usize + chunk_size);

    let mut pos = 0usize;
    while pos + chunk_size <= in_len {
        let input = [&left[pos..pos + chunk_size], &right[pos..pos + chunk_size]];
        let out = resampler.process(&input, None)?;
        out_left.extend_from_slice(&out[0]);
        out_right.extend_from_slice(&out[1]);
        pos += chunk_size;
    }

    // Flush the remaining tail (shorter than one chunk), if there is one.
    // When `in_len` is an exact multiple of `chunk_size`, `pos == in_len`
    // and this slice is empty -- passing an empty (as opposed to `None`)
    // partial input to rubato is a real, reproducible failure ("Insufficient
    // buffer size 0 for input channel 0, expected 1024"), which used to
    // make resampling silently bail out to un-resampled audio (wrong
    // pitch/speed) for any file whose frame count happened to land on
    // that boundary -- common enough with 44.1kHz sources to matter.
    let tail_left = &left[pos..];
    let tail_right = &right[pos..];
    if !tail_left.is_empty() {
        let tail_input = [tail_left, tail_right];
        let out = resampler.process_partial(Some(&tail_input), None)?;
        out_left.extend_from_slice(&out[0]);
        out_right.extend_from_slice(&out[1]);
    }

    // One more flush call with no input to drain internal delay.
    let flush = resampler.process_partial::<Vec<f32>>(None, None)?;
    out_left.extend_from_slice(&flush[0]);
    out_right.extend_from_slice(&flush[1]);

    Ok((out_left, out_right))
}

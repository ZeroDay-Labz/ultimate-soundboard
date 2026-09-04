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

    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        oversampling_factor: 256,
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

    // Flush the remaining tail (shorter than one chunk) plus any samples
    // still buffered inside the resampler.
    let tail_left = &left[pos..];
    let tail_right = &right[pos..];
    let tail_input = [tail_left, tail_right];
    let out = resampler.process_partial(Some(&tail_input), None)?;
    out_left.extend_from_slice(&out[0]);
    out_right.extend_from_slice(&out[1]);

    // One more flush call with no input to drain internal delay.
    let flush = resampler.process_partial::<Vec<f32>>(None, None)?;
    out_left.extend_from_slice(&flush[0]);
    out_right.extend_from_slice(&flush[1]);

    Ok((out_left, out_right))
}

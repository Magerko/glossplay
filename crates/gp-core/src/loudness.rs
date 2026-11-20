//! Loudness of mono speech after ITU-R BS.1770 (K-weighting, 400 ms blocks, gating), and
//! bringing a clip to a set loudness without letting its peaks clip.

/// A biquad filter in direct form I.
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    fn run(&self, x: &[f64]) -> Vec<f64> {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        x.iter()
            .map(|&x0| {
                let y0 = self.b[0] * x0 + self.b[1] * x1 + self.b[2] * x2
                    - self.a[0] * y1
                    - self.a[1] * y2;
                (x2, x1, y2, y1) = (x1, x0, y1, y0);
                y0
            })
            .collect()
    }
}

/// The two K-weighting stages for `rate`. BS.1770 gives coefficients for 48 kHz only; these are
/// the analogue prototypes libebur128 fitted to them, so 24 kHz clips get the same curve.
fn k_weighting(rate: u32) -> [Biquad; 2] {
    let rate = f64::from(rate);
    let (fc, gain_db, q) = (
        1_681.974_450_955_533,
        3.999_843_853_973_347,
        0.707_175_236_955_419_6,
    );
    let k = (std::f64::consts::PI * fc / rate).tan();
    let vh = 10f64.powf(gain_db / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    let (fc, q) = (38.135_470_876_024_44, 0.500_327_037_323_877_3);
    let k = (std::f64::consts::PI * fc / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    [shelf, high_pass]
}

/// The integrated loudness of mono `pcm` in LUFS; `None` when it is all silence.
pub fn integrated(pcm: &[i16], rate: u32) -> Option<f64> {
    let x: Vec<f64> = pcm.iter().map(|&v| f64::from(v) / 32768.0).collect();
    let [shelf, high_pass] = k_weighting(rate);
    let y = high_pass.run(&shelf.run(&x));
    let (block, step) = (rate as usize * 2 / 5, rate as usize / 10);
    let squares: Vec<f64> = if y.len() <= block {
        vec![y.iter().map(|v| v * v).sum::<f64>() / y.len().max(1) as f64]
    } else {
        (0..=(y.len() - block) / step)
            .map(|i| {
                y[i * step..i * step + block]
                    .iter()
                    .map(|v| v * v)
                    .sum::<f64>()
                    / block as f64
            })
            .collect()
    };
    let lufs = |ms: f64| -0.691 + 10.0 * ms.log10();
    let mean = |b: &[f64]| b.iter().sum::<f64>() / b.len() as f64;
    let heard: Vec<f64> = squares
        .into_iter()
        .filter(|&s| s > 0.0 && lufs(s) > -70.0)
        .collect();
    if heard.is_empty() {
        return None;
    }
    let gate = lufs(mean(&heard)) - 10.0;
    let kept: Vec<f64> = heard.into_iter().filter(|&s| lufs(s) > gate).collect();
    Some(lufs(mean(&kept)))
}

/// The most a clip is made louder: past this it is noise or a failed clip, not quiet speech.
const MAX_GAIN_DB: f64 = 24.0;

/// Brings mono `pcm` to `target` LUFS with its samples kept under `ceiling` dBFS. Where the gain
/// would push a peak over, it is lowered for the few milliseconds around it rather than for the
/// whole clip, so a voice with sharp consonants gets as loud as a smooth one.
pub fn normalize(pcm: &mut [i16], rate: u32, target: f64, ceiling: f64) {
    let Some(now) = integrated(pcm, rate) else {
        return;
    };
    let gain = 10f64.powf((target - now).min(MAX_GAIN_DB) / 20.0);
    let limit = 32768.0 * 10f64.powf(ceiling / 20.0);
    let x: Vec<f64> = pcm.iter().map(|&v| f64::from(v) * gain).collect();
    // A gain of at most what every sample within `w` of it needs, then averaged over the same
    // span: each average only takes in gains that already suit the sample it lands on.
    let w = (rate as usize / 200).max(1);
    let mut need = vec![1.0f64; x.len()];
    for (i, v) in x.iter().enumerate().filter(|(_, v)| v.abs() > limit) {
        let g = limit / v.abs();
        for n in &mut need[i.saturating_sub(w)..(i + w + 1).min(x.len())] {
            *n = n.min(g);
        }
    }
    let mut sum = vec![0.0f64; need.len() + 1];
    for (i, n) in need.iter().enumerate() {
        sum[i + 1] = sum[i] + n;
    }
    for (i, v) in pcm.iter_mut().enumerate() {
        let (from, to) = (i.saturating_sub(w), (i + w + 1).min(x.len()));
        let g = (sum[to] - sum[from]) / (to - from) as f64;
        *v = (x[i] * g).round().clamp(-limit, limit) as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f64, hz: f64, rate: u32, secs: f64) -> Vec<i16> {
        (0..(f64::from(rate) * secs) as usize)
            .map(|i| {
                (amp * 32767.0 * (std::f64::consts::TAU * hz * i as f64 / f64::from(rate)).sin())
                    as i16
            })
            .collect()
    }

    #[test]
    fn a_tone_measures_as_the_standard_says() {
        // BS.1770: a 997 Hz sine at full scale in one channel reads -3.01 LUFS.
        for rate in [24_000, 48_000] {
            let l = integrated(&sine(0.1, 997.0, rate, 2.0), rate).unwrap();
            assert!((l + 23.01).abs() < 0.05, "{rate}: {l}");
        }
        assert_eq!(integrated(&[0; 24_000], 24_000), None);
    }

    #[test]
    fn quiet_parts_do_not_drag_the_loudness_down() {
        let mut speech = sine(0.1, 997.0, 24_000, 2.0);
        speech.extend(sine(0.001, 997.0, 24_000, 4.0));
        // Only the blocks across the edge, half loud, are let through the gate.
        let l = integrated(&speech, 24_000).unwrap();
        assert!((l + 23.2).abs() < 0.5, "{l}");
        assert!(l > -24.0);
    }

    #[test]
    fn a_clip_reaches_the_target_and_its_peaks_stay_under_the_ceiling() {
        let mut quiet = sine(0.05, 997.0, 24_000, 2.0);
        normalize(&mut quiet, 24_000, -19.0, -1.0);
        assert!((integrated(&quiet, 24_000).unwrap() + 19.0).abs() < 0.1);

        // Short clicks in a quiet voice: the voice comes up to the target, the clicks do not clip.
        let mut peaky = sine(0.03, 997.0, 24_000, 2.0);
        for at in (1_000..48_000).step_by(6_000) {
            peaky[at] = 12_000;
            peaky[at + 1] = -12_000;
        }
        normalize(&mut peaky, 24_000, -19.0, -1.0);
        let limit = (32768.0 * 10f64.powf(-1.0 / 20.0)) as i32;
        assert!(peaky.iter().all(|v| i32::from(*v).abs() <= limit));
        let l = integrated(&peaky, 24_000).unwrap();
        assert!((l + 19.0).abs() < 1.0, "{l}");
    }
}

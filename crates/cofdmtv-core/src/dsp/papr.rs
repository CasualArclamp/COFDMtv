//! Peak-to-average power reduction by clipping in the (oversampled) time domain and
//! keeping only the used carriers (aicodix `papr.hh`, `ImprovePAPR`).

use super::{Cplx, Fft};

/// Clip the OFDM symbol whose carriers are `freq` (length `n`) and project back onto the
/// carriers in use, with `fact`-times oversampling (`fft` must have size `fact * n`).
/// Samples are clipped where the larger of |re| and |im| exceeds the RMS level.
pub fn improve_papr(freq: &mut [Cplx], fact: usize, fft: &mut Fft) {
    let n = freq.len();
    let size = fact * n;
    debug_assert_eq!(fft.len(), size);
    let used: Vec<bool> = freq.iter().map(|c| c.re != 0.0 || c.im != 0.0).collect();
    let mut over = vec![Cplx::new(0.0, 0.0); size];
    if fact == 1 {
        over.copy_from_slice(freq);
    } else {
        over[..n / 2].copy_from_slice(&freq[..n / 2]);
        over[size - n / 2..].copy_from_slice(&freq[n / 2..]);
    }
    fft.backward(&mut over);
    let factor = 1.0 / (size as f32).sqrt();
    for t in &mut over {
        *t *= factor;
        let amp = t.re.abs().max(t.im.abs());
        if amp > 1.0 {
            *t /= amp;
        }
    }
    fft.forward(&mut over);
    if fact == 1 {
        for (i, f) in freq.iter_mut().enumerate() {
            *f = if used[i] { factor * over[i] } else { Cplx::new(0.0, 0.0) };
        }
    } else {
        for i in 0..n / 2 {
            if used[i] {
                freq[i] = factor * over[i];
            }
        }
        for i in n / 2..n {
            if used[i] {
                freq[i] = factor * over[size - n + i];
            }
        }
    }
}

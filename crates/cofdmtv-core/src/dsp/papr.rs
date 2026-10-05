//! Peak-to-average power reduction by clipping in the (oversampled) time domain and
//! keeping only the used carriers (aicodix `papr.hh`, `ImprovePAPR`).

use super::{Cplx, Fft};

/// What is held to the clipping level: the apps differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clip {
    /// The larger of |re| and |im| (Shredpix).
    Components,
    /// The magnitude (Rattlegram).
    Magnitude,
}

/// Clip the OFDM symbol whose carriers are `freq` (length `n`) and project back onto the
/// carriers in use, with `fact`-times oversampling (`fft` must have size `fact * n`).
/// Samples are clipped where they exceed 1 after scaling by 1/√(`fact`·`n`): for the
/// symbols the encoders make, at about their RMS level (at 32 kHz and up, where `fact`
/// is 1; lower rates are oversampled to about 32 kHz first, which softens the clipping).
pub fn improve_papr(freq: &mut [Cplx], fact: usize, fft: &mut Fft, clip: Clip) {
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
        let amp = match clip {
            Clip::Components => t.re.abs().max(t.im.abs()),
            Clip::Magnitude => t.norm(),
        };
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

//! The receivers' front end: a DC blocker and the Hilbert filter that turns a real audio
//! signal into its analytic signal (aicodix/dsp `blockdc.hh`, `hilbert.hh`, `window.hh`).

use super::Cplx;

/// First-order DC blocker (`BlockDC`).
#[derive(Debug, Clone)]
pub struct BlockDc {
    x1: f32,
    y1: f32,
    a: f32,
    b: f32,
}

impl BlockDc {
    /// A blocker with a time constant of about `samples` samples.
    pub fn new(samples: usize) -> Self {
        let a = (samples as f32 - 1.0) / samples as f32;
        Self { x1: 0.0, y1: 0.0, a, b: (1.0 + a) / 2.0 }
    }

    #[inline]
    pub fn process(&mut self, x0: f32) -> f32 {
        let y0 = self.b * (x0 - self.x1) + self.a * self.y1;
        self.x1 = x0;
        self.y1 = y0;
        y0
    }
}

/// Zeroth-order modified Bessel function of the first kind (Kaiser window), summed until
/// the terms stop changing the result, like the original.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut val = 1.0;
    for n in 1..35 {
        val *= x / f64::from(2 * n);
        let next = sum + val * val;
        if next == sum {
            break;
        }
        sum = next;
    }
    sum
}

/// Kaiser window value `n` of `taps` with parameter `a` (`Kaiser`).
fn kaiser(a: f64, n: usize, taps: usize) -> f64 {
    let t = 2.0 * n as f64 / (taps - 1) as f64 - 1.0;
    bessel_i0(std::f64::consts::PI * a * (1.0 - t * t).sqrt()) / bessel_i0(std::f64::consts::PI * a)
}

/// Discrete Hilbert transformer of `taps` taps (`taps − 1` divisible by 4), Kaiser
/// windowed with a = 2: real input in, analytic signal (delayed by `(taps − 1) / 2`) out.
#[derive(Debug, Clone)]
pub struct Hilbert {
    taps: usize,
    /// The last `taps` inputs, twice over so that a window is always contiguous.
    hist: Vec<f32>,
    pos: usize,
    reco: f32,
    imco: Vec<f32>,
}

impl Hilbert {
    pub fn new(taps: usize) -> Self {
        assert!(taps >= 5 && (taps - 1) % 4 == 0, "taps − 1 must be divisible by 4");
        let a = 2.0;
        let mid = (taps - 1) / 2;
        let reco = kaiser(a, mid, taps) as f32;
        let imco = (0..(taps - 1) / 4)
            .map(|i| (kaiser(a, (2 * i + 1) + mid, taps) * 2.0 / ((2 * i + 1) as f64 * std::f64::consts::PI)) as f32)
            .collect();
        Self { taps, hist: vec![0.0; 2 * taps], pos: 0, reco, imco }
    }

    #[inline]
    pub fn process(&mut self, input: f32) -> Cplx {
        // `real[i]` of the original (oldest first) is `self.hist[self.pos + i]`.
        let real = &self.hist[self.pos..self.pos + self.taps];
        let mid = (self.taps - 1) / 2;
        let re = self.reco * real[mid];
        let mut im = self.imco[0] * (real[mid - 1] - real[mid + 1]);
        for (i, c) in self.imco.iter().enumerate().skip(1) {
            im += c * (real[mid - (2 * i + 1)] - real[mid + (2 * i + 1)]);
        }
        // Shift in the new sample: it becomes the newest of the window starting one later.
        self.hist[self.pos] = input;
        self.hist[self.pos + self.taps] = input;
        self.pos += 1;
        if self.pos == self.taps {
            self.pos = 0;
        }
        Cplx::new(re, im)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hilbert_makes_an_analytic_tone() {
        // A tone at a quarter of the sample rate comes out as e^{jωn}: equal magnitude in
        // both parts, the imaginary one lagging by 90°.
        let mut h = Hilbert::new(33);
        let mut out = Vec::new();
        for n in 0..200 {
            out.push(h.process((std::f32::consts::FRAC_PI_2 * n as f32).cos()));
        }
        let z = out[150];
        let w = out[151];
        assert!((z.norm() - 1.0).abs() < 0.05, "{z}");
        let rot = super::super::div(w, z);
        assert!((rot - Cplx::new(0.0, 1.0)).norm() < 0.05, "{rot}");
    }

    #[test]
    fn dc_is_removed() {
        let mut dc = BlockDc::new(33);
        let mut y = 0.0;
        for _ in 0..2000 {
            y = dc.process(0.5);
        }
        assert!(y.abs() < 1e-4);
    }
}

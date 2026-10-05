//! Theil–Sen line fit (aicodix/dsp `theil_sen.hh`): the median of the pairwise slopes and
//! of the resulting intercepts — robust against the odd wrong phase from a decision error.

/// Fits `y ≈ yint + slope · x`.
#[derive(Debug, Clone, Default)]
pub struct TheilSen {
    temp: Vec<f32>,
    slope: f32,
    yint: f32,
}

/// The `k`-th smallest of `a` (what `quick_select` returns).
fn select(a: &mut [f32], k: usize) -> f32 {
    *a.select_nth_unstable_by(k, f32::total_cmp).1
}

impl TheilSen {
    pub fn compute(&mut self, x: &[f32], y: &[f32]) {
        let len = x.len().min(y.len());
        self.temp.clear();
        for i in 0..len {
            for j in i + 1..len {
                if x[j] != x[i] {
                    self.temp.push((y[j] - y[i]) / (x[j] - x[i]));
                }
            }
        }
        self.slope = if self.temp.is_empty() {
            0.0
        } else {
            let k = self.temp.len() / 2;
            select(&mut self.temp, k)
        };
        self.temp.clear();
        self.temp.extend((0..len).map(|i| y[i] - self.slope * x[i]));
        self.yint = if self.temp.is_empty() {
            0.0
        } else {
            let k = self.temp.len() / 2;
            select(&mut self.temp, k)
        };
    }

    pub fn slope(&self) -> f32 {
        self.slope
    }

    pub fn yint(&self) -> f32 {
        self.yint
    }

    #[inline]
    pub fn eval(&self, x: f32) -> f32 {
        self.yint + self.slope * x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_a_line_despite_outliers() {
        let x: Vec<f32> = (0..50).map(|i| i as f32).collect();
        let mut y: Vec<f32> = x.iter().map(|x| 0.5 + 0.02 * x).collect();
        y[7] = 3.0;
        y[30] = -2.0;
        let mut t = TheilSen::default();
        t.compute(&x, &y);
        assert!((t.slope() - 0.02).abs() < 1e-5 && (t.yint() - 0.5).abs() < 1e-5);
    }
}

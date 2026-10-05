//! Maximum length sequences from a Fibonacci-style shift register (aicodix/code `mls.hh`).

/// A maximum length sequence generator for the polynomial `poly` (bit `n` = coefficient of
/// `x^n`, the highest set bit is the degree).
#[derive(Debug, Clone)]
pub struct Mls {
    poly: u32,
    test: u32,
    reg: u32,
}

fn hibit(mut n: u32) -> u32 {
    n |= n >> 1;
    n |= n >> 2;
    n |= n >> 4;
    n |= n >> 8;
    n |= n >> 16;
    n ^ (n >> 1)
}

impl Mls {
    pub fn new(poly: u32) -> Self {
        Self::with_seed(poly, 1)
    }

    pub fn with_seed(poly: u32, reg: u32) -> Self {
        Self { poly, test: hibit(poly) >> 1, reg }
    }

    pub fn reset(&mut self, reg: u32) {
        self.reg = reg;
    }

    /// The next bit.
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> bool {
        let fb = self.reg & self.test != 0;
        self.reg <<= 1;
        if fb {
            self.reg ^= self.poly;
        }
        fb
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_is_maximal() {
        for (poly, degree) in [(0b1000_1001, 7), (0b1_0010_1011, 8), (0b1001_0101_0001, 11)] {
            let mut seq = Mls::new(poly);
            let len = (1u32 << degree) - 1;
            let first: Vec<bool> = (0..len).map(|_| seq.next()).collect();
            let second: Vec<bool> = (0..len).map(|_| seq.next()).collect();
            assert_eq!(first, second, "period of {poly:#b}");
            let ones = first.iter().filter(|b| **b).count() as u32;
            assert_eq!(ones, len.div_ceil(2), "balance of {poly:#b}");
        }
    }
}

//! Marsaglia's xorshift generator (aicodix/code `xorshift.hh`): scrambles the payload
//! bytes so that long runs of equal bits do not reach the modulator.

/// 32-bit xorshift with the aicodix default seed.
#[derive(Debug, Clone)]
pub struct Xorshift32 {
    y: u32,
}

impl Default for Xorshift32 {
    fn default() -> Self {
        Self { y: 2_463_534_242 }
    }
}

impl Xorshift32 {
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u32 {
        self.y ^= self.y << 13;
        self.y ^= self.y >> 17;
        self.y ^= self.y << 5;
        self.y
    }

    /// XOR `data` with the low bytes of the sequence (scrambling and descrambling alike).
    pub fn scramble(data: &mut [u8]) {
        let mut seq = Self::default();
        for b in data {
            *b ^= seq.next() as u8;
        }
    }
}

/// A xorshift generator confined to `bits` bits (aicodix/code `XorShiftMask`): with the
/// right shifts it visits every value 1…2^bits − 1 once, which the modem uses to permute
/// the code bits.
#[derive(Debug, Clone)]
pub struct XorShiftMask {
    mask: u32,
    shifts: (u32, u32, u32),
    y: u32,
}

impl XorShiftMask {
    pub fn new(bits: u32, first: u32, second: u32, third: u32) -> Self {
        Self { mask: (1 << bits) - 1, shifts: (first, second, third), y: 1 }
    }

    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u32 {
        let (a, b, c) = self.shifts;
        self.y ^= self.y << a;
        self.y &= self.mask;
        self.y ^= self.y >> b;
        self.y ^= self.y << c;
        self.y &= self.mask;
        self.y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_visit_every_value() {
        // The modem's permutations, by code order.
        for (bits, a, b, c) in [(8, 1, 1, 2), (11, 1, 3, 4), (12, 1, 1, 4), (13, 1, 1, 9), (14, 1, 5, 10), (15, 1, 1, 3), (16, 1, 1, 14)] {
            let mut seq = XorShiftMask::new(bits, a, b, c);
            let n = (1usize << bits) - 1;
            let mut seen = vec![false; n + 1];
            for _ in 0..n {
                let v = seq.next() as usize;
                assert!(v >= 1 && !seen[v], "order {bits}: {v} twice");
                seen[v] = true;
            }
        }
    }
}

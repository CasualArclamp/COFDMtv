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

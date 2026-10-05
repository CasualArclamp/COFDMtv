//! Polar codes: non-systematic and systematic encoders (aicodix/code `polar_encoder.hh`),
//! successive-cancellation list decoding (`polar_list_decoder.hh`), and the CRC-aided list
//! decoding of shortened systematic codes that COFDMTV and the modem use (`polar.hh` of
//! Assempix and Rattlegram).
//!
//! Bits are 0/1 bytes in the encoders. The list decoder works on log-likelihood ratios,
//! positive for bit 0, with `L` paths side by side in `[f32; L]` lanes (the C++ uses SIMD
//! registers; the compiler vectorises the lane loops here). Its rate-0 shortcuts are taken
//! at exactly the tree nodes where the original takes them, so both decode alike.

use super::crc::Crc32;

/// Is bit `i` frozen?
#[inline]
pub fn is_frozen(frozen: &[u32], i: usize) -> bool {
    (frozen[i / 32] >> (i % 32)) & 1 != 0
}

/// Number of information (non-frozen) positions of a code of length `1 << level`.
pub fn info_count(frozen: &[u32], level: u32) -> usize {
    frozen[..(1usize << level).div_ceil(32)].iter().map(|w| 32 - w.count_ones() as usize).sum()
}

fn butterflies(codeword: &mut [u8]) {
    let length = codeword.len();
    let mut h = 2;
    while h < length {
        for i in (0..length).step_by(2 * h) {
            for j in i..i + h {
                codeword[j] ^= codeword[j + h];
            }
        }
        h *= 2;
    }
}

/// Non-systematic encoding: `message` fills the information positions in order, frozen
/// positions are 0; `codeword.len()` is the code length.
pub fn encode(codeword: &mut [u8], message: &[u8], frozen: &[u32]) {
    let mut msg = message.iter();
    for i in (0..codeword.len()).step_by(2) {
        let m0 = if is_frozen(frozen, i) { 0 } else { *msg.next().expect("message too short") };
        let m1 = if is_frozen(frozen, i + 1) { 0 } else { *msg.next().expect("message too short") };
        codeword[i] = m0 ^ m1;
        codeword[i + 1] = m1;
    }
    butterflies(codeword);
}

/// Systematic encoding: the information positions of `codeword` carry `message` itself.
pub fn encode_systematic(codeword: &mut [u8], message: &[u8], frozen: &[u32]) {
    encode(codeword, message, frozen);
    for i in (0..codeword.len()).step_by(2) {
        let m0 = if is_frozen(frozen, i) { 0 } else { codeword[i] };
        let m1 = if is_frozen(frozen, i + 1) { 0 } else { codeword[i + 1] };
        codeword[i] = m0 ^ m1;
        codeword[i + 1] = m1;
    }
    butterflies(codeword);
}

/// Path survivor map: lane `k` of a node's result continues lane `map[k]` of its input.
type Map<const L: usize> = [u8; L];

#[inline]
fn identity<const L: usize>() -> Map<L> {
    std::array::from_fn(|k| k as u8)
}

/// `vshuf`: permute the lanes of `a` by `map`.
#[inline]
fn shuf<T: Copy, const L: usize>(a: &[T; L], map: &Map<L>) -> [T; L] {
    std::array::from_fn(|k| a[map[k] as usize])
}

/// The f-function (min-sum): sign(a)·sign(b)·min(|a|, |b|).
#[inline]
fn prod(a: f32, b: f32) -> f32 {
    let m = a.abs().min(b.abs());
    if a.is_sign_negative() != b.is_sign_negative() { -m } else { m }
}

/// Successive-cancellation list decoder with `L` paths for codes up to `1 << max_level`.
pub struct PolarListDecoder<const L: usize> {
    /// The LLRs of the node being decoded at level `m` live in `soft[1 << m .. 2 << m]`.
    soft: Vec<[f32; L]>,
    hard: Vec<[f32; L]>,
    maps: Vec<Map<L>>,
    count: usize,
    metric: [f32; L],
}

impl<const L: usize> PolarListDecoder<L> {
    pub fn new(max_level: u32) -> Self {
        assert!(L >= 1 && L <= 128, "list size");
        let n = 1usize << max_level;
        Self { soft: vec![[0.0; L]; 2 * n], hard: vec![[0.0; L]; n], maps: vec![[0; L]; n], count: 0, metric: [0.0; L] }
    }

    /// Decode the LLRs `codeword` (length `1 << level`). `message` receives the ±1 values
    /// of the information bits, lane `k` holding list path `k` (path 0 ranked first);
    /// returns the number of information bits.
    pub fn decode(&mut self, message: &mut [[f32; L]], codeword: &[f32], frozen: &[u32], level: u32) -> usize {
        let n = 1usize << level;
        assert!(n <= self.hard.len() && codeword.len() >= n && (5..=16).contains(&level));
        self.count = 0;
        self.metric = [1_000_000.0; L];
        self.metric[0] = 0.0;
        for (s, &c) in self.soft[n..2 * n].iter_mut().zip(codeword) {
            *s = [c; L];
        }
        if level == 5 {
            self.small(5, 0, frozen[0], message);
        } else {
            self.tree(level, 0, frozen, message);
        }
        let count = self.count;
        if count > 0 {
            let mut acc = self.maps[count - 1];
            for i in (0..count - 1).rev() {
                message[i] = shuf(&message[i], &acc);
                acc = shuf(&self.maps[i], &acc);
            }
        }
        count
    }

    /// Paths' metrics at the end of the last decode (lower is better).
    pub fn metrics(&self) -> [f32; L] {
        self.metric
    }

    /// The f-function from level `m`'s node into its left child.
    #[inline]
    fn f(&mut self, n: usize) {
        for i in 0..n / 2 {
            let (a, b) = (self.soft[i + n], self.soft[i + n / 2 + n]);
            self.soft[i + n / 2] = std::array::from_fn(|k| prod(a[k], b[k]));
        }
    }

    /// The g-function into the right child, given the left child's hard bits and map.
    #[inline]
    fn g(&mut self, n: usize, hard_off: usize, lmap: &Map<L>) {
        for i in 0..n / 2 {
            let h = self.hard[hard_off + i];
            let a = shuf(&self.soft[i + n], lmap);
            let b = shuf(&self.soft[i + n / 2 + n], lmap);
            self.soft[i + n / 2] = std::array::from_fn(|k| h[k] * a[k] + b[k]);
        }
    }

    /// Combine the children's hard bits.
    #[inline]
    fn combine(&mut self, n: usize, hard_off: usize, rmap: &Map<L>) {
        for i in 0..n / 2 {
            let l = shuf(&self.hard[hard_off + i], rmap);
            let r = self.hard[hard_off + n / 2 + i];
            self.hard[hard_off + i] = std::array::from_fn(|k| l[k] * r[k]);
        }
    }

    /// A node of level `m` ≥ 6; `frozen` starts at the node's first word.
    fn tree(&mut self, m: u32, hard_off: usize, frozen: &[u32], message: &mut [[f32; L]]) -> Map<L> {
        let n = 1usize << m;
        self.f(n);
        let lmap = if m == 6 {
            if frozen[0] == u32::MAX { self.rate0(5, hard_off) } else { self.small(5, hard_off, frozen[0], message) }
        } else {
            self.tree(m - 1, hard_off, frozen, message)
        };
        self.g(n, hard_off, &lmap);
        let rmap = if m == 6 {
            if frozen[1] == u32::MAX { self.rate0(5, hard_off + n / 2) } else { self.small(5, hard_off + n / 2, frozen[1], message) }
        } else {
            self.tree(m - 1, hard_off + n / 2, &frozen[n / 64..], message)
        };
        self.combine(n, hard_off, &rmap);
        shuf(&lmap, &rmap)
    }

    /// A node of level 1..=5 whose frozen bits are the low `1 << m` bits of `frozen`.
    fn small(&mut self, m: u32, hard_off: usize, frozen: u32, message: &mut [[f32; L]]) -> Map<L> {
        let n = 1usize << m;
        self.f(n);
        let half = n / 2;
        let mask = (1u32 << half) - 1;
        let (left, right) = (frozen & mask, frozen >> half);
        let lmap = if m == 1 {
            if left != 0 { self.rate0(0, hard_off) } else { self.rate1(hard_off, message) }
        } else if left == mask {
            self.rate0(m - 1, hard_off)
        } else {
            self.small(m - 1, hard_off, left, message)
        };
        self.g(n, hard_off, &lmap);
        let rmap = if m == 1 {
            if right != 0 { self.rate0(0, hard_off + 1) } else { self.rate1(hard_off + 1, message) }
        } else if right == mask {
            self.rate0(m - 1, hard_off + half)
        } else {
            self.small(m - 1, hard_off + half, right, message)
        };
        self.combine(n, hard_off, &rmap);
        shuf(&lmap, &rmap)
    }

    /// A frozen subtree of level `m`: all bits 0, and every path pays for each input LLR
    /// that says otherwise.
    fn rate0(&mut self, m: u32, hard_off: usize) -> Map<L> {
        let n = 1usize << m;
        for h in &mut self.hard[hard_off..hard_off + n] {
            *h = [1.0; L];
        }
        for i in 0..n {
            let s = self.soft[i + n];
            for k in 0..L {
                if s[k] < 0.0 {
                    self.metric[k] -= s[k];
                }
            }
        }
        identity()
    }

    /// An information bit: every path forks into bit 0 and bit 1; the `L` best survive.
    fn rate1(&mut self, hard_off: usize, message: &mut [[f32; L]]) -> Map<L> {
        let sft = self.soft[1];
        // Rust note: arrays of a length computed from a const generic (2 * L) are not
        // allowed yet, so the forks use a fixed upper bound.
        let mut fork = [0.0f32; 256];
        let mut perm = [0u8; 256];
        for k in 0..L {
            let (mut zero, mut one) = (self.metric[k], self.metric[k]);
            if sft[k] < 0.0 {
                zero -= sft[k];
            } else {
                one += sft[k];
            }
            fork[2 * k] = zero;
            fork[2 * k + 1] = one;
        }
        // Stable insertion sort of the 2L forks, remembering where each came from.
        perm[0] = 0;
        for i in 1..2 * L {
            let t = fork[i];
            let mut j = i;
            while j > 0 && t < fork[j - 1] {
                fork[j] = fork[j - 1];
                perm[j] = perm[j - 1];
                j -= 1;
            }
            fork[j] = t;
            perm[j] = i as u8;
        }
        let map: Map<L> = std::array::from_fn(|k| perm[k] >> 1);
        let hrd: [f32; L] = std::array::from_fn(|k| if perm[k] & 1 != 0 { -1.0 } else { 1.0 });
        self.metric.copy_from_slice(&fork[..L]);
        message[self.count] = hrd;
        self.maps[self.count] = map;
        self.count += 1;
        self.hard[hard_off] = hrd;
        map
    }
}

/// A CRC-aided list decoder for the shortened systematic polar codes of COFDMTV and the
/// modem: the code of length `1 << level` carries `mesg_bits` information bits, of which
/// the first `data_bits + 32` (data and CRC) are sent and the rest are known zeros.
pub struct CaScl<const L: usize> {
    decoder: PolarListDecoder<L>,
    message: Vec<[f32; L]>,
    code: Vec<f32>,
    bits: Vec<u8>,
}

/// What [`CaScl::decode`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The data bits packed least significant bit first.
    pub data: Vec<u8>,
    /// Data bits whose hard decision the decoder corrected.
    pub flips: u32,
    /// The list path that passed the CRC (0 = the best ranked).
    pub path: usize,
}

/// The parameters of one shortened code.
#[derive(Debug, Clone, Copy)]
pub struct ShortCode {
    pub level: u32,
    pub frozen: &'static [u32],
    /// Information positions of the mother code.
    pub mesg_bits: usize,
    /// Data bits (a multiple of 8).
    pub data_bits: usize,
    pub crc_poly: u32,
}

impl ShortCode {
    /// Data plus CRC bits.
    pub fn crc_bits(&self) -> usize {
        self.data_bits + 32
    }

    /// Code bits actually sent.
    pub fn sent_bits(&self) -> usize {
        (1usize << self.level) - (self.mesg_bits - self.crc_bits())
    }

    /// Encode `data` (`data_bits / 8` bytes): the bits to send, 0/1 per byte.
    pub fn encode(&self, data: &[u8]) -> Vec<u8> {
        assert_eq!(data.len() * 8, self.data_bits);
        let n = 1usize << self.level;
        let mut mesg = vec![0u8; self.mesg_bits];
        for (i, m) in mesg.iter_mut().enumerate().take(self.data_bits) {
            *m = u8::from(super::bits::get_le_bit(data, i));
        }
        let mut crc = Crc32::new(self.crc_poly);
        let sum = crc.bytes(data);
        for i in 0..32 {
            mesg[self.data_bits + i] = ((sum >> i) & 1) as u8;
        }
        let mut code = vec![0u8; n];
        encode_systematic(&mut code, &mesg, self.frozen);
        // Shorten: drop the information positions past the CRC (known zeros).
        let mut out = Vec::with_capacity(self.sent_bits());
        let mut k = 0;
        for (i, &c) in code.iter().enumerate() {
            let keep = is_frozen(self.frozen, i) || {
                k += 1;
                k - 1 < self.crc_bits()
            };
            if keep {
                out.push(c);
            }
        }
        out
    }
}

impl<const L: usize> CaScl<L> {
    pub fn new(max_level: u32) -> Self {
        let n = 1usize << max_level;
        Self { decoder: PolarListDecoder::new(max_level), message: vec![[0.0; L]; n], code: vec![0.0; n], bits: vec![0; n] }
    }

    /// List-decode a plain (non-systematic) polar code of length `1 << level`, as the
    /// modem uses: the information bits are the message. Returns their number; then
    /// [`Self::message_bit`] reads path `k`'s bits (path 0 ranked first).
    pub fn decode_plain(&mut self, llr: &[f32], frozen: &[u32], level: u32) -> usize {
        self.decoder.decode(&mut self.message, llr, frozen, level)
    }

    /// Information bit `i` of list path `path` after [`Self::decode_plain`].
    pub fn message_bit(&self, i: usize, path: usize) -> bool {
        self.message[i][path] < 0.0
    }

    /// Decode the received LLRs `received` (`code.sent_bits()` of them). `None` when no
    /// list path passes the CRC.
    pub fn decode(&mut self, received: &[f32], code: &ShortCode) -> Option<Decoded> {
        let n = 1usize << code.level;
        let crc_bits = code.crc_bits();
        assert_eq!(received.len(), code.sent_bits());
        // Lengthen: put the received bits back in place, the dropped ones as sure zeros.
        let mut j = received.len();
        let mut k = code.mesg_bits;
        for i in (0..n).rev() {
            let take = is_frozen(code.frozen, i) || {
                k -= 1;
                k < crc_bits
            };
            self.code[i] = if take {
                j -= 1;
                received[j]
            } else {
                9000.0
            };
        }
        let count = self.decoder.decode(&mut self.message, &self.code[..n], code.frozen, code.level);
        debug_assert_eq!(count, code.mesg_bits);
        let mut crc = Crc32::new(code.crc_poly);
        let mut sys = vec![0u8; crc_bits];
        for path in 0..L {
            // Re-encode the path's information bits to get its systematic bits.
            let mesg: Vec<u8> = self.message[..count].iter().map(|v| u8::from(v[path] < 0.0)).collect();
            encode(&mut self.bits[..n], &mesg, code.frozen);
            let mut s = 0;
            for i in 0..n {
                if s == crc_bits {
                    break;
                }
                if !is_frozen(code.frozen, i) {
                    sys[s] = self.bits[i];
                    s += 1;
                }
            }
            crc.reset();
            for &b in &sys {
                crc.bit(b != 0);
            }
            if crc.value() != 0 {
                continue;
            }
            let mut data = vec![0u8; code.data_bits / 8];
            let mut flips = 0;
            let mut j = 0;
            for (i, &bit) in sys.iter().enumerate().take(code.data_bits) {
                while is_frozen(code.frozen, j) {
                    j += 1;
                }
                flips += u32::from((self.code[j] < 0.0) != (bit != 0));
                j += 1;
                super::bits::set_le_bit(&mut data, i, bit != 0);
            }
            return Some(Decoded { data, flips, path });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::tables::{IMAGE_FROZEN_64512_43072, TEXT_FROZEN_2048_712};
    use crate::coding::xorshift::Xorshift32;

    fn gauss(rng: &mut Xorshift32) -> f32 {
        // Box–Muller from two uniform variates.
        let u1 = (rng.next() as f32 + 1.0) / (u32::MAX as f32 + 2.0);
        let u2 = rng.next() as f32 / u32::MAX as f32;
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }

    #[test]
    fn systematic_encoding_keeps_message() {
        let frozen = &TEXT_FROZEN_2048_712;
        assert_eq!(info_count(frozen, 11), 712);
        let mut rng = Xorshift32::default();
        let mesg: Vec<u8> = (0..712).map(|_| (rng.next() & 1) as u8).collect();
        let mut code = vec![0u8; 2048];
        encode_systematic(&mut code, &mesg, frozen);
        let sys: Vec<u8> = (0..2048).filter(|&i| !is_frozen(frozen, i)).map(|i| code[i]).collect();
        assert_eq!(sys, mesg);
    }

    #[test]
    fn list_decoding_corrects_noise() {
        let code = ShortCode { level: 11, frozen: &TEXT_FROZEN_2048_712, mesg_bits: 712, data_bits: 680, crc_poly: crate::coding::crc::POLY_DATA };
        let mut rng = Xorshift32::default();
        let data: Vec<u8> = (0..85).map(|_| rng.next() as u8).collect();
        let bits = code.encode(&data);
        assert_eq!(bits.len(), 2048);
        // BPSK at Eb/N0 ≈ 2.5 dB (rate 1/3): plenty of hard errors, still decodable.
        let sigma = 0.9f32;
        let llr: Vec<f32> = bits
            .iter()
            .map(|&b| {
                let x = if b != 0 { -1.0 } else { 1.0 };
                2.0 * (x + sigma * gauss(&mut rng)) / (sigma * sigma)
            })
            .collect();
        let mut dec = CaScl::<16>::new(11);
        let out = dec.decode(&llr, &code).expect("decodes");
        assert_eq!(out.data, data);
        assert!(out.flips > 50, "the channel flipped bits: {}", out.flips);
    }

    #[test]
    fn shortened_code_round_trip() {
        // Mode 10..13 code: 64512 sent bits of the 65536 mother code.
        let code = ShortCode { level: 16, frozen: &IMAGE_FROZEN_64512_43072, mesg_bits: 44096, data_bits: 43040, crc_poly: crate::coding::crc::POLY_IMAGE };
        assert_eq!(info_count(code.frozen, 16), 44096);
        assert_eq!(code.sent_bits(), 64512);
        let mut rng = Xorshift32::default();
        let data: Vec<u8> = (0..5380).map(|_| rng.next() as u8).collect();
        let bits = code.encode(&data);
        assert_eq!(bits.len(), 64512);
        let sigma = 0.7f32;
        let llr: Vec<f32> = bits
            .iter()
            .map(|&b| {
                let x = if b != 0 { -1.0 } else { 1.0 };
                2.0 * (x + sigma * gauss(&mut rng)) / (sigma * sigma)
            })
            .collect();
        let mut dec = CaScl::<4>::new(16);
        let out = dec.decode(&llr, &code).expect("decodes");
        assert_eq!(out.data, data);
    }
}

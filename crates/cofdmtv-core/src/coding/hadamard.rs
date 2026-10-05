//! Augmented Hadamard code of 7 bits in 64 (aicodix/code `hadamard_encoder.hh`,
//! `hadamard_decoder.hh`): the modem's pilots carry the seed of each symbol's PAPR
//! scrambling this way.

/// Codeword length.
pub const N: usize = 64;

fn parity(mut x: u32) -> bool {
    x ^= x >> 16;
    x ^= x >> 8;
    x ^= x >> 4;
    x ^= x >> 2;
    x ^= x >> 1;
    x & 1 != 0
}

/// The ±1 codeword of `msg` (0…127).
pub fn encode(msg: u32) -> [i8; N] {
    std::array::from_fn(|i| if parity(msg & (i as u32 | N as u32)) { -1 } else { 1 })
}

/// The message of soft values `code` (fast Hadamard transform); `None` when two
/// candidates are equally likely.
pub fn decode(code: &[i8; N]) -> Option<u32> {
    let mut sum = [0i32; N];
    for i in (0..N).step_by(2) {
        sum[i] = i32::from(code[i]) + i32::from(code[i + 1]);
        sum[i + 1] = i32::from(code[i]) - i32::from(code[i + 1]);
    }
    let mut h = 2;
    while h < N {
        for i in (0..N).step_by(2 * h) {
            for j in i..i + h {
                let (x, y) = (sum[j] + sum[j + h], sum[j] - sum[j + h]);
                sum[j] = x;
                sum[j + h] = y;
            }
        }
        h *= 2;
    }
    let (mut word, mut best, mut next) = (0u32, 0i32, 0i32);
    for (i, &s) in sum.iter().enumerate() {
        let mag = s.abs();
        let msg = i as u32 + if s < 0 { N as u32 } else { 0 };
        if mag > best {
            next = best;
            best = mag;
            word = msg;
        } else if mag > next {
            next = mag;
        }
    }
    (best != next).then_some(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_round_trips_through_noise() {
        for msg in 0..128 {
            let mut c = encode(msg);
            // Flip a quarter of the signs: still decodable (minimum distance 32).
            for i in (0..N).step_by(5) {
                c[i] = -c[i];
            }
            let soft: [i8; N] = std::array::from_fn(|i| c[i] * 100);
            assert_eq!(decode(&soft), Some(msg));
        }
        assert_eq!(decode(&[0; N]), None);
    }
}

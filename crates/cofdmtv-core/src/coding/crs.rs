//! Cauchy Reed–Solomon erasure coding over GF(2^16) (aicodix/code
//! `cauchy_reed_solomon_erasure_coding.hh`, `galois_field.hh`).
//!
//! A message of `count` data blocks is spread over any number of coded blocks, each
//! identified by an `ident` ≥ `count`; any `count` distinct coded blocks give back the
//! data. Coded block `x` is Σ_j d_j / (x + j) (Cauchy matrix; `+` is XOR in the field);
//! decoding multiplies with the inverse of the Cauchy submatrix of the received idents,
//! which has a closed form. Symbols are 16-bit little-endian words of the blocks' bytes.
//!
//! The data blocks themselves can go along as blocks `0…count−1` (a systematic code):
//! every square submatrix of a Cauchy matrix is regular, so any `count` blocks, data or
//! coded, still give back the data. Take the shares of the data blocks in hand out of the
//! coded ones ([`accumulate`]) and solve for the rest ([`decode_columns`]).

use std::sync::OnceLock;

/// x^16 + x^12 + x^3 + x + 1.
const POLY: u32 = 0b1_0001_0000_0000_1011;
const Q: usize = 1 << 16;
/// The multiplicative group's order, also the "log of zero" sentinel.
const N: u32 = (Q - 1) as u32;

struct Tables {
    log: Vec<u16>,
    exp: Vec<u16>,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut log = vec![0u16; Q];
        let mut exp = vec![0u16; Q];
        exp[N as usize] = 0;
        log[0] = N as u16;
        let mut a: u32 = 1;
        for i in 0..N {
            exp[i as usize] = a as u16;
            log[a as usize] = i as u16;
            a <<= 1;
            if a & (Q as u32) != 0 {
                a ^= POLY;
            }
        }
        debug_assert_eq!(a, 1);
        Tables { log, exp }
    })
}

/// An element in log form (`i` stands for α^i).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Index(u32);

impl Index {
    const ONE: Index = Index(0);

    fn of(v: u16) -> Index {
        debug_assert!(v != 0);
        Index(u32::from(tables().log[v as usize]))
    }

    fn mul(self, b: Index) -> Index {
        let s = self.0 + b.0;
        Index(if s >= N { s - N } else { s })
    }

    fn div(self, b: Index) -> Index {
        Index(if self.0 < b.0 { self.0 + N - b.0 } else { self.0 - b.0 })
    }

    fn rcp(self) -> Index {
        Index(if self.0 == 0 { 0 } else { N - self.0 })
    }

    /// `self · v` for a value `v`.
    #[inline]
    fn times(self, v: u16, t: &Tables) -> u16 {
        if v == 0 {
            return 0;
        }
        let s = self.0 + u32::from(t.log[v as usize]);
        t.exp[(if s >= N { s - N } else { s }) as usize]
    }
}

/// c (+)= b · a over 16-bit little-endian symbols.
fn multiply_accumulate(c: &mut [u8], a: &[u8], b: Index, init: bool) {
    let t = tables();
    for (cc, aa) in c.chunks_exact_mut(2).zip(a.chunks_exact(2)) {
        let p = b.times(u16::from_le_bytes([aa[0], aa[1]]), t);
        let v = if init { p } else { p ^ u16::from_le_bytes([cc[0], cc[1]]) };
        cc.copy_from_slice(&v.to_le_bytes());
    }
}

/// a_ij = 1 / (x_i + y_j) with row `i` (an ident) and column `j` (a data block).
fn cauchy(i: u16, j: u16) -> Index {
    Index::of(i ^ j).rcp()
}

/// Encode coded block `ident` (≥ `data.len() / block_bytes`) of the data blocks in `data`
/// (`count` blocks of `block_bytes` each, `block_bytes` even) into `block`.
pub fn encode(data: &[u8], block: &mut [u8], ident: u16, block_bytes: usize) {
    let count = data.len() / block_bytes;
    assert!(block_bytes % 2 == 0 && count >= 1 && usize::from(ident) >= count && block.len() == block_bytes);
    for (k, d) in data.chunks_exact(block_bytes).enumerate() {
        multiply_accumulate(block, d, cauchy(ident, k as u16), k == 0);
    }
}

/// Recover all data blocks from `count` coded blocks: `blocks` holds them one after the
/// other (`block_bytes` each), `idents` their identifiers. Writes `count` blocks to `data`.
pub fn decode(data: &mut [u8], blocks: &[u8], idents: &[u16], block_bytes: usize) {
    let columns: Vec<u16> = (0..idents.len() as u16).collect();
    decode_columns(data, blocks, idents, &columns, block_bytes);
}

/// Add data block `column`'s share to coded block `ident`: `block += data / (ident +
/// column)`. Adding is subtracting, so this also takes a data block in hand out of a
/// coded one.
pub fn accumulate(block: &mut [u8], data: &[u8], ident: u16, column: u16) {
    assert!(ident != column && block.len() == data.len() && block.len().is_multiple_of(2));
    multiply_accumulate(block, data, cauchy(ident, column), false);
}

/// Recover the data blocks `columns` from as many coded blocks that hold only their shares
/// (the other data blocks' taken out, see [`accumulate`]): `blocks` one after the other,
/// `idents` their identifiers. Writes the blocks of `columns`, in that order, to `data`.
pub fn decode_columns(data: &mut [u8], blocks: &[u8], idents: &[u16], columns: &[u16], block_bytes: usize) {
    let n = idents.len();
    assert!(columns.len() == n && blocks.len() == n * block_bytes && data.len() == n * block_bytes && block_bytes.is_multiple_of(2));
    for i in 0..n {
        // The row of the inverse Cauchy matrix for data block `columns[i]` (closed form, as
        // in the original's `inverse_cauchy_matrix`, there with columns 0…n−1).
        let col_i = columns[i];
        let (mut row_num, mut row_den) = (Index::ONE, Index::ONE);
        for k in 0..n {
            row_num = row_num.mul(Index::of(idents[k] ^ col_i));
            if k != i {
                row_den = row_den.mul(Index::of(col_i ^ columns[k]));
            }
        }
        let out = &mut data[i * block_bytes..(i + 1) * block_bytes];
        for j in 0..n {
            let (mut num, mut den) = (row_num, row_den);
            for k in 0..n {
                num = num.mul(Index::of(idents[j] ^ columns[k]));
                if k != j {
                    den = den.mul(Index::of(idents[j] ^ idents[k]));
                }
            }
            let b = num.div(Index::of(idents[j] ^ col_i).mul(den));
            multiply_accumulate(out, &blocks[j * block_bytes..(j + 1) * block_bytes], b, j == 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::xorshift::Xorshift32;

    #[test]
    fn any_count_blocks_recover_the_data() {
        let (count, bytes) = (4usize, 64usize);
        let mut rng = Xorshift32::default();
        let data: Vec<u8> = (0..count * bytes).map(|_| rng.next() as u8).collect();
        let idents: Vec<u16> = (0..7).map(|i| (count + i) as u16).collect();
        let coded: Vec<Vec<u8>> = idents
            .iter()
            .map(|&id| {
                let mut b = vec![0u8; bytes];
                encode(&data, &mut b, id, bytes);
                b
            })
            .collect();
        for pick in [[0, 1, 2, 3], [6, 4, 2, 0], [3, 5, 6, 1]] {
            let blocks: Vec<u8> = pick.iter().flat_map(|&p| coded[p].clone()).collect();
            let ids: Vec<u16> = pick.iter().map(|&p| idents[p]).collect();
            let mut out = vec![0u8; count * bytes];
            decode(&mut out, &blocks, &ids, bytes);
            assert_eq!(out, data, "{pick:?}");
        }
    }

    #[test]
    fn data_and_coded_blocks_mixed_recover_the_data() {
        let (count, bytes) = (5usize, 32usize);
        let mut rng = Xorshift32::default();
        let data: Vec<u8> = (0..count * bytes).map(|_| rng.next() as u8).collect();
        let block = |id: u16| -> Vec<u8> {
            if usize::from(id) < count {
                data[usize::from(id) * bytes..][..bytes].to_vec()
            } else {
                let mut b = vec![0u8; bytes];
                encode(&data, &mut b, id, bytes);
                b
            }
        };
        // Data blocks in hand, coded ones making up for the rest.
        for (have, coded) in [(vec![0, 1, 2, 3], vec![5]), (vec![1, 3], vec![9, 6, 5]), (vec![4], vec![5, 6, 7, 8]), (vec![], vec![5, 6, 7, 8, 9])] {
            let missing: Vec<u16> = (0..count as u16).filter(|j| !have.contains(j)).collect();
            let mut rows = Vec::new();
            for &id in &coded {
                let mut b = block(id);
                for &j in &have {
                    accumulate(&mut b, &block(j), id, j);
                }
                rows.extend(b);
            }
            let mut out = vec![0u8; missing.len() * bytes];
            decode_columns(&mut out, &rows, &coded, &missing, bytes);
            for (i, &j) in missing.iter().enumerate() {
                assert_eq!(out[i * bytes..][..bytes], block(j)[..], "{have:?} {coded:?}: block {j}");
            }
        }
    }
}

//! Single bits of byte arrays, most (`be`) or least (`le`) significant bit first
//! (aicodix/code `bitman.hh`).

#[inline]
pub fn get_be_bit(buf: &[u8], pos: usize) -> bool {
    (buf[pos / 8] >> (7 - pos % 8)) & 1 != 0
}

#[inline]
pub fn get_le_bit(buf: &[u8], pos: usize) -> bool {
    (buf[pos / 8] >> (pos % 8)) & 1 != 0
}

#[inline]
pub fn set_be_bit(buf: &mut [u8], pos: usize, val: bool) {
    let mask = 1 << (7 - pos % 8);
    buf[pos / 8] = (buf[pos / 8] & !mask) | if val { mask } else { 0 };
}

#[inline]
pub fn set_le_bit(buf: &mut [u8], pos: usize, val: bool) {
    let mask = 1 << (pos % 8);
    buf[pos / 8] = (buf[pos / 8] & !mask) | if val { mask } else { 0 };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_orders() {
        let mut b = [0u8; 2];
        set_be_bit(&mut b, 0, true);
        set_le_bit(&mut b, 8, true);
        assert_eq!(b, [0x80, 0x01]);
        assert!(get_be_bit(&b, 0) && get_le_bit(&b, 7) && get_le_bit(&b, 8) && get_be_bit(&b, 15));
        set_be_bit(&mut b, 0, false);
        assert_eq!(b[0], 0);
    }
}

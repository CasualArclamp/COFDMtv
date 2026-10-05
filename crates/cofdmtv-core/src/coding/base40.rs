//! Call signs packed in base 40 (space, `/`, 0–9, A–Z), up to nine characters, as the
//! aicodix modem's preamble carries them (`base40_encoder`/`base40_decoder`).

/// Characters by value (values 0–2 print as spaces; 1 and 2 are unused).
pub const ALPHABET: &[u8; 40] = b"   /0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
/// 40^9: packed values must stay below.
pub const LIMIT: u64 = 262_144_000_000_000;

/// Pack a call sign; `None` for characters outside the alphabet.
pub fn encode(call: &str) -> Option<u64> {
    call.bytes().try_fold(0u64, |acc, c| {
        let v = match c {
            b' ' => 0,
            b'/' => 3,
            b'0'..=b'9' => u64::from(c - b'0') + 4,
            b'a'..=b'z' => u64::from(c - b'a') + 14,
            b'A'..=b'Z' => u64::from(c - b'A') + 14,
            _ => return None,
        };
        Some(acc * 40 + v)
    })
}

/// Unpack to nine characters, right-aligned.
pub fn decode(mut val: u64) -> String {
    let mut s = [b' '; 9];
    for c in s.iter_mut().rev() {
        *c = ALPHABET[(val % 40) as usize];
        val /= 40;
    }
    String::from_utf8_lossy(&s).into_owned()
}

/// Why a call sign cannot be sent over the modem.
pub fn check(call: &str) -> Result<u64, String> {
    let call = call.trim();
    if call.is_empty() {
        return Err("enter a call sign".into());
    }
    if call.len() > 9 {
        return Err("a call sign has at most 9 characters".into());
    }
    match encode(call) {
        Some(v) if v > 0 && v < LIMIT => Ok(v),
        _ => Err(format!("'{call}': letters, digits and '/' only")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        assert_eq!(decode(encode("DL1ABC/P").unwrap()), " DL1ABC/P");
        assert_eq!(encode("x-y"), None);
        assert!(check("DL1ABC").is_ok() && check("").is_err() && check("TOOLONGCALL").is_err());
    }
}

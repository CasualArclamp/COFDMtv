//! Call signs packed in base 37 (space, 0–9, A–Z), up to nine characters, as the COFDMTV
//! preamble carries them (Shredpix/Assempix/Rattlegram `base37`).

/// Characters of the alphabet by value.
pub const ALPHABET: &[u8; 37] = b" 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
/// Longest call sign.
pub const MAX_LEN: usize = 9;
/// 37^9: packed values must stay below.
pub const LIMIT: u64 = 129_961_739_795_077;

/// The value of a character: digits 1–10, letters (either case) 11–36, anything else 0.
pub fn digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0' + 1,
        b'a'..=b'z' => c - b'a' + 11,
        b'A'..=b'Z' => c - b'A' + 11,
        _ => 0,
    }
}

/// Pack a call sign (the first nine characters count).
pub fn encode(call: &str) -> u64 {
    call.bytes().take(MAX_LEN).fold(0, |acc, c| 37 * acc + u64::from(digit(c)))
}

/// Unpack to nine characters, right-aligned (leading spaces), as the apps show them.
pub fn decode(mut val: u64) -> String {
    let mut s = [b' '; MAX_LEN];
    for c in s.iter_mut().rev() {
        *c = ALPHABET[(val % 37) as usize];
        val /= 37;
    }
    String::from_utf8_lossy(&s).into_owned()
}

/// Why a call sign cannot be sent.
pub fn check(call: &str) -> Result<(), String> {
    let call = call.trim();
    if call.is_empty() {
        return Err("enter a call sign".into());
    }
    if call.len() > MAX_LEN {
        return Err(format!("a call sign has at most {MAX_LEN} characters"));
    }
    if let Some(c) = call.chars().find(|c| !c.is_ascii_alphanumeric()) {
        return Err(format!("'{c}' cannot be sent: only letters and digits"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        assert_eq!(decode(encode("DL1ABC")), "   DL1ABC");
        assert_eq!(decode(encode("anonymous")), "ANONYMOUS");
        assert!(encode("ZZZZZZZZZ") < LIMIT);
        assert_eq!(encode("ZZZZZZZZZ"), LIMIT - 1);
        assert!(check("DL1ABC").is_ok() && check("DL1/ABC").is_err() && check("TOOLONGCALL").is_err());
    }
}

pub const FNV_OFFSET_BASIS: u32 = 2166136261;
pub const FNV_PRIME: u32 = 16777619;

/// Computes the standard 32-bit FNV-1a hash of a byte slice.
pub fn fnv1a_32(bytes: &[u8]) -> u32 {
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in bytes {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Computes the standard 32-bit FNV-1a hash of a string, converted to lowercase.
/// This matches how Wwise event IDs and lookup keys are resolved.
pub fn hash_string(s: &str) -> u32 {
    let lower = s.to_ascii_lowercase();
    fnv1a_32(lower.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fnv1a_32() {
        // Test vectors for FNV-1a 32-bit
        assert_eq!(fnv1a_32(b""), FNV_OFFSET_BASIS);
        // "a" — canonical FNV-1a 32 vector. The previous value (0x050C5D3F)
        // was a mistyped FNV-1 vector (FNV-1("a") is 0x050C5D7E), not FNV-1a.
        assert_eq!(fnv1a_32(b"a"), 0xE40C292C);
        // Canonical vector from the FNV reference test suite, to pin the
        // multi-byte path as well.
        assert_eq!(fnv1a_32(b"foobar"), 0xBF9CF968);
        // Wwise lowercase check
        assert_eq!(hash_string("Play_Sheepinator"), hash_string("play_sheepinator"));
    }
}

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
        // "a"
        assert_eq!(fnv1a_32(b"a"), 0x050C5D3F);
        // Wwise lowercase check
        assert_eq!(hash_string("Play_Sheepinator"), hash_string("play_sheepinator"));
    }
}

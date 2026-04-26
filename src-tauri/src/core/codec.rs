use crate::core::error::{Result, ToolkitError};

pub fn decompress(comp_data: &[u8], real_size: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; real_size];
    let mut ri = 0usize;
    let mut ci = 0usize;

    while ri <= real_size && ci < comp_data.len() {
        // direct block
        let a = comp_data[ci] as usize;
        ci += 1;
        let b = if (a & 0xF0) == 0xF0 {
            let v = comp_data[ci] as usize;
            ci += 1;
            v
        } else {
            0
        };

        let mut direct = (a >> 4) + b;
        while direct >= 270 && (direct - 15) % 255 == 0 {
            let v = comp_data[ci] as usize;
            ci += 1;
            direct += v;
            if v == 0 {
                break;
            }
        }

        for i in 0..direct {
            if ri + i >= real_size || ci + i >= comp_data.len() {
                break;
            }
            out[ri + i] = comp_data[ci + i];
        }
        ri += direct;
        ci += direct;

        if !(ri <= real_size && ci < comp_data.len()) {
            break;
        }

        // reverse block
        let ba = comp_data[ci] as usize;
        let bb = comp_data[ci + 1] as usize;
        ci += 2;
        let rev_offset = ba + (bb << 8);
        let mut reverse = (a & 0xF) + 4;

        if reverse == 19 {
            reverse += comp_data[ci] as usize;
            ci += 1;
            while reverse >= 274 && (reverse - 19) % 255 == 0 {
                let v = comp_data[ci] as usize;
                ci += 1;
                reverse += v;
                if v == 0 {
                    break;
                }
            }
        }

        for i in 0..reverse {
            let src = ri + i;
            let from = if src >= rev_offset { src - rev_offset } else { 0 };
            if src < real_size && from < real_size {
                out[src] = out[from];
            }
        }
        ri += reverse;
    }

    Ok(out)
}

pub fn detect_and_decompress(data: &[u8]) -> Result<Vec<u8>> {
    const DAT1_MAGIC: u32 = 0x44415431;

    if data.len() < 7 {
        return Err(ToolkitError::Parse("file too small".into()));
    }

    let normal_magic = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if normal_magic == DAT1_MAGIC {
        return Ok(data.to_vec());
    }

    let compressed_magic = u32::from_le_bytes(data[2..6].try_into().unwrap());
    let rare_magic = u32::from_le_bytes(data[1..5].try_into().unwrap());
    let rarest_magic = u32::from_le_bytes(data[3..7].try_into().unwrap());

    if compressed_magic == DAT1_MAGIC {
        let real_size = u32::from_le_bytes(data[10..14].try_into().unwrap()) as usize;
        return decompress(&data[14..], real_size);
    } else if rare_magic == DAT1_MAGIC {
        let real_size = u32::from_le_bytes(data[9..13].try_into().unwrap()) as usize;
        return decompress(&data[13..], real_size);
    } else if rarest_magic == DAT1_MAGIC {
        let real_size = u32::from_le_bytes(data[11..15].try_into().unwrap()) as usize;
        return decompress(&data[15..], real_size);
    }

    // not compressed, return as-is and let DAT1 parser handle the error
    Ok(data.to_vec())
}

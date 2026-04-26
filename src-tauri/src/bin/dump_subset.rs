use std::io::Cursor;
use byteorder::{LE, ReadBytesExt};
use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::codec::detect_and_decompress;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: dump_subset <model_file.model>");
        std::process::exit(1);
    }

    let data = std::fs::read(&args[1]).unwrap();
    let mut cur = Cursor::new(&data);
    let _magic = cur.read_u32::<LE>().unwrap();
    let _off = cur.read_u32::<LE>().unwrap();
    let _sz = cur.read_u32::<LE>().unwrap();
    let mut unk = [0u8; 24];
    std::io::Read::read_exact(&mut cur, &mut unk).unwrap();
    let pos = cur.position() as usize;
    let decompressed = detect_and_decompress(&data[pos..]).unwrap();
    let dat1 = Dat1::parse(&decompressed).unwrap();

    // Built section
    if let Some(built) = dat1.get_section_data(0x283D0383) {
        println!("=== Built section ({} bytes) ===", built.len());
        if built.len() >= 0x34 {
            let ox = f32::from_le_bytes(built[0x1C..0x20].try_into().unwrap());
            let oy = f32::from_le_bytes(built[0x20..0x24].try_into().unwrap());
            let oz = f32::from_le_bytes(built[0x24..0x28].try_into().unwrap());
            let scale = f32::from_le_bytes(built[0x2C..0x30].try_into().unwrap());
            let ukw5 = f32::from_le_bytes(built[0x30..0x34].try_into().unwrap());
            println!("  position_offset: ({}, {}, {})", ox, oy, oz);
            println!("  position_scale (0x2C): {}", scale);
            println!("  ukw5 (0x30, uv_log_scales reinterp): 0x{:08X} = {} as float",
                u32::from_le_bytes(built[0x30..0x34].try_into().unwrap()), ukw5);
            let iuvscale = i32::from_le_bytes(built[0x30..0x34].try_into().unwrap());
            let shift = (iuvscale & 0xF) as u32;
            let uv_scale = (1u32 << shift) as f32 / 16384.0;
            println!("  uv_scale (computed): {} (shift={})", uv_scale, shift);
        }
    }

    // Mesh/Subset definitions
    if let Some(mesh_data) = dat1.get_section_data(0x78D9CBDE) {
        let count = mesh_data.len() / 64;
        println!("\n=== Mesh definitions ({} meshes) ===", count);
        for i in 0..count {
            let entry = &mesh_data[i * 64..(i + 1) * 64];
            let ox = f32::from_le_bytes(entry[0..4].try_into().unwrap());
            let oy = f32::from_le_bytes(entry[4..8].try_into().unwrap());
            let oz = f32::from_le_bytes(entry[8..12].try_into().unwrap());
            let unk2 = u16::from_le_bytes(entry[12..14].try_into().unwrap());
            let unk3 = u16::from_le_bytes(entry[14..16].try_into().unwrap());
            let vs = u32::from_le_bytes(entry[20..24].try_into().unwrap());
            let is = u32::from_le_bytes(entry[24..28].try_into().unwrap());
            let ic = u32::from_le_bytes(entry[28..32].try_into().unwrap());
            let vc = u32::from_le_bytes(entry[32..36].try_into().unwrap());
            let flags = u16::from_le_bytes(entry[36..38].try_into().unwrap());
            println!("  mesh[{:2}] origin=({:10.4}, {:10.4}, {:10.4}) unk2={} unk3={} vs={} vc={} is={} ic={} flags=0x{:04X}",
                i, ox, oy, oz, unk2, unk3, vs, vc, is, ic, flags);
        }
    }
}

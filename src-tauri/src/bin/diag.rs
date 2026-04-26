use std::io::Cursor;
use byteorder::{LE, ReadBytesExt};
use omnitool_lib::core::dat1::Dat1;
use omnitool_lib::core::codec::detect_and_decompress;

fn parse_sections(data: &[u8]) -> Dat1 {
    let mut cur = Cursor::new(data);
    let _magic = cur.read_u32::<LE>().unwrap();
    let _off = cur.read_u32::<LE>().unwrap();
    let _sz = cur.read_u32::<LE>().unwrap();
    let mut unk = [0u8; 24];
    std::io::Read::read_exact(&mut cur, &mut unk).unwrap();
    let pos = cur.position() as usize;
    let decompressed = detect_and_decompress(&data[pos..]).unwrap();
    Dat1::parse(&decompressed).unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: diag <original.model> <modified.model>");
        std::process::exit(1);
    }

    let orig = parse_sections(&std::fs::read(&args[1]).unwrap());
    let modv = parse_sections(&std::fs::read(&args[2]).unwrap());

    let ov = orig.get_section_data(0xA98BE69B).unwrap();
    let mv = modv.get_section_data(0xA98BE69B).unwrap();
    let vc = ov.len().min(mv.len()) / 16;

    let mut pos_diff = 0u32;
    let mut norm_diff = 0u32;
    let mut uv_diff = 0u32;
    let mut w_diff = 0u32;
    let mut samples_printed = 0;

    for i in 0..vc {
        let o = &ov[i*16..(i+1)*16];
        let m = &mv[i*16..(i+1)*16];
        if o == m { continue; }

        let pos_changed = o[0..6] != m[0..6];
        let w_changed = o[6..8] != m[6..8];
        let norm_changed = o[8..12] != m[8..12];
        let uv_changed = o[12..16] != m[12..16];

        if pos_changed { pos_diff += 1; }
        if norm_changed { norm_diff += 1; }
        if uv_changed { uv_diff += 1; }
        if w_changed { w_diff += 1; }

        if samples_printed < 5 {
            let ox = i16::from_le_bytes([o[0],o[1]]);
            let mx = i16::from_le_bytes([m[0],m[1]]);
            let oy = i16::from_le_bytes([o[2],o[3]]);
            let my = i16::from_le_bytes([m[2],m[3]]);
            let oz = i16::from_le_bytes([o[4],o[5]]);
            let mz = i16::from_le_bytes([m[4],m[5]]);
            let on = u32::from_le_bytes([o[8],o[9],o[10],o[11]]);
            let mn = u32::from_le_bytes([m[8],m[9],m[10],m[11]]);
            println!("v[{}] pos:({},{},{})->({},{},{}) norm:0x{:08X}->0x{:08X} pos?{} norm?{} uv?{} w?{}",
                i, ox,oy,oz, mx,my,mz, on,mn, pos_changed, norm_changed, uv_changed, w_changed);
            samples_printed += 1;
        }
    }
    println!("\nTotal verts: {}  Sizes: orig={} mod={}", vc, ov.len(), mv.len());
    println!("Diffs -> pos:{} norm:{} uv:{} w:{}", pos_diff, norm_diff, uv_diff, w_diff);

    // Check mesh definitions
    let om = orig.get_section_data(0x78D9CBDE).unwrap();
    let mm = modv.get_section_data(0x78D9CBDE).unwrap();
    let mc = om.len().min(mm.len()) / 64;
    let mut mesh_diffs = 0;
    for i in 0..mc {
        if om[i*64..(i+1)*64] != mm[i*64..(i+1)*64] {
            mesh_diffs += 1;
            if mesh_diffs <= 5 {
                let mut oc = Cursor::new(&om[i*64..(i+1)*64]);
                let mut mc2 = Cursor::new(&mm[i*64..(i+1)*64]);
                oc.set_position(36); mc2.set_position(36);
                let of = oc.read_u16::<LE>().unwrap();
                let mf = mc2.read_u16::<LE>().unwrap();
                println!("mesh[{}] flags: 0x{:X} -> 0x{:X}", i, of, mf);
            }
        }
    }
    println!("Mesh diffs: {}/{}", mesh_diffs, mc);

    // Check skin data sizes
    for (tag, name) in [(0xDCA379A2u32, "skin_data"), (0xC61B1FF5, "skin_batch"), (0xCCBAFF15, "rcra_skin")] {
        let os = orig.get_section_data(tag).map(|d| d.len());
        let ms = modv.get_section_data(tag).map(|d| d.len());
        println!("{}: orig={:?} mod={:?}", name, os, ms);
    }
}

//! Encodes a PNG as an uncompressed sRGB `.texture` on a donor texture's headers (alpha kept).
//!
//! usage: png2texture <donor.texture> <in.png> <out.texture>   (sides must be powers of two)

use omnitool_lib::tools::texture_converter::texture_from_rgba;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        eprintln!("usage: png2texture <donor.texture> <in.png> <out.texture>");
        std::process::exit(1);
    }
    let donor = std::fs::read(&args[0]).expect("read donor texture");
    let image = image::open(&args[1]).expect("read png").to_rgba8();
    let out = texture_from_rgba(&donor, &image).unwrap_or_else(|e| panic!("encode: {e}"));
    std::fs::write(&args[2], &out).expect("write texture");
    println!("{}: {}x{} RGBA, {} bytes", args[2], image.width(), image.height(), out.len());
}

use omnitool_lib::tools::model_converter::model::ModelFile;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 { eprintln!("usage: sections <model>"); std::process::exit(1); }
    let data = std::fs::read(&args[1]).expect("read");
    let m = ModelFile::parse(&data).expect("parse");
    println!("{} sections", m.dat1.sections.len());
    for (i, s) in m.dat1.sections.iter().enumerate() {
        let sz = m.dat1.section_data[i].len();
        println!("  0x{:08X}  off=0x{:08X}  size={}", s.tag, s.offset, sz);
    }
}

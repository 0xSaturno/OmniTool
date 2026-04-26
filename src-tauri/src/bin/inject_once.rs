use omnitool_lib::tools::model_converter::{
    ascii_reader::{inject_ascii, parse_ascii},
    model::ModelFile,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: inject_once <ascii_file.ascii> <base_file.model> <out_file.model>");
        std::process::exit(1);
    }

    let ascii_text = std::fs::read_to_string(&args[1]).expect("failed to read ascii file");
    let ascii = parse_ascii(&ascii_text).expect("failed to parse ascii");

    let model_data = std::fs::read(&args[2]).expect("failed to read base model file");
    let mut model = ModelFile::parse(&model_data).expect("failed to parse base model");

    inject_ascii(&mut model, &ascii).expect("failed to inject ascii");

    let out = model.save();
    std::fs::write(&args[3], out).expect("failed to write output model");
}

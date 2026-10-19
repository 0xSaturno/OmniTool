//! Lists, for every dag asset whose path contains the text, what it loads and what loads it.
//!
//! usage: dag_query <dag> <text>

use std::path::Path;

use omnitool_lib::core::dag::Dag;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: dag_query <dag> <text>");
        std::process::exit(1);
    }
    let dag = Dag::load(Path::new(&args[0])).expect("read dag");
    let text = args[1].to_lowercase();
    let dependents = dag.dependents_index(None);
    let name = |i: usize| dag.name(i).map(|n| n.replace('\\', "/")).unwrap_or_else(|| format!("{:016X}", dag.id(i)));
    for i in 0..dag.len() {
        if !name(i).to_lowercase().contains(&text) {
            continue;
        }
        println!("{}", name(i));
        for d in dag.direct_dependencies(i, None) {
            println!("  loads     {}", name(d));
        }
        for &p in dependents.direct(i) {
            println!("  loaded by {}", name(p as usize));
        }
    }
}

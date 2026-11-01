use std::io::{self, Read};

fn main() {
    let mut input = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut input) {
        eprintln!("read request: {error}");
        std::process::exit(1);
    }
    match rightkit_dictation_engine::l3_cleanup::render_prompt_convergence_payloads_json(&input) {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

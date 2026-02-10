fn main() {
    if let Err(err) = flac_cue_split::run() {
        eprintln!("error: {}", err);
        std::process::exit(1);
    }
}

use clap::Parser;

fn main() {
    if let Err(error) = akeep::run(akeep::cli::Cli::parse()) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

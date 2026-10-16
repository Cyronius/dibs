fn main() {
    std::process::exit(dibs::cli::main(std::env::args().skip(1).collect()));
}

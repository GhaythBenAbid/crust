//! crust — a fast, curl-compatible HTTP client.

mod args;
mod glob;
mod http;
mod tls;
mod url;
mod util;

use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match args::parse(argv) {
        Ok(args::Parsed::Run(opts)) => ExitCode::from(http::run(&opts)),
        Ok(args::Parsed::Exit) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("crust: {}", e.msg);
            eprintln!("crust: try 'crust --help' for more information");
            ExitCode::from(e.code)
        }
    }
}

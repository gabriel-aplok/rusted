mod build;
mod cache;
mod cli;
mod config;
mod manifest;
mod profile;
mod registry;
mod remote;
mod report;
mod resolver;
mod toml;
mod util;
mod workspace;

fn main() {
    #[cfg(unix)]
    unsafe {
        extern "C" {
            fn signal(signal: std::os::raw::c_int, handler: usize) -> usize;
        }
        const SIGPIPE: std::os::raw::c_int = 13;
        const SIG_DFL: usize = 0;
        signal(SIGPIPE, SIG_DFL);
    }
    std::process::exit(cli::run(std::env::args_os().skip(1)));
}

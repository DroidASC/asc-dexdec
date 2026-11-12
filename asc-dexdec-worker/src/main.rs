//! `asc-dexdec-worker`: upstream dexdec as asc's built-in `dexdec` decompile
//! backend, serving asc's backend protocol on stdin / stdout.
//!
//! asc starts it as a child process, so a stack overflow or an out-of-memory
//! abort in the decompiler only ends the worker. Run with no arguments;
//! `--version` prints the worker's version, `--protocol-version` the protocol
//! version it speaks (asc's installer checks that one).

mod fault;
mod observer;
mod options;
mod pool;
mod server;
mod session;
mod stdio;

use std::process::ExitCode;

use asc_decompile_protocol::PROTOCOL_VERSION;

// Decompiling is dominated by short-lived IR and AST allocations across the
// rayon pool; upstream's own CLI uses mimalloc for the same reason.
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The worker's version, reported by `--version` and `initialize`.
const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "usage: asc-dexdec-worker [--version | --protocol-version]
speaks asc's decompile backend protocol on stdio; asc starts it itself";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next()) {
        (None, _) => match server::serve_stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("asc-dexdec-worker: {err}");
                ExitCode::FAILURE
            }
        },
        (Some(flag), None) if flag == "--version" || flag == "-V" => {
            println!("asc-dexdec-worker {VERSION}");
            ExitCode::SUCCESS
        }
        (Some(flag), None) if flag == "--protocol-version" => {
            println!("{PROTOCOL_VERSION}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

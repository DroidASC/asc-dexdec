//! Faults injected through [`FAULT_ENV`], for tests of the process boundary
//! (this crate's and asc's).

use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

use crate::observer::CancelFlag;

/// Environment variable naming a fault to inject: `panic:<descriptor>`
/// panics while decompiling that class, `stall:<descriptor>` holds that class
/// until the request is cancelled (at most a minute), `print:<descriptor>`
/// writes to stdout while decompiling it. Never set it in production.
pub(crate) const FAULT_ENV: &str = "ASC_DEXDEC_WORKER_FAULT";

#[derive(Debug, Default)]
pub(crate) enum Fault {
    #[default]
    None,
    Panic(String),
    Stall(String),
    Print(String),
}

impl Fault {
    pub(crate) fn from_env() -> Self {
        let Some(spec) = std::env::var_os(FAULT_ENV) else {
            return Self::None;
        };
        let spec = spec.to_string_lossy();
        match spec.split_once(':') {
            Some(("panic", class)) => Self::Panic(class.to_owned()),
            Some(("stall", class)) => Self::Stall(class.to_owned()),
            Some(("print", class)) => Self::Print(class.to_owned()),
            _ => {
                eprintln!("asc-dexdec-worker: ignoring {FAULT_ENV}={spec}");
                Self::None
            }
        }
    }

    /// Fires the fault if it targets DEX class `class`.
    pub(crate) fn inject(&self, class: &str, cancel: &CancelFlag) {
        match self {
            Self::Panic(target) if target == class => panic!("injected fault in {class}"),
            Self::Stall(target) if target == class => {
                let deadline = Instant::now() + Duration::from_secs(60);
                while !cancel.is_cancelled() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(5));
                }
            }
            // Half a frame and then a whole line, as a careless dependency would.
            Self::Print(target) if target == class => {
                print!("{{\"jsonrpc\":\"2.0\",");
                println!("stray stdout while decompiling {class}");
                // Best effort: a failed flush only loses the stray text.
                let _ = io::stdout().flush();
            }
            Self::None | Self::Panic(_) | Self::Stall(_) | Self::Print(_) => {}
        }
    }
}

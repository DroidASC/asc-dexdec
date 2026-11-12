//! The large-stack thread pool decompilation runs on, and panic isolation.

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::sync::OnceLock;

/// Stack reserved for each decompiling thread.
///
/// dexdec's lowering, type flow and printers recurse over nested control flow
/// and expressions, several KiB of native stack per level; rayon's default
/// 2 MiB overflows on deep methods. The reservation only costs address space
/// until it is touched.
const STACK_SIZE: usize = 64 * 1024 * 1024;

/// Runs `op` on the worker's own rayon pool, so `op` and every parallel
/// iterator dexdec starts inside it run on [`STACK_SIZE`] stacks. If the pool
/// cannot be created, `op` runs on the calling thread.
pub(crate) fn install<R: Send>(op: impl FnOnce() -> R + Send) -> R {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .stack_size(STACK_SIZE)
            .thread_name(|index| format!("dexdec-{index}"))
            .build()
            .inspect_err(|err| eprintln!("asc-dexdec-worker: no large-stack pool ({err}); decompiling on the request thread"))
            .ok()
    });
    match pool {
        Some(pool) => pool.install(op),
        None => op(),
    }
}

/// Runs `op`, turning a panic into its message instead of unwinding.
///
/// Callers must not reuse state `op` may have left half-updated.
pub(crate) fn catch_panic<R>(op: impl FnOnce() -> R) -> Result<R, String> {
    panic::catch_unwind(AssertUnwindSafe(op)).map_err(|payload| panic_message(payload.as_ref()))
}

/// The message a panic was raised with.
pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic with a non-string payload".to_owned())
}

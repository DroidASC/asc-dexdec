//! Keeping stdout for protocol frames only.

use std::io;

/// Where the frames go: a private duplicate of stdout, with stdout itself
/// (fd 1) then pointing at stderr. Output that the decompiler or a dependency
/// prints lands in the worker's log instead of in the middle of a frame.
///
/// When stderr cannot take over fd 1 (it is closed), stdout stays as it is
/// and the frames still go to the duplicate.
#[cfg(unix)]
pub(crate) fn protocol_stdout() -> io::Result<std::fs::File> {
    use std::io::Write;
    use std::os::fd::AsFd;

    // The lock keeps other threads from writing while fd 1 moves.
    let mut stdout = io::stdout().lock();
    stdout.flush()?;
    let frames = stdout.as_fd().try_clone_to_owned()?;
    // SAFETY: dup2 only swaps which open file fd 1 refers to; the duplicate
    // above keeps the original open, and no Rust object owns fd 1 or 2.
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
        eprintln!(
            "asc-dexdec-worker: cannot point stdout at stderr: {}",
            io::Error::last_os_error()
        );
    }
    Ok(std::fs::File::from(frames))
}

/// Where the frames go: stdout itself; nothing guards it from stray output.
#[cfg(not(unix))]
pub(crate) fn protocol_stdout() -> io::Result<io::Stdout> {
    Ok(io::stdout())
}

//! Bound on every file and stdin stream this program loads into memory.
//!
//! The measured input of 104857600 bytes (100 MiB) stays under this.
//! A larger input is refused before its body is kept.
//! The fetch HTTP body cap is a separate, smaller limit and is not this one.

use std::io::{self, Read};
use std::path::Path;

/// Largest input accepted per file or stdin stream, in bytes (256 MiB).
pub const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;

fn too_large() -> io::Error {
    io::Error::new(
        io::ErrorKind::FileTooLarge,
        format!("file is larger than {MAX_INPUT_BYTES} bytes"),
    )
}

/// Read `path`, or standard input when `path` is `-`, up to [`MAX_INPUT_BYTES`].
///
/// A regular file past the limit is refused from its size, so the body is
/// not loaded. Standard input and any other stream are read at most one
/// byte past the limit.
pub fn read_capped(path: &Path) -> io::Result<Vec<u8>> {
    if path == Path::new("-") {
        return read_capped_reader(io::stdin());
    }
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_file() && meta.len() > MAX_INPUT_BYTES {
            return Err(too_large());
        }
    }
    let file = std::fs::File::open(path)?;
    read_capped_reader(file)
}

/// Read at most one byte past [`MAX_INPUT_BYTES`]. A longer stream is refused
/// and the rest is not kept.
fn read_capped_reader(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    // `read_to_end` retries EINTR. The wrapper turns a caught SIGINT into a
    // different error so the read stops instead of continuing.
    crate::interrupt::StopOnSigint::new(reader)
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_INPUT_BYTES {
        return Err(too_large());
    }
    Ok(buf)
}

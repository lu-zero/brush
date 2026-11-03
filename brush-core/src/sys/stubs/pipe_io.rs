//! Reads and writes on pipes, where there's no telling them not to block.

use std::io::{self, Read as _, Write as _};

/// Reads from `reader` into `buf`, as [`std::io::Read::read`] does.
pub(crate) fn read(reader: &std::io::PipeReader, buf: &mut [u8]) -> io::Result<usize> {
    let mut reader = reader;
    reader.read(buf)
}

/// Writes `buf` to `writer`, as [`std::io::Write::write`] does.
pub(crate) fn write(writer: &std::io::PipeWriter, buf: &[u8]) -> io::Result<usize> {
    let mut writer = writer;
    writer.write(buf)
}

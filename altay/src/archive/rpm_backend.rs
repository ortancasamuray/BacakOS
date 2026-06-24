// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! RPM package backend (read/extract). An RPM is: a 96-byte lead, a signature
//! header, the main header, then a compressed cpio payload. We parse the
//! headers to find the payload offset, sniff its compressor by magic bytes, and
//! walk the `newc` cpio entries — all in-process.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::filesystem::Progress;

use super::{safe_join, ArchiveError, Backend, Member, Options};

pub struct RpmBackend;

impl Backend for RpmBackend {
    fn list(&self, archive: &Path, _opts: &Options) -> Result<Vec<Member>, ArchiveError> {
        let mut reader = payload_reader(archive)?;
        let mut out = Vec::new();
        walk_cpio(&mut reader, |name, mode, size, data| {
            // Skip the data (list only needs metadata).
            io::copy(&mut data.take(size), &mut io::sink())?;
            out.push(Member {
                path: PathBuf::from(name.trim_start_matches("./")),
                is_dir: is_dir(mode),
                size,
                compressed_size: size,
                encrypted: false,
            });
            Ok(())
        })?;
        Ok(out)
    }

    fn extract(
        &self,
        archive: &Path,
        into: &Path,
        _opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        let mut reader = payload_reader(archive)?;
        let mut state = Progress { bytes_done: 0, bytes_total: 0, files_done: 0, files_total: 0 };
        walk_cpio(&mut reader, |name, mode, size, data| {
            let rel = name.trim_start_matches("./");
            let out_path = safe_join(into, Path::new(rel))?;
            if is_dir(mode) {
                std::fs::create_dir_all(&out_path)?;
                io::copy(&mut data.take(size), &mut io::sink())?;
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut out = std::fs::File::create(&out_path)?;
                io::copy(&mut data.take(size), &mut out)?;
            }
            state.bytes_done = state.bytes_done.saturating_add(size);
            state.files_done += 1;
            on_progress(state);
            Ok(())
        })?;
        Ok(())
    }

    fn create(
        &self,
        _archive: &Path,
        _sources: &[PathBuf],
        _opts: &Options,
        _on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        Err(ArchiveError::Unsupported)
    }
}

fn is_dir(mode: u32) -> bool {
    mode & 0o170000 == 0o040000
}

/// Open the RPM, find the payload, and wrap it in the right decompressor.
fn payload_reader(archive: &Path) -> Result<Box<dyn Read>, ArchiveError> {
    let mut f = std::fs::File::open(archive)?;
    let offset = payload_offset(&mut f)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut magic = [0u8; 6];
    let n = f.read(&mut magic)?;
    let magic = &magic[..n];
    f.seek(SeekFrom::Start(offset))?;
    let dec: Box<dyn Read> = if magic.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::GzDecoder::new(f))
    } else if magic.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Box::new(xz2::read::XzDecoder::new(f))
    } else if magic.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Box::new(zstd::stream::read::Decoder::new(f)?)
    } else if magic.starts_with(&[0x42, 0x5a, 0x68]) {
        Box::new(bzip2::read::BzDecoder::new(f))
    } else {
        return Err(ArchiveError::Backend(
            "unsupported RPM payload compressor (expected gzip/xz/zstd/bzip2)".into(),
        ));
    };
    Ok(dec)
}

/// Parse lead + signature header + main header to find the payload offset.
fn payload_offset(f: &mut std::fs::File) -> Result<u64, ArchiveError> {
    let mut lead = [0u8; 96];
    f.read_exact(&mut lead)?;
    if lead[0..4] != [0xED, 0xAB, 0xEE, 0xDB] {
        return Err(ArchiveError::Backend("not an RPM (bad lead magic)".into()));
    }
    let after_sig = skip_header(f, 96, true)?;
    let after_main = skip_header(f, after_sig, false)?;
    Ok(after_main)
}

/// Skip one RPM header structure starting at `off`; pad to 8 bytes for the
/// signature header. Returns the offset just past it.
fn skip_header(f: &mut std::fs::File, off: u64, pad8: bool) -> Result<u64, ArchiveError> {
    f.seek(SeekFrom::Start(off))?;
    let mut intro = [0u8; 16];
    f.read_exact(&mut intro)?;
    if intro[0..4] != [0x8e, 0xad, 0xe8, 0x01] {
        return Err(ArchiveError::Backend("bad RPM header magic".into()));
    }
    let nindex = u32::from_be_bytes([intro[8], intro[9], intro[10], intro[11]]) as u64;
    let hsize = u32::from_be_bytes([intro[12], intro[13], intro[14], intro[15]]) as u64;
    let mut end = off + 16 + nindex * 16 + hsize;
    if pad8 {
        end = (end + 7) & !7;
    }
    Ok(end)
}

/// Walk a `newc` cpio stream, invoking `f(name, mode, filesize, reader)` for
/// each entry. The callback must read exactly `filesize` bytes from `reader`.
fn walk_cpio<R: Read>(
    r: &mut R,
    mut f: impl FnMut(&str, u32, u64, &mut R) -> Result<(), ArchiveError>,
) -> Result<(), ArchiveError> {
    loop {
        let mut header = [0u8; 110];
        r.read_exact(&mut header).map_err(ArchiveError::Io)?;
        if &header[0..6] != b"070701" && &header[0..6] != b"070702" {
            return Err(ArchiveError::Backend("unsupported cpio format in RPM payload".into()));
        }
        let field = |i: usize| -> u32 {
            let s = std::str::from_utf8(&header[6 + i * 8..6 + i * 8 + 8]).unwrap_or("0");
            u32::from_str_radix(s, 16).unwrap_or(0)
        };
        let mode = field(1);
        let filesize = field(6) as u64;
        let namesize = field(11) as u64;

        let mut name_buf = vec![0u8; namesize as usize];
        r.read_exact(&mut name_buf).map_err(ArchiveError::Io)?;
        // Header(110) + name padded to a 4-byte boundary.
        skip(r, pad4(110 + namesize))?;
        let name = String::from_utf8_lossy(&name_buf).trim_end_matches('\0').to_string();

        if name == "TRAILER!!!" {
            return Ok(());
        }

        f(&name, mode, filesize, r)?;
        // The callback consumed `filesize` bytes; skip the data padding.
        skip(r, pad4(filesize))?;
    }
}

/// Bytes needed to pad `n` up to the next multiple of 4.
fn pad4(n: u64) -> u64 {
    (4 - (n % 4)) % 4
}

fn skip<R: Read>(r: &mut R, n: u64) -> Result<(), ArchiveError> {
    if n > 0 {
        io::copy(&mut r.take(n), &mut io::sink()).map_err(ArchiveError::Io)?;
    }
    Ok(())
}

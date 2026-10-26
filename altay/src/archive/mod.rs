// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Archive support. Phase 1 defines the format taxonomy and the trait every
//! backend implements; concrete backends (zip, tar family, 7z, rar, …) are
//! filled in per format in later phases. All archive paths are sandbox-checked
//! before any backend touches the disk.

use std::path::{Path, PathBuf};

use crate::filesystem::Progress;
use crate::security::{AccessDenied, Sandbox};

pub use exclude::Exclude;

mod cmd_backend;
mod deb_backend;
pub mod exclude;
mod rar_backend;
mod rpm_backend;
mod sevenz;
mod single;
mod tar_backend;
mod zip_backend;

/// Every archive container/codec Altay intends to support, mapped from the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    // Zip family
    Zip, Zipx,
    // 7-zip
    SevenZ,
    // RAR / comic-book variants
    Rar, Cbr,
    // Legacy containers (7z-backed, read-only)
    Ace, Alz, Arj, Lzh, Zoo,
    // Plain tar
    Tar,
    // Tar + standard codecs
    TarGz, TarBz2, TarXz, TarZst,
    // Tar + extended codecs
    TarLz, TarLzo, TarLzma, TarBr, TarZ,
    // Single-file standard codecs
    Gzip, Bzip2, Bz, Xz, Lzma, Zstd,
    // Single-file extended codecs
    Lzip, Lzop, Brotli, Compress, Rzip,
    // System / package / disk image
    Cab, Ar, Cpio, Iso, Dmg, Wim,
    Apk, Jar, War, Ear, Deb, Rpm, Pkg,
}

impl Format {
    /// Detect a format, preferring file content (magic bytes) and falling back
    /// to the extension. Content detection catches mis-named or extension-less
    /// archives.
    pub fn detect(path: &Path) -> Option<Format> {
        if let Some(by_magic) = Self::from_magic(path) {
            return Some(by_magic);
        }
        Self::from_path(path)
    }

    /// Sniff the leading bytes of the file. Returns `None` if unreadable or
    /// unrecognised. Compressed-tar variants still need the extension to know
    /// they wrap a tar, so plain gz/xz/etc. are reported here as the single
    /// codec; `detect` reconciles with the extension afterwards via `from_path`
    /// only when magic yields nothing.
    fn from_magic(path: &Path) -> Option<Format> {
        use std::io::Read;
        let mut buf = [0u8; 8];
        let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
        let b = &buf[..n];
        if b.starts_with(b"7z\xBC\xAF\x27\x1C") { return Some(Format::SevenZ); }
        if b.starts_with(b"Rar!\x1A\x07") { return Some(Format::Rar); }
        if b.starts_with(b"PK\x03\x04") || b.starts_with(b"PK\x05\x06") {
            return Some(Format::from_path(path)
                .filter(|f| matches!(f, Format::Jar | Format::War | Format::Ear | Format::Apk | Format::Zipx))
                .unwrap_or(Format::Zip));
        }
        // ARJ: magic \x60\xEA
        if b.starts_with(b"\x60\xEA") { return Some(Format::Arj); }
        // ACE: bytes 2-6 = **ACE
        if b.len() >= 7 && &b[2..7] == b"**ACE" { return Some(Format::Ace); }
        None
    }

    /// Best-effort detection from a file name (extension based).
    pub fn from_path(path: &Path) -> Option<Format> {
        let name = path.file_name()?.to_string_lossy().to_lowercase();
        let two = |s: &str| name.ends_with(s);
        Some(match () {
            // Multi-part suffixes must come before single-extension checks.
            _ if two(".tar.gz")   || two(".tgz")   => Format::TarGz,
            _ if two(".tar.bz2")  || two(".tbz2")  => Format::TarBz2,
            _ if two(".tar.xz")   || two(".txz")   => Format::TarXz,
            _ if two(".tar.zst")                   => Format::TarZst,
            _ if two(".tar.lz")   || two(".tlz")   => Format::TarLz,
            _ if two(".tar.lzo")  || two(".tzo")   => Format::TarLzo,
            _ if two(".tar.lzma")                  => Format::TarLzma,
            _ if two(".tar.br")                    => Format::TarBr,
            _ if two(".tar.z")    || two(".taz")   => Format::TarZ,
            _ if two(".tar")                       => Format::Tar,
            _ => match path.extension()?.to_string_lossy().to_lowercase().as_str() {
                "zip"  => Format::Zip,
                "zipx" => Format::Zipx,
                "7z"   => Format::SevenZ,
                "rar"  => Format::Rar,
                "cbr"  => Format::Cbr,
                "ace"  => Format::Ace,
                "alz"  => Format::Alz,
                "arj"  => Format::Arj,
                "lzh" | "lha" => Format::Lzh,
                "zoo"  => Format::Zoo,
                "gz"   => Format::Gzip,
                "bz2"  => Format::Bzip2,
                "bz"   => Format::Bz,
                "xz"   => Format::Xz,
                "lzma" => Format::Lzma,
                "zst"  => Format::Zstd,
                "lz"   => Format::Lzip,
                "lzo"  => Format::Lzop,
                "br"   => Format::Brotli,
                "rz"   => Format::Rzip,
                "z"    => Format::Compress,
                "cab"  => Format::Cab,
                "ar"   => Format::Ar,
                "cpio" => Format::Cpio,
                "iso"  => Format::Iso,
                "dmg"  => Format::Dmg,
                "wim"  => Format::Wim,
                "apk"  => Format::Apk,
                "jar"  => Format::Jar,
                "war"  => Format::War,
                "ear"  => Format::Ear,
                "deb"  => Format::Deb,
                "rpm"  => Format::Rpm,
                "pkg"  => Format::Pkg,
                _ => return None,
            },
        })
    }

    /// Whether Altay can currently write this format (vs read-only).
    pub fn is_writable(self) -> bool {
        !matches!(self,
            Format::Dmg | Format::Rpm | Format::Deb | Format::Iso | Format::Wim |
            Format::Cbr | Format::Ace | Format::Alz | Format::Arj | Format::Lzh | Format::Zoo |
            Format::Rzip | Format::Cab | Format::Cpio | Format::Ar
        )
    }
}

/// One member inside an archive, as shown when "open archive as folder".
#[derive(Debug, Clone)]
pub struct Member {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error(transparent)]
    Denied(#[from] AccessDenied),
    #[error("unsupported or unrecognised archive format")]
    Unsupported,
    #[error("archive is password protected")]
    PasswordRequired,
    #[error("backend error: {0}")]
    Backend(String),
    #[error("operation cancelled")]
    Cancelled,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// An I/O error tied to a specific file (a source being read, or the
    /// archive being written) so the user can see which one failed.
    #[error("{}: {source}", path.display())]
    File { path: PathBuf, source: std::io::Error },
    /// A member that would land outside the destination, either by its name
    /// ("../x", "/etc/x") or by being written through a symbolic link.
    #[error("unsafe archive member: {}", .0.display())]
    Unsafe(PathBuf),
}

impl ArchiveError {
    /// `map_err` adapter that attaches `path` to an I/O error.
    pub fn at(path: &Path) -> impl FnOnce(std::io::Error) -> ArchiveError + '_ {
        move |source| ArchiveError::File { path: path.to_path_buf(), source }
    }

    /// The underlying I/O error and the file it concerns, if any.
    pub fn io(&self) -> Option<(&std::io::Error, Option<&Path>)> {
        match self {
            ArchiveError::Io(e) => Some((e, None)),
            ArchiveError::File { path, source } => Some((source, Some(path))),
            _ => None,
        }
    }
}

/// Options shared across create/extract operations.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub password: Option<String>,
    /// Cooperative cancellation. When present and flipped to `true`, a
    /// long-running create/extract aborts at its next checkpoint and
    /// returns [`ArchiveError::Cancelled`]. Backends that don't support
    /// mid-operation cancellation simply ignore it.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Names left out of a created archive, at any depth below the given
    /// sources (the sources themselves are always included). Honoured by
    /// the zip writer, which is what the UI creates.
    pub exclude: Exclude,
}

/// Implemented by each format backend.
pub trait Backend {
    fn list(&self, archive: &Path, opts: &Options) -> Result<Vec<Member>, ArchiveError>;

    /// List members, calling `on_progress(done, total)` after each entry.
    /// `total == 0` means the total count is unknown (show indeterminate UI).
    /// Default: calls `list()` once and reports `(total, total)` at the end.
    fn list_streamed(
        &self,
        archive: &Path,
        opts: &Options,
        on_progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<Member>, ArchiveError> {
        let members = self.list(archive, opts)?;
        on_progress(members.len(), members.len());
        Ok(members)
    }

    fn extract(
        &self,
        archive: &Path,
        into: &Path,
        opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError>;
    fn create(
        &self,
        archive: &Path,
        sources: &[PathBuf],
        opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError>;
}

/// How the bytes of an archive are organised — drives backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Random-access multi-file container (zip, jar, apk, …).
    Zip,
    /// 7-zip container.
    SevenZ,
    /// tar, optionally wrapped in a single compressor.
    Tar(Option<Codec>),
    /// A lone compressed stream of one file (`file.txt.gz`).
    Single(Codec),
    /// Recognised but no backend yet (rar, deb, rpm, iso, …).
    Unsupported,
}

/// Single-file compression codecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    Lzma,
    // Extended codecs (process-based or brotli crate)
    Lzip,
    Lzop,
    Brotli,
    Compress,
}

impl Format {
    pub fn family(self) -> Family {
        match self {
            Format::Zip | Format::Zipx | Format::Jar | Format::War | Format::Ear | Format::Apk
                => Family::Zip,
            Format::SevenZ => Family::SevenZ,
            Format::Tar     => Family::Tar(None),
            Format::TarGz   => Family::Tar(Some(Codec::Gzip)),
            Format::TarBz2  => Family::Tar(Some(Codec::Bzip2)),
            Format::TarXz   => Family::Tar(Some(Codec::Xz)),
            Format::TarZst  => Family::Tar(Some(Codec::Zstd)),
            Format::TarLz   => Family::Tar(Some(Codec::Lzip)),
            Format::TarLzo  => Family::Tar(Some(Codec::Lzop)),
            Format::TarLzma => Family::Tar(Some(Codec::Lzma)),
            Format::TarBr   => Family::Tar(Some(Codec::Brotli)),
            Format::TarZ    => Family::Tar(Some(Codec::Compress)),
            Format::Gzip    => Family::Single(Codec::Gzip),
            Format::Bzip2 | Format::Bz => Family::Single(Codec::Bzip2),
            Format::Xz      => Family::Single(Codec::Xz),
            Format::Zstd    => Family::Single(Codec::Zstd),
            Format::Lzma    => Family::Single(Codec::Lzma),
            Format::Lzip    => Family::Single(Codec::Lzip),
            Format::Lzop    => Family::Single(Codec::Lzop),
            Format::Brotli  => Family::Single(Codec::Brotli),
            Format::Compress => Family::Single(Codec::Compress),
            _ => Family::Unsupported,
        }
    }
}

/// Resolve a backend for a format.
pub fn backend_for(format: Format) -> Result<Box<dyn Backend>, ArchiveError> {
    match format {
        Format::Rar | Format::Cbr
            => return Ok(Box::new(rar_backend::RarBackend)),
        Format::Deb
            => return Ok(Box::new(deb_backend::DebBackend)),
        Format::Rpm
            => return Ok(Box::new(rpm_backend::RpmBackend)),
        Format::Ace | Format::Alz | Format::Arj | Format::Lzh | Format::Zoo
        | Format::Cab | Format::Iso | Format::Cpio | Format::Ar
            => return Ok(Box::new(cmd_backend::CmdBackend)),
        Format::Rzip
            => return Ok(Box::new(single::SinglePipedBackend {
                decompress_cmd: "rzip",
                decompress_args: &["-d", "-k", "-o", "/dev/stdout"],
                compress_cmd: "rzip",
                compress_args: &["-k", "-o"],
            })),
        _ => {}
    }
    match format.family() {
        Family::Zip    => Ok(Box::new(zip_backend::ZipBackend)),
        Family::SevenZ => Ok(Box::new(sevenz::SevenZBackend)),
        Family::Tar(codec)    => Ok(Box::new(tar_backend::TarBackend { codec })),
        Family::Single(codec) => Ok(Box::new(single::SingleBackend { codec })),
        Family::Unsupported   => Err(ArchiveError::Unsupported),
    }
}

/// A possibly-combined archive path. For split multi-volume sets
/// (`x.7z.001`, `x.zip.002`, …) the parts are concatenated into a temp file,
/// which is removed when this guard is dropped.
pub(crate) struct Combined {
    path: PathBuf,
    temp: Option<PathBuf>,
}

impl Combined {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Combined {
    fn drop(&mut self) {
        if let Some(t) = &self.temp {
            let _ = std::fs::remove_file(t);
        }
    }
}

/// If `path` is the first part of a numbered split set, concatenate all parts
/// into a temp file; otherwise pass the path through unchanged.
pub(crate) fn combine_if_split(path: &Path) -> Result<Combined, ArchiveError> {
    let Some((parts, inner_name)) = split_parts(path) else {
        return Ok(Combined { path: path.to_path_buf(), temp: None });
    };
    let tmp = std::env::temp_dir().join(format!("altay-vol-{}-{}", std::process::id(), inner_name));
    let mut out = std::fs::File::create(&tmp)?;
    for part in parts {
        let mut f = std::fs::File::open(&part)?;
        std::io::copy(&mut f, &mut out)?;
    }
    Ok(Combined { path: tmp.clone(), temp: Some(tmp) })
}

/// Detect `name.ext.001` + `name.ext.002` … Returns the ordered parts and the
/// inner file name (`name.ext`). Requires ≥2 parts so a lone `.001` is ignored.
fn split_parts(first: &Path) -> Option<(Vec<PathBuf>, String)> {
    let name = first.file_name()?.to_str()?;
    let (stem, num) = name.rsplit_once('.')?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) || num.parse::<u64>().ok()? != 1 {
        return None;
    }
    let width = num.len();
    let dir = first.parent()?;
    let mut parts = vec![first.to_path_buf()];
    let mut i = 2u64;
    loop {
        let candidate = dir.join(format!("{stem}.{i:0width$}"));
        if candidate.exists() {
            parts.push(candidate);
            i += 1;
        } else {
            break;
        }
    }
    if parts.len() >= 2 {
        Some((parts, stem.to_string()))
    } else {
        None
    }
}

/// List an archive's members after validating it, calling
/// `on_progress(done, total)` after each entry.
/// `total == 0` means total is unknown; `done == total` means complete.
/// Fraction ≈ `done as f32 / total as f32` (only when total > 0).
pub fn list_members_tracked(
    sandbox: &Sandbox,
    archive: impl AsRef<Path>,
    opts: &Options,
    on_progress: &mut dyn FnMut(usize, usize),
) -> Result<Vec<Member>, ArchiveError> {
    let safe = sandbox.resolve(archive)?;
    let combined = combine_if_split(safe.as_path())?;
    let format = Format::detect(combined.path()).ok_or(ArchiveError::Unsupported)?;
    backend_for(format)?.list_streamed(combined.path(), opts, on_progress)
}

/// Extract an archive into a destination directory, both sandbox-validated.
pub fn extract(
    sandbox: &Sandbox,
    archive: impl AsRef<Path>,
    into: impl AsRef<Path>,
    opts: &Options,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(), ArchiveError> {
    let safe = sandbox.resolve(archive)?;
    let dest = sandbox.resolve_for_create(into)?;
    std::fs::create_dir_all(dest.as_path())?;
    let combined = combine_if_split(safe.as_path())?;
    let format = Format::detect(combined.path()).ok_or(ArchiveError::Unsupported)?;
    backend_for(format)?.extract(combined.path(), dest.as_path(), opts, on_progress)
}

/// Create an archive from a set of sources. The target's extension chooses the
/// format. Sources and target are sandbox-validated.
pub fn create(
    sandbox: &Sandbox,
    archive: impl AsRef<Path>,
    sources: &[PathBuf],
    opts: &Options,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(), ArchiveError> {
    let target = sandbox.resolve_for_create(archive)?;
    let format = Format::from_path(target.as_path()).ok_or(ArchiveError::Unsupported)?;
    if !format.is_writable() {
        return Err(ArchiveError::Unsupported);
    }
    let mut validated = Vec::with_capacity(sources.len());
    for s in sources {
        validated.push(sandbox.resolve(s)?.into_path_buf());
    }
    backend_for(format)?.create(target.as_path(), &validated, opts, on_progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn workdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("altay-arc-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src/sub")).unwrap();
        fs::write(d.join("src/a.txt"), b"hello altay").unwrap();
        fs::write(d.join("src/sub/b.txt"), b"nested file").unwrap();
        d
    }

    fn roundtrip(archive_name: &str) {
        let d = workdir(archive_name);
        roundtrip_dir(&d, archive_name);
        let _ = fs::remove_dir_all(&d);
    }

    /// Archive `d/src`, list it and extract it again, checking `a.txt`.
    fn roundtrip_dir(d: &Path, archive_name: &str) {
        let archive = d.join(archive_name);
        let format = Format::from_path(&archive).unwrap();
        let backend = backend_for(format).unwrap();
        let mut noop = |_: Progress| {};

        let sources = vec![d.join("src")];
        backend.create(&archive, &sources, &Options::default(), &mut noop).unwrap();
        assert!(archive.exists(), "{archive_name}: archive not created");

        let members = backend.list(&archive, &Options::default()).unwrap();
        assert!(
            members.iter().any(|m| m.path.to_string_lossy().ends_with("a.txt")),
            "{archive_name}: a.txt missing from listing ({} members)",
            members.len()
        );

        let out = d.join("out");
        backend.extract(&archive, &out, &Options::default(), &mut noop).unwrap();
        let extracted = fs::read(out.join("src/a.txt"))
            .or_else(|_| fs::read(out.join("a.txt")))
            .expect("extracted a.txt");
        assert_eq!(extracted, b"hello altay");
    }

    #[test]
    fn zip_roundtrip() {
        roundtrip("bundle.zip");
    }

    #[test]
    fn zip_error_names_the_unreadable_file() {
        let d = workdir("zip-unreadable");
        let missing = d.join("src/gone.txt");
        let archive = d.join("bundle.zip");
        let mut noop = |_: Progress| {};
        let err = backend_for(Format::Zip)
            .unwrap()
            .create(&archive, &[missing.clone()], &Options::default(), &mut noop)
            .unwrap_err();
        let (io, path) = err.io().expect("an I/O error");
        assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(path, Some(missing.as_path()));
        assert!(err.to_string().contains("gone.txt"), "{err}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn zip_honours_exclude_patterns() {
        let d = workdir("zip-exclude");
        fs::create_dir_all(d.join("src/target/release")).unwrap();
        fs::write(d.join("src/target/release/app"), b"binary").unwrap();
        fs::write(d.join("src/sub/disk.img"), b"image").unwrap();
        let archive = d.join("bundle.zip");
        let mut noop = |_: Progress| {};
        let backend = backend_for(Format::Zip).unwrap();
        let opts = Options { exclude: Exclude::new("*.img", true), ..Default::default() };
        backend.create(&archive, &[d.join("src")], &opts, &mut noop).unwrap();
        let names: Vec<String> = backend
            .list(&archive, &Options::default())
            .unwrap()
            .iter()
            .map(|m| m.path.to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n.ends_with("a.txt")), "{names:?}");
        assert!(names.iter().any(|n| n.ends_with("sub/b.txt")), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("target") || n.ends_with(".img")), "{names:?}");

        // A source the user picked explicitly is kept even if it matches.
        let picked = d.join("picked.zip");
        backend.create(&picked, &[d.join("src/target")], &opts, &mut noop).unwrap();
        let names = backend.list(&picked, &Options::default()).unwrap();
        assert!(names.iter().any(|m| m.path.to_string_lossy().ends_with("release/app")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn sevenz_keeps_symlinks() {
        let d = workdir("7z-links");
        let archive = d.join("links.7z");
        crafted_7z(&archive, &[("src/a.txt", None), ("src/link", Some("a.txt"))]);
        let out = d.join("out");
        let mut noop = |_: Progress| {};
        backend_for(Format::SevenZ).unwrap().extract(&archive, &out, &Options::default(), &mut noop).unwrap();
        assert_eq!(fs::read_link(out.join("src/link")).unwrap(), Path::new("a.txt"));
        assert_eq!(fs::read(out.join("src/link")).unwrap(), b"pwned");
        let mtime = fs::metadata(out.join("src/a.txt")).unwrap().modified().unwrap();
        assert_eq!(mtime, std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn zip_keeps_symlinks() {
        let d = workdir("zip-links");
        std::os::unix::fs::symlink("a.txt", d.join("src/link-to-file")).unwrap();
        std::os::unix::fs::symlink("sub", d.join("src/link-to-dir")).unwrap();
        std::os::unix::fs::symlink("nowhere", d.join("src/dangling")).unwrap();
        roundtrip_dir(&d, "bundle.zip");
        let out = d.join("out/src");
        assert_eq!(fs::read_link(out.join("link-to-file")).unwrap(), Path::new("a.txt"));
        assert_eq!(fs::read_link(out.join("link-to-dir")).unwrap(), Path::new("sub"));
        assert_eq!(fs::read_link(out.join("dangling")).unwrap(), Path::new("nowhere"));
        assert_eq!(fs::read(out.join("link-to-file")).unwrap(), b"hello altay");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tar_keeps_symlinks_and_hard_links() {
        let d = workdir("tar-links");
        let archive = d.join("links.tar");
        {
            let mut b = tar::Builder::new(fs::File::create(&archive).unwrap());
            b.append_path_with_name(d.join("src/a.txt"), "a.txt").unwrap();
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            b.append_link(&mut h, "soft", "a.txt").unwrap();
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Link);
            h.set_size(0);
            b.append_link(&mut h, "hard", "a.txt").unwrap();
            b.finish().unwrap();
        }
        let out = d.join("out");
        let mut noop = |_: Progress| {};
        backend_for(Format::Tar).unwrap().extract(&archive, &out, &Options::default(), &mut noop).unwrap();
        assert_eq!(fs::read_link(out.join("soft")).unwrap(), Path::new("a.txt"));
        assert_eq!(fs::read(out.join("hard")).unwrap(), b"hello altay");
        assert!(!fs::symlink_metadata(out.join("hard")).unwrap().file_type().is_symlink());
        let _ = fs::remove_dir_all(&d);
    }

    /// A zip with `entries` written in order: (name, Some(link target)) for a
    /// symlink, (name, None) for a file containing "pwned".
    fn crafted_zip(path: &Path, entries: &[(&str, Option<&str>)]) {
        let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (name, link) in entries {
            match link {
                Some(t) => z.add_symlink(*name, *t, opts).unwrap(),
                None => {
                    z.start_file(*name, opts).unwrap();
                    std::io::Write::write_all(&mut z, b"pwned").unwrap();
                }
            }
        }
        z.finish().unwrap();
    }

    fn crafted_tar(path: &Path, entries: &[(&str, Option<&str>)]) {
        let mut b = tar::Builder::new(fs::File::create(path).unwrap());
        for (name, link) in entries {
            let mut h = tar::Header::new_gnu();
            match link {
                Some(t) => {
                    h.set_entry_type(tar::EntryType::Symlink);
                    h.set_size(0);
                    b.append_link(&mut h, name, t).unwrap();
                }
                None => {
                    h.set_size(5);
                    h.set_mode(0o644);
                    b.append_data(&mut h, name, &b"pwned"[..]).unwrap();
                }
            }
        }
        b.finish().unwrap();
    }

    /// A 7z archive the way p7zip/7-Zip store unix entries: the mode in the
    /// high half of the attributes, a link's target as its content.
    fn crafted_7z(path: &Path, entries: &[(&str, Option<&str>)]) {
        let mut w = sevenz_rust::SevenZWriter::create(path).unwrap();
        for (name, link) in entries {
            let mode: u32 = if link.is_some() { 0o120777 } else { 0o100644 };
            let mut entry = sevenz_rust::SevenZArchiveEntry::new();
            entry.name = name.to_string();
            entry.has_stream = true;
            entry.has_windows_attributes = true;
            entry.windows_attributes = 0x8000 | (mode << 16);
            entry.has_last_modified_date = true;
            entry.last_modified_date = (std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000)).try_into().unwrap();
            let content = link.unwrap_or("pwned").as_bytes();
            w.push_archive_entry(entry, Some(content)).unwrap();
        }
        w.finish().unwrap();
    }

    /// A cpio (newc) archive, extracted through the `7z` command.
    fn crafted_cpio(path: &Path, entries: &[(&str, Option<&str>)]) {
        let mut out = Vec::new();
        let trailer = [("TRAILER!!!", None)];
        for (ino, (name, link)) in entries.iter().chain(trailer.iter()).enumerate() {
            let (mode, data): (u32, &[u8]) = match link {
                Some(t) => (0o120777, t.as_bytes()),
                None if *name == "TRAILER!!!" => (0, b""),
                None => (0o100644, b"pwned"),
            };
            let fields = [ino as u32 + 1, mode, 0, 0, 1, 0, data.len() as u32, 0, 0, 0, 0, name.len() as u32 + 1, 0];
            out.extend_from_slice(b"070701");
            for f in fields {
                out.extend_from_slice(format!("{f:08X}").as_bytes());
            }
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.resize(out.len().next_multiple_of(4), 0);
            out.extend_from_slice(data);
            out.resize(out.len().next_multiple_of(4), 0);
        }
        fs::write(path, out).unwrap();
    }

    fn has_7z_command() -> bool {
        std::process::Command::new("7z").arg("i").output().is_ok()
    }

    /// Writing through, or on top of, a symlink from the same archive, or
    /// outside the destination by name, must be refused, leaving everything
    /// outside the destination untouched.
    #[test]
    fn extraction_never_writes_through_symlinks() {
        for fmt in ["zip", "tar", "7z", "cpio"] {
            if fmt == "cpio" && !has_7z_command() {
                eprintln!("skipping cpio: no 7z command");
                continue;
            }
            let d = workdir(&format!("evil-{fmt}"));
            let outside = d.join("outside");
            fs::create_dir_all(&outside).unwrap();
            let victim = d.join("victim.txt");
            fs::write(&victim, b"original").unwrap();
            let mut cases: Vec<(&str, Vec<(&str, Option<&str>)>)> = vec![
                ("dir", vec![("d", Some(outside.to_str().unwrap())), ("d/evil", None)]),
                // The zip writer refuses duplicate names, but "./x" is the
                // same member once normalised — the same trick a crafted
                // archive would use.
                ("file", vec![("x", Some(victim.to_str().unwrap())), (if fmt == "zip" { "./x" } else { "x" }, None)]),
            ];
            // The tar writer refuses `..` names itself.
            if fmt != "tar" {
                cases.push(("name", vec![("../outside/evil", None)]));
            }
            for (case, entries) in cases {
                let archive = d.join(format!("{case}.{fmt}"));
                match fmt {
                    "zip" => crafted_zip(&archive, &entries),
                    "tar" => crafted_tar(&archive, &entries),
                    "7z" => crafted_7z(&archive, &entries),
                    _ => crafted_cpio(&archive, &entries),
                }
                let out = d.join(format!("out-{case}"));
                fs::create_dir_all(&out).unwrap();
                let mut noop = |_: Progress| {};
                let format = Format::detect(&archive).unwrap();
                let err = backend_for(format).unwrap().extract(&archive, &out, &Options::default(), &mut noop).unwrap_err();
                assert!(matches!(err, ArchiveError::Unsafe(_)), "{fmt}/{case}: {err}");
            }
            assert!(fs::read_dir(&outside).unwrap().next().is_none(), "{fmt}: wrote into the outside dir");
            assert_eq!(fs::read(&victim).unwrap(), b"original", "{fmt}: overwrote the victim");
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn zip_skips_fifos() {
        let d = workdir("zip-fifo");
        let fifo = d.join("src/pipe");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        // Opening the FIFO would block forever; creation must skip it.
        roundtrip_dir(&d, "bundle.zip");
        let _ = fs::remove_dir_all(&d);
    }

    /// Over 4 GiB needs ZIP64. Sparse input, so it costs CPU (~4 GiB of
    /// deflate) but no disk; run with `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn zip_file_over_4gib() {
        let d = workdir("zip-large");
        let big = d.join("src/big.img");
        let f = fs::File::create(&big).unwrap();
        let size = u32::MAX as u64 + 4096;
        f.set_len(size).unwrap();
        drop(f);
        let archive = d.join("bundle.zip");
        let mut noop = |_: Progress| {};
        let backend = backend_for(Format::Zip).unwrap();
        backend.create(&archive, &[d.join("src")], &Options::default(), &mut noop).unwrap();
        let members = backend.list(&archive, &Options::default()).unwrap();
        let m = members.iter().find(|m| m.path.ends_with("big.img")).expect("big.img listed");
        assert_eq!(m.size, size);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn targz_roundtrip() {
        roundtrip("bundle.tar.gz");
    }

    #[test]
    fn tarzst_roundtrip() {
        roundtrip("bundle.tar.zst");
    }

    #[test]
    fn sevenz_roundtrip() {
        roundtrip("bundle.7z");
    }

    #[test]
    fn detects_format_by_magic() {
        let d = workdir("magic");
        let archive = d.join("noext");
        backend_for(Format::Zip)
            .unwrap()
            .create(&archive, &[d.join("src")], &Options::default(), &mut |_| {})
            .unwrap();
        // No extension, but magic bytes say zip.
        assert_eq!(Format::detect(&archive), Some(Format::Zip));
        let _ = fs::remove_dir_all(&d);
    }

    fn build_minimal_deb(path: &Path) {
        // data.tar.gz containing ./hello.txt
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tb = tar::Builder::new(enc);
        let data = b"hi from deb";
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tb.append_data(&mut h, "hello.txt", &data[..]).unwrap();
        let gz = tb.into_inner().unwrap().finish().unwrap();

        let mut builder = ar::Builder::new(fs::File::create(path).unwrap());
        builder
            .append(&ar::Header::new(b"debian-binary".to_vec(), 4), &b"2.0\n"[..])
            .unwrap();
        builder
            .append(&ar::Header::new(b"data.tar.gz".to_vec(), gz.len() as u64), &gz[..])
            .unwrap();
    }

    #[test]
    fn deb_list_and_extract() {
        let d = workdir("deb");
        let deb = d.join("pkg.deb");
        build_minimal_deb(&deb);
        let backend = backend_for(Format::Deb).unwrap();

        let members = backend.list(&deb, &Options::default()).unwrap();
        assert!(members.iter().any(|m| m.path.to_string_lossy().ends_with("hello.txt")));

        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        backend.extract(&deb, &out, &Options::default(), &mut |_| {}).unwrap();
        let got = fs::read(out.join("hello.txt")).expect("extracted hello.txt");
        assert_eq!(got, b"hi from deb");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn multivolume_split_zip_is_combined() {
        let d = workdir("split");
        // Make a real zip, then split its bytes into .001/.002 parts.
        let zip = d.join("bundle.zip");
        backend_for(Format::Zip)
            .unwrap()
            .create(&zip, &[d.join("src")], &Options::default(), &mut |_| {})
            .unwrap();
        let bytes = fs::read(&zip).unwrap();
        let mid = bytes.len() / 2;
        fs::write(d.join("bundle.zip.001"), &bytes[..mid]).unwrap();
        fs::write(d.join("bundle.zip.002"), &bytes[mid..]).unwrap();

        let combined = combine_if_split(&d.join("bundle.zip.001")).unwrap();
        let format = Format::detect(combined.path()).expect("combined detects as zip");
        assert_eq!(format, Format::Zip);
        let members = backend_for(format).unwrap().list(combined.path(), &Options::default()).unwrap();
        assert!(members.iter().any(|m| m.path.to_string_lossy().ends_with("a.txt")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn safe_join_blocks_traversal() {
        let dest = Path::new("/tmp/x");
        assert!(safe_join(dest, Path::new("../etc/passwd")).is_err());
        assert!(safe_join(dest, Path::new("/etc/passwd")).is_err());
        assert!(safe_join(dest, Path::new("ok/file.txt")).is_ok());
    }
}

/// Guard against path-traversal in archive member names ("Zip Slip"). Returns
/// the safe joined path, or an error if the member escapes `dest`.
pub(crate) fn safe_join(dest: &Path, member: &Path) -> Result<PathBuf, ArchiveError> {
    let mut out = dest.to_path_buf();
    for comp in member.components() {
        use std::path::Component::*;
        match comp {
            Normal(c) => out.push(c),
            CurDir => {}
            RootDir | Prefix(_) | ParentDir => return Err(ArchiveError::Unsafe(member.to_path_buf())),
        }
    }
    Ok(out)
}

// ---- Writing extracted members ----------------------------------------------
//
// Symbolic links are recreated as they are stored, but nothing is ever written
// through one or on top of one: an archive could otherwise first create
// `d -> /etc` and then a member `d/passwd` (or a link `x -> ~/.bashrc` followed
// by a file `x`) and write outside the destination. Same policy as bsdtar's
// default. Name checks alone (`safe_join`) cannot catch this, since a chain of
// links resolves differently from the path text.

/// Refuse `out_path` if any existing directory between `dest` and it is a
/// symbolic link.
fn check_ancestors(dest: &Path, out_path: &Path) -> Result<(), ArchiveError> {
    let rel = out_path.strip_prefix(dest).map_err(|_| ArchiveError::Unsafe(out_path.to_path_buf()))?;
    let mut cur = dest.to_path_buf();
    let mut comps = rel.components().peekable();
    while let Some(c) = comps.next() {
        if comps.peek().is_none() {
            break; // the member itself, checked by the caller
        }
        cur.push(c);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return Err(ArchiveError::Unsafe(out_path.to_path_buf())),
            Ok(_) => {}
            Err(_) => break, // not created yet: create_dir_all makes a real dir
        }
    }
    Ok(())
}

/// Create the parent directories of a member that is about to be written.
fn prepare_parent(dest: &Path, out_path: &Path) -> Result<(), ArchiveError> {
    check_ancestors(dest, out_path)?;
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(ArchiveError::at(parent))?;
    }
    Ok(())
}

/// Create (or truncate, for a duplicate member) a regular file. Fails rather
/// than following a symbolic link already at `out_path`.
pub(crate) fn create_member_file(dest: &Path, out_path: &Path) -> Result<std::fs::File, ArchiveError> {
    use std::os::unix::fs::OpenOptionsExt;
    prepare_parent(dest, out_path)?;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(out_path)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                ArchiveError::Unsafe(out_path.to_path_buf())
            } else {
                ArchiveError::File { path: out_path.to_path_buf(), source: e }
            }
        })
}

pub(crate) fn create_member_dir(dest: &Path, out_path: &Path) -> Result<(), ArchiveError> {
    check_ancestors(dest, out_path)?;
    if std::fs::symlink_metadata(out_path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(ArchiveError::Unsafe(out_path.to_path_buf()));
    }
    std::fs::create_dir_all(out_path).map_err(ArchiveError::at(out_path))
}

/// Copy a member's content, attributing a failure to the side it came from:
/// reading the archive (corrupt data) or writing the output (disk full, …).
pub(crate) fn copy_member(src: &mut (impl std::io::Read + ?Sized), dst: &mut std::fs::File, archive: &Path, out: &Path) -> Result<u64, ArchiveError> {
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = src.read(&mut buf).map_err(ArchiveError::at(archive))?;
        if n == 0 {
            return Ok(total);
        }
        std::io::Write::write_all(dst, &buf[..n]).map_err(ArchiveError::at(out))?;
        total += n as u64;
    }
}

/// Prepare `out_path` for a backend that writes the member itself (UnRAR):
/// the same checks as [`create_member_file`] / [`create_member_symlink`].
pub(crate) fn prepare_member_path(dest: &Path, out_path: &Path, is_symlink: bool) -> Result<(), ArchiveError> {
    prepare_parent(dest, out_path)?;
    match std::fs::symlink_metadata(out_path) {
        Ok(m) if m.file_type().is_symlink() || is_symlink => Err(ArchiveError::Unsafe(out_path.to_path_buf())),
        _ => Ok(()),
    }
}

/// Apply the rules above to an archive's listing up front, for a tool that
/// writes the members itself (the `7z` command). Each item is a member name
/// and whether it is a link (symbolic or hard). Refuses a member that escapes
/// `dest`, lies below a link member, or shares its path with one.
pub(crate) fn check_listing(dest: &Path, members: &[(PathBuf, bool)]) -> Result<(), ArchiveError> {
    let mut joined = Vec::with_capacity(members.len());
    let mut links = std::collections::HashSet::new();
    for (name, is_link) in members {
        let out = safe_join(dest, name)?;
        check_ancestors(dest, &out)?;
        if *is_link {
            links.insert(out.clone());
        }
        joined.push(out);
    }
    let mut seen = std::collections::HashSet::new();
    for out in joined {
        let below_link = out.ancestors().skip(1).take_while(|a| *a != dest).any(|a| links.contains(a));
        let shares_link = !seen.insert(out.clone()) && links.contains(&out);
        if below_link || shares_link || std::fs::symlink_metadata(&out).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(ArchiveError::Unsafe(out));
        }
    }
    Ok(())
}

/// Recreate a symbolic link member pointing at `target` (stored verbatim).
pub(crate) fn create_member_symlink(dest: &Path, out_path: &Path, target: &Path) -> Result<(), ArchiveError> {
    prepare_parent(dest, out_path)?;
    if std::fs::symlink_metadata(out_path).is_ok() {
        // Duplicate member: never replace what is already there with a link.
        return Err(ArchiveError::Unsafe(out_path.to_path_buf()));
    }
    std::os::unix::fs::symlink(target, out_path).map_err(ArchiveError::at(out_path))
}

// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Archive support. Phase 1 defines the format taxonomy and the trait every
//! backend implements; concrete backends (zip, tar family, 7z, rar, …) are
//! filled in per format in later phases. All archive paths are sandbox-checked
//! before any backend touches the disk.

use std::path::{Path, PathBuf};

use crate::filesystem::Progress;
use crate::security::{AccessDenied, Sandbox};

mod cmd_backend;
mod deb_backend;
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
    Rar, Rar5, Cbr,
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
    pub compressed_size: u64,
    pub encrypted: bool,
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
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Options shared across create/extract operations.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub password: Option<String>,
    /// Split output into volumes of this many bytes (0 = single file).
    pub volume_bytes: u64,
}

/// Implemented by each format backend.
pub trait Backend {
    fn list(&self, archive: &Path, opts: &Options) -> Result<Vec<Member>, ArchiveError>;
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
        Format::Rar | Format::Rar5 | Format::Cbr
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

/// Entry point used by the UI: list an archive's members after validating it.
pub fn list_members(
    sandbox: &Sandbox,
    archive: impl AsRef<Path>,
    opts: &Options,
) -> Result<Vec<Member>, ArchiveError> {
    let safe = sandbox.resolve(archive)?;
    let combined = combine_if_split(safe.as_path())?;
    let format = Format::detect(combined.path()).ok_or(ArchiveError::Unsupported)?;
    backend_for(format)?.list(combined.path(), opts)
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
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn zip_roundtrip() {
        roundtrip("bundle.zip");
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
            RootDir | Prefix(_) | ParentDir => {
                return Err(ArchiveError::Backend(format!(
                    "unsafe archive member path: {}",
                    member.display()
                )))
            }
        }
    }
    Ok(out)
}

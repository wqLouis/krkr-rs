//! The XP3 command family: list, inspect and extract an archive's entries.

use std::path::{Component, Path, PathBuf};

use crate::cli::{self, CommandSpec, OptionSpec};
use crate::error::ToolError;

/// The family's usage text.
const USAGE: &str = "\
XP3 archive tools:

  krkr-devtools xp3 info <archive.xp3>
  krkr-devtools xp3 list <archive.xp3> [--filter SUBSTR]
  krkr-devtools xp3 cat <archive.xp3> <name> [--lines A:B]
  krkr-devtools xp3 extract <archive.xp3> <name> -o <path>
  krkr-devtools xp3 extract-all <archive.xp3> -o <dir> [--filter SUBSTR]

An entry name is matched after normalization: the name is lowercased, a
backslash becomes a slash, runs of slashes collapse into one and leading
slashes are dropped. A name that is already normalized names the same entry.

`--filter` keeps the entries whose normalized name contains the given text,
compared without regard to case. `--lines A:B` prints only the lines A to B
of a text entry, counted from 1; the text must be UTF-8 or UTF-16.
";

/// The `info` command's contract.
const INFO: CommandSpec = CommandSpec {
    min_positionals: 1,
    max_positionals: 1,
    options: &[],
};

/// The `list` command's contract.
const LIST: CommandSpec = CommandSpec {
    min_positionals: 1,
    max_positionals: 1,
    options: &[OptionSpec {
        key: "filter",
        spellings: &["--filter"],
    }],
};

/// The `cat` command's contract.
const CAT: CommandSpec = CommandSpec {
    min_positionals: 2,
    max_positionals: 2,
    options: &[OptionSpec {
        key: "lines",
        spellings: &["--lines"],
    }],
};

/// The `extract` command's contract.
const EXTRACT: CommandSpec = CommandSpec {
    min_positionals: 2,
    max_positionals: 2,
    options: &[OptionSpec {
        key: "output",
        spellings: &["-o", "--output"],
    }],
};

/// The `extract-all` command's contract.
const EXTRACT_ALL: CommandSpec = CommandSpec {
    min_positionals: 1,
    max_positionals: 1,
    options: &[
        OptionSpec {
            key: "output",
            spellings: &["-o", "--output"],
        },
        OptionSpec {
            key: "filter",
            spellings: &["--filter"],
        },
    ],
};

/// Run the family.
pub fn run(argv: &[String]) -> Result<(), ToolError> {
    let Some(command) = argv.first() else {
        crate::out::write_line(&mut std::io::stderr().lock(), "<stderr>", USAGE)?;
        return Err(ToolError::Usage("a command is required".to_string()));
    };
    let rest = &argv[1..];
    match command.as_str() {
        "info" => info(rest),
        "list" => list(rest),
        "cat" => cat(rest),
        "extract" => extract(rest),
        "extract-all" => extract_all(rest),
        "--help" | "-h" | "help" => {
            let mut out = crate::out::Out::standard();
            out.text(USAGE)?;
            Ok(())
        }
        other => Err(ToolError::Usage(format!("unknown xp3 command `{other}`"))),
    }
}

/// An opened archive: the file-system service, the open file and the index.
struct Archive {
    filesystem: fs::FileSystem,
    file: fs::FileState,
    index: fs::xp3::Xp3Archive,
}

impl Archive {
    /// Open an archive and read its index.
    fn open(path: &Path) -> Result<Archive, ToolError> {
        let filesystem = fs::FileSystem::host(Path::new("/"));
        let file = fs::open(
            &filesystem,
            &api::fs::FsPath::host(path),
            api::fs::OpenMode::Read,
        )
        .map_err(|source| ToolError::Service {
            path: path.to_path_buf(),
            source,
        })?;
        let index = fs::xp3::Xp3Archive::open(&filesystem, &file).map_err(ToolError::Archive)?;
        Ok(Archive {
            filesystem,
            file,
            index,
        })
    }

    /// The archive's entries, ordered by their normalized name.
    fn entries(&self) -> Vec<&fs::xp3::Entry> {
        let mut entries: Vec<&fs::xp3::Entry> = self.index.entries().collect();
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
    }

    /// Read one entry's bytes.
    fn read(&self, entry: &fs::xp3::Entry) -> Result<Vec<u8>, ToolError> {
        self.index
            .read_entry(&self.filesystem, &self.file, entry)
            .map_err(ToolError::Archive)
    }
}

/// The entry the archive holds under `name`.
fn entry_of<'a>(archive: &'a Archive, name: &str) -> Result<&'a fs::xp3::Entry, ToolError> {
    archive.index.entry(name).ok_or_else(|| {
        let normalized = fs::xp3::normalize_in_archive_name(name);
        ToolError::Usage(format!("the archive has no entry named `{normalized}`"))
    })
}

/// Print the archive's shape and its entries.
fn info(argv: &[String]) -> Result<(), ToolError> {
    let invocation = cli::parse(argv, &INFO)?;
    let path = cli::positional(&invocation, 0)?;
    let archive = Archive::open(Path::new(path))?;
    let mut out = crate::out::Out::standard();
    out.line(&format!("archive: {path}"))?;
    out.line(&format!("entries: {}", archive.index.len()))?;
    for (number, section) in archive.index.index_sections().iter().enumerate() {
        let encode = match section.encode {
            fs::xp3::IndexEncode::Raw => "raw",
            fs::xp3::IndexEncode::Zlib => "zlib",
        };
        out.line(&format!(
            "index block {number}: offset={} encode={encode} file-chunks={} new-names={}",
            section.offset, section.file_chunks, section.new_names
        ))?;
    }
    out.line("")?;
    out.line(&format!(
        "{:<48} {:>12} {:>12} {:>8}",
        "name", "size", "stored", "segments"
    ))?;
    for entry in archive.entries() {
        out.line(&format!(
            "{:<48} {:>12} {:>12} {:>8}",
            entry.name,
            entry.org_size,
            entry.arc_size,
            entry.segments.len()
        ))?;
    }
    Ok(())
}

/// Print the archive's entry names.
fn list(argv: &[String]) -> Result<(), ToolError> {
    let invocation = cli::parse(argv, &LIST)?;
    let path = cli::positional(&invocation, 0)?;
    let archive = Archive::open(Path::new(path))?;
    let filter = invocation.value("filter").map(str::to_lowercase);
    let mut out = crate::out::Out::standard();
    for entry in archive.entries() {
        if let Some(filter) = &filter
            && !entry.name.to_lowercase().contains(filter.as_str())
        {
            continue;
        }
        out.line(&entry.name)?;
    }
    Ok(())
}

/// Write one entry's bytes to stdout.
fn cat(argv: &[String]) -> Result<(), ToolError> {
    let invocation = cli::parse(argv, &CAT)?;
    let path = cli::positional(&invocation, 0)?;
    let name = cli::positional(&invocation, 1)?;
    let archive = Archive::open(Path::new(path))?;
    let entry = entry_of(&archive, name)?;
    let bytes = archive.read(entry)?;
    match invocation.value("lines") {
        None => write_stdout(&bytes)?,
        Some(range) => {
            let (first, last) = parse_line_range(range)?;
            let text = decode_text(&bytes)?;
            let mut out = crate::out::Out::standard();
            for (number, line) in text.lines().enumerate() {
                let number = number + 1;
                if number >= first && number <= last {
                    out.line(&format!("{number}: {line}"))?;
                }
            }
        }
    }
    Ok(())
}

/// Write one entry's bytes to a file.
fn extract(argv: &[String]) -> Result<(), ToolError> {
    let invocation = cli::parse(argv, &EXTRACT)?;
    let path = cli::positional(&invocation, 0)?;
    let name = cli::positional(&invocation, 1)?;
    let output = invocation.value("output").ok_or_else(|| {
        ToolError::Usage("`extract` needs the output path (`-o <path>`)".to_string())
    })?;
    let archive = Archive::open(Path::new(path))?;
    let entry = entry_of(&archive, name)?;
    let bytes = archive.read(entry)?;
    let target = PathBuf::from(output);
    write_file(&target, &bytes)?;
    let mut out = crate::out::Out::standard();
    out.line(&format!("wrote {} bytes to {output}", bytes.len()))?;
    Ok(())
}

/// Write every matching entry below a directory.
fn extract_all(argv: &[String]) -> Result<(), ToolError> {
    let invocation = cli::parse(argv, &EXTRACT_ALL)?;
    let path = cli::positional(&invocation, 0)?;
    let output = invocation.value("output").ok_or_else(|| {
        ToolError::Usage("`extract-all` needs the output directory (`-o <dir>`)".to_string())
    })?;
    let archive = Archive::open(Path::new(path))?;
    let root = PathBuf::from(output);
    std::fs::create_dir_all(&root).map_err(|source| ToolError::Host {
        path: root.clone(),
        source,
    })?;
    let filter = invocation.value("filter").map(str::to_lowercase);
    let mut written = 0usize;
    for entry in archive.entries() {
        if let Some(filter) = &filter
            && !entry.name.to_lowercase().contains(filter.as_str())
        {
            continue;
        }
        let target = entry_target(&root, &entry.name)?;
        let bytes = archive.read(entry)?;
        write_file(&target, &bytes)?;
        written += 1;
    }
    let mut out = crate::out::Out::standard();
    out.line(&format!("wrote {written} entries to {output}"))?;
    Ok(())
}

/// The path of an entry below the output root.
///
/// A name that would leave the root is refused: an archive is data, and its
/// names must not decide where the tool writes.
fn entry_target(root: &Path, name: &str) -> Result<PathBuf, ToolError> {
    let mut relative = PathBuf::new();
    for component in Path::new(name).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ToolError::Usage(format!(
                    "the entry name `{name}` leaves the output directory"
                )));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(ToolError::Usage(format!(
            "the entry name `{name}` does not name a file"
        )));
    }
    Ok(root.join(relative))
}

/// Write bytes to a file, creating the parent directories.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), ToolError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| ToolError::Host {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, bytes).map_err(|source| ToolError::Host {
        path: path.to_path_buf(),
        source,
    })
}

/// Write bytes to stdout.
fn write_stdout(bytes: &[u8]) -> Result<(), ToolError> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    crate::out::write_bytes(&mut handle, "<stdout>", bytes)?;
    crate::out::flush(&mut handle, "<stdout>")
}

/// The 1-based, inclusive line range of a `--lines` argument.
fn parse_line_range(text: &str) -> Result<(usize, usize), ToolError> {
    let Some((first, last)) = text.split_once(':') else {
        return Err(ToolError::Usage(format!(
            "`--lines` needs a `A:B` range, found `{text}`"
        )));
    };
    let first: usize = first
        .parse()
        .map_err(|_| ToolError::Usage(format!("`{first}` is not a line number")))?;
    let last: usize = last
        .parse()
        .map_err(|_| ToolError::Usage(format!("`{last}` is not a line number")))?;
    if first == 0 || first > last {
        return Err(ToolError::Usage(format!(
            "the line range {first}:{last} is not a 1-based ascending range"
        )));
    }
    Ok((first, last))
}

/// The UTF-8 byte-order mark.
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
/// The UTF-16 little-endian byte-order mark.
const UTF16_LE_BOM: &[u8] = &[0xFF, 0xFE];
/// The UTF-16 big-endian byte-order mark.
const UTF16_BE_BOM: &[u8] = &[0xFE, 0xFF];

/// Decode an entry's bytes as text.
///
/// A byte-order mark selects the encoding; without one the bytes must be
/// UTF-8. Bytes that are not text in either encoding are a failure: the tool
/// does not replace characters it cannot decode.
fn decode_text(bytes: &[u8]) -> Result<String, ToolError> {
    if let Some(rest) = bytes.strip_prefix(UTF8_BOM) {
        return decode_utf8(rest);
    }
    if let Some(rest) = bytes.strip_prefix(UTF16_LE_BOM) {
        return decode_utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(UTF16_BE_BOM) {
        return decode_utf16(rest, u16::from_be_bytes);
    }
    decode_utf8(bytes)
}

/// Decode bytes as UTF-8.
fn decode_utf8(bytes: &[u8]) -> Result<String, ToolError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        ToolError::Usage(
            "the entry is not UTF-8 text; read it without `--lines` for the raw bytes".to_string(),
        )
    })
}

/// Decode bytes as UTF-16 in the given byte order.
fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Result<String, ToolError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(ToolError::Usage(
            "the entry's UTF-16 text has an odd byte count".to_string(),
        ));
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| unit(*pair))
        .collect();
    String::from_utf16(&units)
        .map_err(|_| ToolError::Usage("the entry is not valid UTF-16 text".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory for one test's files.
    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("krkr-devtools-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("the directory is created");
        directory
    }

    /// An archive holding one entry whose stored name uses a backslash.
    ///
    /// The archive image is the in-memory builder's: the magic, the index
    /// offset, a raw index block and the `info` / `segm` / `adlr` chunks.
    fn archive_with_one_entry(name: &str) -> (PathBuf, PathBuf) {
        let directory = scratch(name);
        let path = directory.join("data.xp3");
        let image = testkit::build_xp3(&[("Data\\Foo.TXT", b"hello\nworld\n", false)]);
        std::fs::write(&path, &image).expect("the archive is written");
        (directory, path)
    }

    /// A raw name and a normalized name reach the same entry.
    ///
    /// The stored name uses a backslash and an upper-case letter; the lookup
    /// normalizes the request the same way the archive normalizes the stored
    /// name, so both spellings find the entry.
    #[test]
    fn a_raw_name_and_a_normalized_name_reach_the_same_entry() {
        let (directory, path) = archive_with_one_entry("names");
        let archive = Archive::open(&path).expect("the archive opens");
        let raw = entry_of(&archive, "Data\\Foo.TXT").expect("the raw name finds the entry");
        let normalized =
            entry_of(&archive, "data/foo.txt").expect("the normalized name finds the entry");
        assert_eq!(raw.name, normalized.name);
        assert_eq!(raw.name, "data/foo.txt");
        let bytes = archive.read(raw).expect("the entry reads");
        assert_eq!(bytes.as_slice(), b"hello\nworld\n");
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A name the archive does not hold is a usage failure.
    #[test]
    fn a_name_the_archive_does_not_hold_is_refused() {
        let (directory, path) = archive_with_one_entry("missing");
        let archive = Archive::open(&path).expect("the archive opens");
        let error = entry_of(&archive, "data/absent.txt").expect_err("the name is refused");
        assert!(matches!(&error, ToolError::Usage(_)), "{error:?}");
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// The line range is one-based and inclusive.
    #[test]
    fn a_line_range_is_one_based_and_inclusive() {
        assert_eq!(parse_line_range("2:4").expect("the range parses"), (2, 4));
        assert!(parse_line_range("0:4").is_err());
        assert!(parse_line_range("4:2").is_err());
        assert!(parse_line_range("4").is_err());
    }

    /// Text is decoded from its byte-order mark, and bytes that are text in
    /// neither encoding are a failure.
    #[test]
    fn text_is_decoded_from_its_byte_order_mark() {
        assert_eq!(decode_text(b"hello").expect("the text decodes"), "hello");
        assert_eq!(
            decode_text(&[0xEF, 0xBB, 0xBF, b'h', b'i']).expect("the text decodes"),
            "hi"
        );
        let mut little = vec![0xFF, 0xFE];
        for unit in "hi".encode_utf16() {
            little.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_text(&little).expect("the text decodes"), "hi");
        let mut big = vec![0xFE, 0xFF];
        for unit in "hi".encode_utf16() {
            big.extend_from_slice(&unit.to_be_bytes());
        }
        assert_eq!(decode_text(&big).expect("the text decodes"), "hi");
        assert!(decode_text(&[0xA0, 0xA1]).is_err());
        assert!(decode_text(&[0xFF, 0xFE, 0x41]).is_err());
    }

    /// An entry name decides where the tool writes, so a name that leaves the
    /// output directory is refused.
    #[test]
    fn an_entry_name_cannot_leave_the_output_directory() {
        let root = Path::new("/tmp/krkr-devtools-out");
        assert_eq!(
            entry_target(root, "data/foo.txt").expect("the target is inside the root"),
            PathBuf::from("/tmp/krkr-devtools-out/data/foo.txt")
        );
        assert!(entry_target(root, "../escape.txt").is_err());
        assert!(entry_target(root, "data/../../escape.txt").is_err());
        assert!(entry_target(root, "/etc/passwd").is_err());
        assert!(entry_target(root, ".").is_err());
    }
}

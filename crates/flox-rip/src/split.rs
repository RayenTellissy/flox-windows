//! Splitting a file into Telegram-sized parts in place.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flox_core::error::{Error, Result};

/// Telegram's 2 GB document limit (decimal).
pub const PART_SIZE: u64 = 2_000_000_000;

/// Copy buffer, as on the Mac.
const CHUNK: usize = 8 * 1024 * 1024;

/// `media.mp4` → `media.part3.mp4`.
fn part_path(file: &Path, index: u64) -> PathBuf {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match file.extension() {
        Some(ext) => format!("{stem}.part{index}.{}", ext.to_string_lossy()),
        None => format!("{stem}.part{index}"),
    };
    file.with_file_name(name)
}

/// Copies `source[start..end]` into a new file at `to`.
fn copy_range(source: &mut File, start: u64, end: u64, to: &Path, buf: &mut [u8]) -> Result<()> {
    let mut out = File::create(to)?;
    source.seek(SeekFrom::Start(start))?;
    let mut left = end - start;
    while left > 0 {
        let want = usize::try_from(left).map_or(buf.len(), |l| l.min(buf.len()));
        let n = source.read(&mut buf[..want])?;
        if n == 0 {
            return Err(Error::Other(format!("split: {} ended early", to.display())));
        }
        out.write_all(&buf[..n])?;
        left -= n as u64;
    }
    out.flush()?;
    Ok(())
}

/// Carves parts from the end, truncating the source; never two copies on disk.
/// Returns `media.part1.mp4`… in order, or the file unchanged when it fits.
pub fn split(file: &Path, part_size: u64) -> Result<Vec<PathBuf>> {
    if part_size == 0 {
        return Err(Error::Other("split: part size is zero".to_string()));
    }
    let size = fs::metadata(file)?.len();
    if size <= part_size {
        return Ok(vec![file.to_path_buf()]);
    }
    let count = size.div_ceil(part_size);
    let mut buf = vec![0u8; CHUNK];
    {
        let mut source = OpenOptions::new().read(true).write(true).open(file)?;
        let mut end = size;
        for index in (2..=count).rev() {
            let start = (index - 1) * part_size;
            copy_range(&mut source, start, end, &part_path(file, index), &mut buf)?;
            source.set_len(start)?;
            end = start;
        }
        source.sync_all()?;
    }
    // The handle is closed first: Windows refuses to rename an open file.
    fs::rename(file, part_path(file, 1))?;
    Ok((1..=count).map(|i| part_path(file, i)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn names_parts() {
        assert_eq!(
            part_path(Path::new("/t/j/media.mp4"), 2),
            PathBuf::from("/t/j/media.part2.mp4")
        );
        assert_eq!(
            part_path(Path::new("/t/j/media"), 1),
            PathBuf::from("/t/j/media.part1")
        );
    }

    #[test]
    fn small_file_is_unchanged() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("media.mp4");
        fs::write(&file, bytes(100))?;
        assert_eq!(split(&file, 100)?, vec![file.clone()]);
        assert_eq!(fs::read(&file)?, bytes(100));
        Ok(())
    }

    #[test]
    fn splits_in_order_and_round_trips() -> Result<()> {
        for (len, part, parts) in [(101, 100, 2), (300, 100, 3), (1000, 7, 143), (5, 1, 5)] {
            let dir = tempfile::tempdir()?;
            let file = dir.path().join("media.mp4");
            let data = bytes(len);
            fs::write(&file, &data)?;
            let out = split(&file, part)?;
            assert_eq!(out.len(), parts);
            assert!(!file.exists());
            let mut joined = Vec::new();
            for (i, p) in out.iter().enumerate() {
                assert_eq!(p, &part_path(&file, i as u64 + 1));
                let chunk = fs::read(p)?;
                if i + 1 < parts {
                    assert_eq!(chunk.len() as u64, part);
                }
                joined.extend(chunk);
            }
            assert_eq!(joined, data);
            assert_eq!(fs::read_dir(dir.path())?.count(), parts);
        }
        Ok(())
    }
}

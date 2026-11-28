//! MP4 "faststart" rewrite: move the `moov` atom (the index) in front of
//! `mdat` and shift the chunk offsets accordingly. Web media pipelines
//! (WebKitGTK's GStreamer backend in particular) consume the `<video>` /
//! `<audio>` source as a non-seekable byte stream — with the index at the
//! END of the file (the default for phone exports), playback decodes the
//! first frame and then stalls waiting for data it can no longer reach.
//! Moving `moov` to the front (what `ffmpeg -movflags +faststart` does)
//! makes the file playable from a plain sequential stream. Only the box
//! layout and the chunk offsets change — the sample data is copied
//! verbatim, no re-encoding.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};

/// A `moov` box bigger than this is rejected instead of buffered.
const MAX_MOOV: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
struct TopBox {
    ty: [u8; 4],
    /// Offset of the box header.
    start: u64,
    /// Full box length, header included.
    len: u64,
}

fn read_box(f: &mut File, pos: u64, file_len: u64) -> io::Result<Option<TopBox>> {
    if pos >= file_len {
        return Ok(None);
    }
    f.seek(SeekFrom::Start(pos))?;
    let mut hdr = [0u8; 8];
    f.read_exact(&mut hdr)?;
    let size32 = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as u64;
    let ty: [u8; 4] = hdr[4..8].try_into().unwrap();
    let len = if size32 == 1 {
        let mut ls = [0u8; 8];
        f.read_exact(&mut ls)?;
        u64::from_be_bytes(ls)
    } else if size32 == 0 {
        // box extends to end of file
        file_len - pos
    } else {
        size32
    };
    if len < 8 || pos + len > file_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("truncated mp4 box at {pos} (len {len}, file {file_len})"),
        ));
    }
    Ok(Some(TopBox { ty, start: pos, len }))
}

/// Boxes whose payload holds child boxes, and where the children start
/// (offset from the box start). `meta` is a full box: its children start
/// after a 4-byte version/flags field. Everything else is a leaf.
fn child_offset(ty: &[u8; 4]) -> Option<usize> {
    match ty {
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"mvex" | b"udta" | b"ilst" => Some(8),
        b"meta" => Some(12),
        _ => None,
    }
}

/// Add `delta` to every chunk offset (`stco` 32-bit, `co64` 64-bit) in the
/// box tree stored in `data` (a region made of sibling boxes).
fn patch_offsets(data: &mut [u8], delta: i64) {
    let mut i = 0usize;
    while i + 8 <= data.len() {
        let size32 = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as i64;
        let ty: [u8; 4] = data[i + 4..i + 8].try_into().unwrap();
        let (size, wide) = if size32 == 1 {
            let size64 = i64::from_be_bytes(data[i + 8..i + 16].try_into().unwrap());
            if size64 < 16 || i + size64 as usize > data.len() {
                break;
            }
            (size64, true)
        } else if size32 == 0 {
            break;
        } else {
            (size32, false)
        };
        if size < 8 || i + size as usize > data.len() {
            break;
        }
        let end = i + size as usize;
        match &ty {
            b"stco" => {
                let body = &mut data[i + 8..end];
                if body.len() >= 8 {
                    for entry in body[8..].chunks_exact_mut(4) {
                        let v = u32::from_be_bytes(entry.try_into().unwrap()) as i64;
                        if let Some(nv) = v
                            .checked_add(delta)
                            .filter(|n| (0..=u32::MAX as i64).contains(n))
                        {
                            entry.copy_from_slice(&(nv as u32).to_be_bytes());
                        }
                    }
                }
            }
            b"co64" => {
                let body = &mut data[i + 8..end];
                if body.len() >= 8 {
                    for entry in body[8..].chunks_exact_mut(8) {
                        let v = u64::from_be_bytes(entry.try_into().unwrap());
                        if let Some(nv) = v.checked_add(delta as u64) {
                            entry.copy_from_slice(&nv.to_be_bytes());
                        }
                    }
                }
            }
            _ => {
                if let Some(off) = child_offset(&ty) {
                    let cstart = i + off + if wide { 8 } else { 0 };
                    if cstart < end {
                        patch_offsets(&mut data[cstart..end], delta);
                    }
                }
            }
        }
        i = end;
    }
}

fn copy_range(src: &mut File, dst: &mut File, start: u64, len: u64) -> io::Result<()> {
    let mut buf = vec![0u8; 1024 * 1024];
    let mut left = len;
    src.seek(SeekFrom::Start(start))?;
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let n = src.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("unexpected EOF copying mp4 box at {start}"),
            ));
        }
        dst.write_all(&buf[..n])?;
        left -= n as u64;
    }
    Ok(())
}

/// Rewrite the MP4 at `path` so that `moov` precedes `mdat` (in place, via
/// a temp file + rename). Returns the size of the moved `moov` box, or
/// `None` when nothing needs to be done (not an ISO-BMFF file, no moov or
/// mdat, or already faststart).
pub fn make_faststart(path: &Path) -> Result<Option<u64>> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let file_len = file.metadata().context("stat")?.len();
    // a valid ISO-BMFF file starts with an `ftyp` box of a sane size —
    // anything else is not our format and is left alone
    if file_len >= 8 {
        file.seek(SeekFrom::Start(0))?;
        let mut hdr = [0u8; 8];
        file.read_exact(&mut hdr)?;
        let first = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as u64;
        if &hdr[4..8] != b"ftyp" || !(8..=file_len).contains(&first) {
            return Ok(None);
        }
    } else {
        return Ok(None);
    }
    let mut boxes = Vec::new();
    let mut pos = 0u64;
    while let Some(b) = read_box(&mut file, pos, file_len)? {
        boxes.push(b);
        pos += b.len;
    }
    if boxes.first().is_none_or(|b| b.ty != *b"ftyp") {
        return Ok(None); // not an ISO-BMFF file
    }
    let moov_i = boxes.iter().position(|b| b.ty == *b"moov");
    let mdat_i = boxes.iter().position(|b| b.ty == *b"mdat");
    let (Some(moov_i), Some(mdat_i)) = (moov_i, mdat_i) else {
        return Ok(None);
    };
    if moov_i < mdat_i {
        return Ok(None); // already faststart
    }
    let moov = &boxes[moov_i];
    if moov.len > MAX_MOOV {
        bail!(
            "moov box too large to rewrite ({} MiB)",
            moov.len / 1024 / 1024
        );
    }

    // `moov` is inserted right before `mdat`, so `mdat` (and everything
    // after it) shifts forward by exactly the moov box's size — which is
    // also the shift every sample-data chunk offset needs.
    let delta = moov.len as i64;

    file.seek(SeekFrom::Start(moov.start))?;
    let mut moov_bytes = vec![0u8; moov.len as usize];
    file.read_exact(&mut moov_bytes)?;
    patch_offsets(&mut moov_bytes[8..], delta);

    let tmp = path.with_extension("fs.tmp");
    {
        let mut out = File::create(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        for (i, b) in boxes.iter().enumerate() {
            if i == moov_i {
                continue; // written right before mdat
            }
            if i == mdat_i {
                out.write_all(&moov_bytes)?;
            }
            copy_range(&mut file, &mut out, b.start, b.len)?;
        }
        out.flush()?;
        out.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(Some(moov.len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(payload);
        v
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "sn-faststart-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// ftyp + mdat + moov (moov LAST) with one stco entry pointing at the
    /// start of the mdat payload.
    fn build_nonfaststart(mdat_payload: &[u8]) -> (Vec<u8>, u64) {
        let ftyp = box_(b"ftyp", b"isom\x02\x00\x00\x00isomiso2");
        let mdat_payload_start = ftyp.len() as u64 + 8;
        let mut stco = vec![0u8; 12];
        stco[4..8].copy_from_slice(&1u32.to_be_bytes()); // one entry
        stco[8..12]
            .copy_from_slice(&(mdat_payload_start as u32).to_be_bytes()); // points at mdat payload
        let stco_box = box_(b"stco", &stco);
        let moov = box_(
            b"moov",
            &box_(b"trak", &box_(b"mdia", &box_(b"minf", &box_(b"stbl", &stco_box)))),
        );
        let mdat = box_(b"mdat", mdat_payload);
        let mut file = Vec::new();
        file.extend_from_slice(&ftyp);
        file.extend_from_slice(&mdat);
        file.extend_from_slice(&moov);
        (file, mdat_payload_start)
    }

    #[test]
    fn faststart_moves_moov_and_fixes_offsets() {
        let d = tmpdir("move");
        let f = d.join("v.mp4");
        let payload = b"0123456789abcdef";
        let (file, old_mdat_start) = build_nonfaststart(payload);
        std::fs::write(&f, &file).unwrap();
        let ftyp_len = 8 + 16;
        let mdat_len = 8 + payload.len() as u64;
        let moov_len = file.len() as u64 - ftyp_len - mdat_len;

        let moved = make_faststart(&f).unwrap();
        assert_eq!(moved, Some(moov_len));

        let data = std::fs::read(&f).unwrap();
        assert_eq!(data.len(), file.len());
        // ftyp, then moov, then mdat
        assert_eq!(&data[4..8], b"ftyp");
        let moov_off = ftyp_len as usize;
        assert_eq!(&data[moov_off + 4..moov_off + 8], b"moov");
        let mdat_off = moov_off + moov_len as usize;
        assert_eq!(&data[mdat_off + 4..mdat_off + 8], b"mdat");
        assert_eq!(&data[mdat_off + 8..], payload);
        // the stco entry now points at the MOVED mdat payload
        let moov_bytes = &data[moov_off..moov_off + moov_len as usize];
        let stco_i = moov_bytes.windows(4).position(|w| w == b"stco").unwrap();
        let off = u32::from_be_bytes(moov_bytes[stco_i + 12..stco_i + 16].try_into().unwrap())
            as u64;
        assert_eq!(off, mdat_off as u64 + 8);
        assert_eq!(off, old_mdat_start + moov_len);
        // a second run is a no-op
        assert_eq!(make_faststart(&f).unwrap(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn faststart_noop_when_moov_already_first() {
        let d = tmpdir("noop");
        let f = d.join("v.mp4");
        let (mut file, _) = build_nonfaststart(b"xyz");
        // reorder: ftyp, moov, mdat
        let ftyp_len: usize = 8 + 16;
        let mdat_len: usize = 8 + 3;
        let moov_len = file.len() - ftyp_len - mdat_len;
        let moov_start = file.len() - moov_len;
        let moov = file[moov_start..].to_vec();
        let mdat = file[ftyp_len..moov_start].to_vec();
        file = [
            file[..ftyp_len].to_vec(),
            moov,
            mdat,
        ]
        .concat();
        std::fs::write(&f, &file).unwrap();
        assert_eq!(make_faststart(&f).unwrap(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn faststart_ignores_non_mp4() {
        let d = tmpdir("nomp4");
        let f = d.join("a.bin");
        std::fs::write(&f, b"this is not an mp4 file at all").unwrap();
        assert_eq!(make_faststart(&f).unwrap(), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}

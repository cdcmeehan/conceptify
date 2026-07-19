//! Video-asset ingestion domain (epic conceptify-z9y, artifact-spec §1.4/§8.3).
//!
//! Owns the `save-asset` pipeline behind
//! `PUT /api/v1/threads/:thread_id/assets/:sha256`: content-address
//! verification (the server re-hashes the body — `E-ASSET-HASH`), the §1.4
//! encoding-budget enforcement via a hand-rolled ISO-BMFF box walker
//! (`E-ASSET-TYPE` / `E-ASSET-DURATION`, warnings `W-ASSET-RES` /
//! `W-ASSET-LONG`), and immutable content-addressed storage at
//! `<artifacts-root>/<project-id>/threads/<slug>/assets/<sha256>.mp4`.
//!
//! Rule IDs are the stable identifiers reserved in docs/artifact-spec.md §8.3
//! — that doc is the contract.
//!
//! Storage properties (per the z9y.1 decision):
//! - **temp + rename** (N4): a crash mid-write never leaves a partial file at
//!   the final name (`artifacts::atomic_write`, same discipline as artifact
//!   saves).
//! - **Immutable + idempotent**: the name IS the SHA-256 of the content, so a
//!   re-upload of already-stored bytes is a no-op `200` (the file is not
//!   rewritten). Cross-version dedup is free for the same reason.
//! - **GC is free**: `assets/` sits inside the per-thread artifact directory
//!   that thread deletion already removes recursively. There is deliberately
//!   NO supersede-time GC — old artifact versions must keep rendering.
//!
//! The MP4 sniffing is a minimal hand-rolled box walker, not a media crate:
//! we only need `ftyp` (container sanity), `moov/mvhd` (duration),
//! `trak/mdia/hdlr` + `stbl/stsd` (codec: `avc1` H.264 within High ≤ L4.0,
//! 8-bit 4:2:0; audio `mp4a`/AAC-LC ≤ 2ch), and the visual sample entry's
//! width/height. Anything unparseable is by definition not a valid MP4
//! (`E-ASSET-TYPE`), per §8.3.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::artifacts::{self, Issue, Validation};

/// Hard cap: assets above this are rejected (`E-ASSET-SIZE`, spec §8.3).
pub const MAX_ASSET_BYTES: usize = 20_971_520; // 20 MiB
/// Hard cap: clips longer than this are rejected (`E-ASSET-DURATION`).
pub const MAX_DURATION_SECS: f64 = 120.0;
/// Soft cap: clips longer than this warn (`W-ASSET-LONG`).
pub const WARN_DURATION_SECS: f64 = 90.0;
/// Soft cap: resolutions beyond 1280×720 warn (`W-ASSET-RES`).
pub const WARN_MAX_WIDTH: u16 = 1280;
pub const WARN_MAX_HEIGHT: u16 = 720;

/// `<root>/<project-id>/threads/<slug>/assets` — the per-thread asset dir.
/// Deliberately INSIDE [`artifacts::thread_dir`] so thread deletion's
/// recursive directory removal garbage-collects assets with zero extra code.
pub fn assets_dir(root: &Path, project_id: &str, slug: &str) -> PathBuf {
    artifacts::thread_dir(root, project_id, slug).join("assets")
}

/// `<root>/<project-id>/threads/<slug>/assets/<sha256>.mp4` — the immutable
/// content-addressed asset file. Must stay in lockstep with the URL grammar
/// (`cfy-asset://localhost/<thread-id>/<sha256>.mp4`) served by
/// `crate::asset_protocol`; the protocol tests pin the two together by saving
/// through [`save_asset`] and reading back through this helper.
pub fn asset_file_path(root: &Path, project_id: &str, slug: &str, sha256: &str) -> PathBuf {
    assets_dir(root, project_id, slug).join(format!("{sha256}.mp4"))
}

/// `true` iff `s` is a well-formed content address: exactly 64 lowercase hex
/// characters (the §1.4 grammar — uppercase is rejected on purpose so every
/// asset has exactly one canonical URL/path).
pub fn is_valid_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Lowercase-hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The canonical in-artifact reference for a stored asset (docs/api.md).
pub fn asset_url(thread_id: &str, sha256: &str) -> String {
    format!("cfy-asset://localhost/{thread_id}/{sha256}.mp4")
}

/// Result of a successful upload.
#[derive(Debug)]
pub struct SavedAsset {
    pub thread_id: String,
    /// Not surfaced in the HTTP response (docs/api.md shape); kept for the
    /// storage-invariant tests below.
    #[cfg_attr(not(test), allow(dead_code))]
    pub project_id: String,
    pub sha256: String,
    pub bytes: u64,
    pub url: String,
    /// `true` when the content-addressed file already existed (idempotent
    /// re-upload — nothing was rewritten). Same 200 either way; tests pin
    /// the no-rewrite behavior through this.
    #[cfg_attr(not(test), allow(dead_code))]
    pub already_existed: bool,
    pub warnings: Vec<Issue>,
}

/// Errors from the upload pipeline. Variants map to HTTP statuses in
/// `server::assets_routes` (mirrors `artifacts::ArtifactError`).
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("thread not found: {0}")]
    ThreadNotFound(String),

    /// §8.3 hard failures — nothing was stored.
    #[error("asset rejected: {}", .0.first().map(|i| i.code).unwrap_or("E-?"))]
    Rejected(Vec<Issue>),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

// ---------------------------------------------------------------------------
// The upload pipeline
// ---------------------------------------------------------------------------

/// Validate and store `bytes` as `<sha256>.mp4` in `thread_id`'s asset dir.
///
/// Designed to run inside the shared connection lock like `save_artifact`
/// (one thread lookup, no writes to the DB — assets have no rows; the file
/// system is the source of truth, exactly what the `E-ASSET-REF` existence
/// check and the `cfy-asset://` protocol handler consult).
pub fn save_asset(
    conn: &Connection,
    root: &Path,
    thread_id: &str,
    url_sha256: &str,
    bytes: &[u8],
) -> Result<SavedAsset, AssetError> {
    let row = conn
        .query_row(
            "SELECT project_id, slug FROM threads WHERE id = ?1",
            [thread_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((project_id, slug)) = row else {
        return Err(AssetError::ThreadNotFound(thread_id.to_owned()));
    };

    let validation = validate_asset(url_sha256, bytes);
    if !validation.errors.is_empty() {
        return Err(AssetError::Rejected(validation.errors));
    }

    let file = asset_file_path(root, &project_id, &slug, url_sha256);
    let already_existed = file.is_file();
    if !already_existed {
        fs::create_dir_all(assets_dir(root, &project_id, &slug))?;
        artifacts::atomic_write(&file, bytes)?;
    }
    // Content-addressed: if the file exists, its bytes ARE these bytes (the
    // hash check above proved it), so rewriting would be pure churn.

    Ok(SavedAsset {
        thread_id: thread_id.to_owned(),
        project_id,
        sha256: url_sha256.to_owned(),
        bytes: bytes.len() as u64,
        url: asset_url(thread_id, url_sha256),
        already_existed,
        warnings: validation.warnings,
    })
}

/// Run the §8.3 rule set against an uploaded body. Pure function.
///
/// Ordering: an over-cap body short-circuits (don't hash/sniff 20+ MiB of
/// junk, mirroring `E-SIZE-MAX`); below the cap, hash and sniff issues are
/// collected together so the agent sees everything wrong at once.
pub fn validate_asset(url_sha256: &str, bytes: &[u8]) -> Validation {
    let mut v = Validation::default();

    if bytes.len() > MAX_ASSET_BYTES {
        v.errors.push(Issue::new(
            "E-ASSET-SIZE",
            format!(
                "asset is {} bytes, over the 20 MiB hard cap ({MAX_ASSET_BYTES} bytes)",
                bytes.len()
            ),
        ));
        return v;
    }

    if !is_valid_sha256(url_sha256) {
        // A malformed URL sha can never equal any body hash — same rule.
        v.errors.push(Issue::new(
            "E-ASSET-HASH",
            format!(
                "URL sha256 segment \"{url_sha256}\" is not 64 lowercase hex characters"
            ),
        ));
    } else {
        let actual = sha256_hex(bytes);
        if actual != url_sha256 {
            v.errors.push(Issue::new(
                "E-ASSET-HASH",
                format!(
                    "body hashes to {actual} but the URL names {url_sha256} \
                     (the URL sha must be the SHA-256 of the exact bytes uploaded)"
                ),
            ));
        }
    }

    match sniff_mp4(bytes) {
        Err(reason) => v.errors.push(Issue::new(
            "E-ASSET-TYPE",
            format!("not a valid MP4/H.264 asset per artifact-spec §1.4: {reason}"),
        )),
        Ok(info) => {
            if info.duration_secs > MAX_DURATION_SECS {
                v.errors.push(Issue::new(
                    "E-ASSET-DURATION",
                    format!(
                        "duration is {:.1}s, over the 120s hard cap",
                        info.duration_secs
                    ),
                ));
            } else if info.duration_secs > WARN_DURATION_SECS {
                v.warnings.push(Issue::new(
                    "W-ASSET-LONG",
                    format!(
                        "duration is {:.1}s, over the 90s advisory cap (aim for 30–90s)",
                        info.duration_secs
                    ),
                ));
            }
            if info.width > WARN_MAX_WIDTH || info.height > WARN_MAX_HEIGHT {
                v.warnings.push(Issue::new(
                    "W-ASSET-RES",
                    format!(
                        "resolution is {}×{}, over the 1280×720 advisory cap",
                        info.width, info.height
                    ),
                ));
            }
        }
    }

    v
}

// ---------------------------------------------------------------------------
// ISO-BMFF sniffing (minimal, purpose-built — see module docs)
// ---------------------------------------------------------------------------

/// Facts extracted from a syntactically valid, budget-checkable MP4.
#[derive(Debug, PartialEq)]
pub struct Mp4Info {
    pub duration_secs: f64,
    pub width: u16,
    pub height: u16,
}

/// One parsed box: its 4CC and the content slice (header excluded).
struct Mp4Box<'a> {
    kind: [u8; 4],
    content: &'a [u8],
}

/// Iterate the boxes laid out back-to-back in `data` (a whole file or a
/// container box's content). Malformed sizes end iteration by yielding an
/// error the caller surfaces as `E-ASSET-TYPE`.
fn boxes(data: &[u8]) -> impl Iterator<Item = Result<Mp4Box<'_>, &'static str>> {
    let mut off = 0usize;
    let mut dead = false;
    std::iter::from_fn(move || {
        if dead || off >= data.len() {
            return None;
        }
        if data.len() - off < 8 {
            dead = true;
            return Some(Err("truncated box header"));
        }
        let size32 = u32::from_be_bytes(data[off..off + 4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = data[off + 4..off + 8].try_into().unwrap();
        let (header_len, total) = match size32 {
            0 => (8usize, (data.len() - off) as u64), // box extends to EOF
            1 => {
                if data.len() - off < 16 {
                    dead = true;
                    return Some(Err("truncated 64-bit box header"));
                }
                let large = u64::from_be_bytes(data[off + 8..off + 16].try_into().unwrap());
                (16usize, large)
            }
            n => (8usize, n),
        };
        if total < header_len as u64 || total > (data.len() - off) as u64 {
            dead = true;
            return Some(Err("box size exceeds available data"));
        }
        let content = &data[off + header_len..off + total as usize];
        off += total as usize;
        Some(Ok(Mp4Box { kind, content }))
    })
}

/// The content of the first direct child box of `data` with 4CC `kind`.
fn find_box<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<Option<&'a [u8]>, &'static str> {
    for b in boxes(data) {
        let b = b?;
        if &b.kind == kind {
            return Ok(Some(b.content));
        }
    }
    Ok(None)
}

fn required<'a>(
    data: &'a [u8],
    kind: &[u8; 4],
    what: &'static str,
) -> Result<&'a [u8], String> {
    find_box(data, kind)
        .map_err(|e| format!("{what}: {e}"))?
        .ok_or_else(|| format!("missing {what} box"))
}

fn be_u16(data: &[u8], off: usize) -> Result<u16, &'static str> {
    data.get(off..off + 2)
        .map(|s| u16::from_be_bytes(s.try_into().unwrap()))
        .ok_or("truncated field")
}

fn be_u32(data: &[u8], off: usize) -> Result<u32, &'static str> {
    data.get(off..off + 4)
        .map(|s| u32::from_be_bytes(s.try_into().unwrap()))
        .ok_or("truncated field")
}

fn be_u64(data: &[u8], off: usize) -> Result<u64, &'static str> {
    data.get(off..off + 8)
        .map(|s| u64::from_be_bytes(s.try_into().unwrap()))
        .ok_or("truncated field")
}

/// Sniff `bytes` as an MP4 and enforce the §1.4 codec allowlist. `Err` is the
/// human-readable reason, reported under `E-ASSET-TYPE`.
pub fn sniff_mp4(bytes: &[u8]) -> Result<Mp4Info, String> {
    // Container sanity: the very first box must be `ftyp` (ISO BMFF). This is
    // the "magic number" check — nothing else (WebM, MKV, raw H.264, HTML…)
    // starts this way.
    let first = boxes(bytes)
        .next()
        .ok_or("empty file")?
        .map_err(|e| format!("unparseable leading box: {e}"))?;
    if &first.kind != b"ftyp" {
        return Err(format!(
            "first box is \"{}\", expected ftyp (not an ISO-BMFF/MP4 file)",
            String::from_utf8_lossy(&first.kind)
        ));
    }

    let moov = required(bytes, b"moov", "moov")?;
    let mvhd = required(moov, b"mvhd", "mvhd")?;
    let duration_secs = parse_mvhd_duration(mvhd).map_err(|e| format!("mvhd: {e}"))?;

    let mut video: Option<(u16, u16)> = None;
    for b in boxes(moov) {
        let b = b.map_err(|e| format!("moov children: {e}"))?;
        if &b.kind != b"trak" {
            continue;
        }
        let mdia = required(b.content, b"mdia", "trak/mdia")?;
        let hdlr = required(mdia, b"hdlr", "trak/mdia/hdlr")?;
        // hdlr content: version/flags (4), pre_defined (4), handler_type (4).
        let handler: [u8; 4] = hdlr
            .get(8..12)
            .ok_or("truncated hdlr")?
            .try_into()
            .unwrap();
        let minf = required(mdia, b"minf", "trak/mdia/minf")?;
        let stbl = required(minf, b"stbl", "trak/mdia/minf/stbl")?;
        let stsd = required(stbl, b"stsd", "trak/…/stsd")?;
        match &handler {
            b"vide" => {
                let (w, h) = check_video_sample_entry(stsd)?;
                // First video track wins for the resolution warning; every
                // video track must individually pass the codec allowlist.
                video.get_or_insert((w, h));
            }
            b"soun" => check_audio_sample_entry(stsd)?,
            // Other handlers (timed text, metadata, chapters…) are tolerated:
            // §1.4 constrains video and audio; extra tracks are harmless.
            _ => {}
        }
    }

    let Some((width, height)) = video else {
        return Err("no video track (hdlr \"vide\") found".into());
    };

    Ok(Mp4Info {
        duration_secs,
        width,
        height,
    })
}

/// `mvhd` → duration in seconds (version 0 and 1 layouts).
fn parse_mvhd_duration(mvhd: &[u8]) -> Result<f64, &'static str> {
    let version = *mvhd.first().ok_or("empty mvhd")?;
    let (timescale, duration) = match version {
        // version/flags(4) creation(4) modification(4) timescale(4) duration(4)
        0 => (be_u32(mvhd, 12)?, be_u32(mvhd, 16)? as u64),
        // version/flags(4) creation(8) modification(8) timescale(4) duration(8)
        1 => (be_u32(mvhd, 20)?, be_u64(mvhd, 24)?),
        _ => return Err("unknown mvhd version"),
    };
    if timescale == 0 {
        return Err("timescale is zero");
    }
    Ok(duration as f64 / timescale as f64)
}

/// H.264 profile_idc values admitted by §1.4 ("High profile or lower"):
/// Baseline (66), Main (77), Extended (88), High (100).
const ALLOWED_AVC_PROFILES: [u8; 4] = [66, 77, 88, 100];
/// §1.4: level ≤ 4.0 (AVCLevelIndication is level × 10).
const MAX_AVC_LEVEL: u8 = 40;

/// Enforce §1.4 on a video `stsd`: exactly the `avc1` sample entry, profile ≤
/// High, level ≤ 4.0, and (when the avcC High-profile extension is present)
/// 8-bit 4:2:0. Returns (width, height) from the visual sample entry.
fn check_video_sample_entry(stsd: &[u8]) -> Result<(u16, u16), String> {
    // stsd content: version/flags(4) entry_count(4), then sample entries laid
    // out as ordinary boxes.
    let entries = stsd.get(8..).ok_or("truncated stsd")?;
    let entry = boxes(entries)
        .next()
        .ok_or("stsd has no sample entries")?
        .map_err(|e| format!("stsd entry: {e}"))?;
    if &entry.kind != b"avc1" {
        return Err(format!(
            "video codec is \"{}\", only avc1 (H.264) is permitted — no WebM/HEVC/AV1",
            String::from_utf8_lossy(&entry.kind)
        ));
    }
    // VisualSampleEntry content layout (offsets exclude the 8-byte box
    // header): reserved(6) dref_index(2) pre_defined(2) reserved(2)
    // pre_defined(12) width(2) height(2) horiz/vert-resolution(8) reserved(4)
    // frame_count(2) compressorname(32) depth(2) pre_defined(2) — 78 fixed
    // bytes, then child boxes (avcC among them).
    let width = be_u16(entry.content, 24).map_err(|e| format!("visual entry width: {e}"))?;
    let height = be_u16(entry.content, 26).map_err(|e| format!("visual entry height: {e}"))?;
    let children = entry
        .content
        .get(78..)
        .ok_or("truncated visual sample entry")?;
    let avcc = find_box(children, b"avcC")
        .map_err(|e| format!("avc1 children: {e}"))?
        .ok_or("avc1 entry has no avcC configuration box")?;
    check_avcc(avcc)?;
    Ok((width, height))
}

/// AVCDecoderConfigurationRecord checks: profile/level from the fixed header;
/// chroma/bit-depth from the High-profile extension when present. The
/// extension is only defined for profiles 100/110/122/144 and some muxers
/// omit it even then — absence is tolerated (the profile/level gate already
/// excludes the 10-bit/4:2:2 profile families, so an omitted extension on a
/// permitted profile cannot smuggle in a disallowed format).
fn check_avcc(avcc: &[u8]) -> Result<(), String> {
    let profile = *avcc.get(1).ok_or("truncated avcC")?;
    let level = *avcc.get(3).ok_or("truncated avcC")?;
    if !ALLOWED_AVC_PROFILES.contains(&profile) {
        return Err(format!(
            "H.264 profile_idc {profile} is not permitted (allowed: Baseline 66, \
             Main 77, Extended 88, High 100)"
        ));
    }
    if level > MAX_AVC_LEVEL {
        return Err(format!(
            "H.264 level {}.{} exceeds the 4.0 cap",
            level / 10,
            level % 10
        ));
    }

    if matches!(profile, 100 | 110 | 122 | 144) {
        // Walk past the SPS/PPS arrays to the extension bytes.
        let num_sps = avcc.get(5).map(|b| b & 0x1f).unwrap_or(0) as usize;
        let mut off = 6usize;
        for _ in 0..num_sps {
            let Ok(len) = be_u16(avcc, off) else { return Ok(()) };
            off += 2 + len as usize;
        }
        let Some(&num_pps) = avcc.get(off) else { return Ok(()) };
        off += 1;
        for _ in 0..num_pps {
            let Ok(len) = be_u16(avcc, off) else { return Ok(()) };
            off += 2 + len as usize;
        }
        if let Some(ext) = avcc.get(off..off + 3) {
            let chroma_format = ext[0] & 0x03;
            let bit_depth_luma = (ext[1] & 0x07) + 8;
            let bit_depth_chroma = (ext[2] & 0x07) + 8;
            if chroma_format != 1 {
                return Err(format!(
                    "chroma format {chroma_format} is not 4:2:0 (yuv420p required)"
                ));
            }
            if bit_depth_luma != 8 || bit_depth_chroma != 8 {
                return Err(format!(
                    "bit depth {bit_depth_luma}/{bit_depth_chroma} is not 8-bit"
                ));
            }
        }
    }
    Ok(())
}

/// Enforce §1.4 on an audio `stsd`: `mp4a` (MPEG-4 audio), ≤ 2 channels, and
/// — best-effort, when the esds descriptors parse — AAC-LC specifically.
fn check_audio_sample_entry(stsd: &[u8]) -> Result<(), String> {
    let entries = stsd.get(8..).ok_or("truncated audio stsd")?;
    let entry = boxes(entries)
        .next()
        .ok_or("audio stsd has no sample entries")?
        .map_err(|e| format!("audio stsd entry: {e}"))?;
    if &entry.kind != b"mp4a" {
        return Err(format!(
            "audio codec is \"{}\", only mp4a (AAC-LC) is permitted",
            String::from_utf8_lossy(&entry.kind)
        ));
    }
    // AudioSampleEntry content: reserved(6) dref_index(2) reserved(8)
    // channelcount(2) samplesize(2) pre_defined(2) reserved(2) samplerate(4),
    // then child boxes (esds) at 28.
    let channels = be_u16(entry.content, 16).map_err(|e| format!("audio entry: {e}"))?;
    if channels > 2 {
        return Err(format!("audio has {channels} channels, cap is 2"));
    }
    if let Ok(Some(esds)) = find_box(entry.content.get(28..).unwrap_or(&[]), b"esds") {
        if let Some((object_type, audio_object_type)) = parse_esds(esds) {
            // objectTypeIndication 0x40 = MPEG-4 Audio; AOT 2 = AAC-LC.
            if object_type != 0x40 || audio_object_type.is_some_and(|aot| aot != 2) {
                return Err(format!(
                    "audio is not AAC-LC (objectType 0x{object_type:02x}, AOT {audio_object_type:?})"
                ));
            }
        }
        // Unparseable descriptors: tolerated — mp4a + channel cap already
        // held, and rejecting on descriptor quirks would fail legit files.
    }
    Ok(())
}

/// Best-effort esds walk → (objectTypeIndication, audioObjectType). MPEG-4
/// descriptors use a tag byte + a base-128 continuation-bit length.
fn parse_esds(esds: &[u8]) -> Option<(u8, Option<u8>)> {
    fn read_descriptor(data: &[u8], expect_tag: u8) -> Option<&[u8]> {
        let (&tag, mut rest) = data.split_first()?;
        if tag != expect_tag {
            return None;
        }
        let mut len = 0usize;
        for _ in 0..4 {
            let (&b, r) = rest.split_first()?;
            rest = r;
            len = (len << 7) | (b & 0x7f) as usize;
            if b & 0x80 == 0 {
                break;
            }
        }
        rest.get(..len)
    }

    // esds content: version/flags(4), then ES_Descriptor (tag 0x03).
    let es = read_descriptor(esds.get(4..)?, 0x03)?;
    // ES_Descriptor: ES_ID(2), flags(1) [assuming no optional fields — ffmpeg
    // and every sane muxer], then DecoderConfigDescriptor (tag 0x04).
    let dcd = read_descriptor(es.get(3..)?, 0x04)?;
    let object_type = *dcd.first()?;
    // DecoderConfigDescriptor: objectType(1), streamType/bufferSize(4),
    // maxBitrate(4), avgBitrate(4), then DecoderSpecificInfo (tag 0x05).
    let aot = read_descriptor(dcd.get(13..)?, 0x05)
        .and_then(|dsi| dsi.first())
        .map(|b| b >> 3); // first 5 bits = audioObjectType
    Some((object_type, aot))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A real ffmpeg-produced clip: 2s, 64×36, H.264 High L3.0 yuv420p,
    /// faststart, no audio (~4 KiB).
    pub(crate) const TINY_MP4: &[u8] = include_bytes!("../tests/fixtures/tiny-avc1.mp4");
    /// Same but Main profile with a mono AAC-LC track (~11 KiB).
    pub(crate) const TINY_MP4_AAC: &[u8] =
        include_bytes!("../tests/fixtures/tiny-avc1-aac.mp4");

    // -- synthetic MP4 builder (full structural control for negative cases) --

    fn boxed(kind: &[u8; 4], content: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + content.len());
        out.extend_from_slice(&((8 + content.len()) as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(content);
        out
    }

    fn mvhd_v0(timescale: u32, duration: u32) -> Vec<u8> {
        let mut c = vec![0u8; 100]; // v0 mvhd is 100 content bytes
        c[12..16].copy_from_slice(&timescale.to_be_bytes());
        c[16..20].copy_from_slice(&duration.to_be_bytes());
        boxed(b"mvhd", &c)
    }

    fn avcc(profile: u8, level: u8) -> Vec<u8> {
        // Minimal avcC: version, profile, compat, level, lengthSize, 0 SPS,
        // 0 PPS (no High-profile extension bytes — tolerated by design).
        boxed(b"avcC", &[1, profile, 0, level, 0xff, 0xe0, 0])
    }

    fn visual_entry(format: &[u8; 4], width: u16, height: u16, avcc_box: &[u8]) -> Vec<u8> {
        let mut c = vec![0u8; 78];
        c[24..26].copy_from_slice(&width.to_be_bytes());
        c[26..28].copy_from_slice(&height.to_be_bytes());
        c.extend_from_slice(avcc_box);
        boxed(format, &c)
    }

    fn stsd(entry: &[u8]) -> Vec<u8> {
        let mut c = vec![0, 0, 0, 0, 0, 0, 0, 1]; // version/flags + count 1
        c.extend_from_slice(entry);
        boxed(b"stsd", &c)
    }

    fn hdlr(handler: &[u8; 4]) -> Vec<u8> {
        let mut c = vec![0u8; 8];
        c.extend_from_slice(handler);
        c.extend_from_slice(&[0u8; 13]); // reserved + empty name
        boxed(b"hdlr", &c)
    }

    fn trak(handler: &[u8; 4], stsd_box: &[u8]) -> Vec<u8> {
        let stbl = boxed(b"stbl", stsd_box);
        let minf = boxed(b"minf", &stbl);
        let mut mdia_content = hdlr(handler);
        mdia_content.extend_from_slice(&minf);
        let mdia = boxed(b"mdia", &mdia_content);
        boxed(b"trak", &mdia)
    }

    fn audio_entry(format: &[u8; 4], channels: u16) -> Vec<u8> {
        let mut c = vec![0u8; 28];
        c[16..18].copy_from_slice(&channels.to_be_bytes());
        boxed(format, &c)
    }

    /// A structurally valid synthetic MP4 with tunable knobs.
    fn synthetic(
        duration_secs: u32,
        width: u16,
        height: u16,
        profile: u8,
        level: u8,
    ) -> Vec<u8> {
        let entry = visual_entry(b"avc1", width, height, &avcc(profile, level));
        let mut moov_content = mvhd_v0(1000, duration_secs * 1000);
        moov_content.extend_from_slice(&trak(b"vide", &stsd(&entry)));
        let mut out = boxed(b"ftyp", b"isom\0\0\0\0isomavc1");
        out.extend_from_slice(&boxed(b"moov", &moov_content));
        out.extend_from_slice(&boxed(b"mdat", &[0u8; 32]));
        out
    }

    fn ok_synthetic() -> Vec<u8> {
        synthetic(60, 1280, 720, 100, 40)
    }

    fn error_codes(v: &Validation) -> Vec<&'static str> {
        v.errors.iter().map(|i| i.code).collect()
    }

    fn warning_codes(v: &Validation) -> Vec<&'static str> {
        v.warnings.iter().map(|i| i.code).collect()
    }

    fn validate_hashed(bytes: &[u8]) -> Validation {
        validate_asset(&sha256_hex(bytes), bytes)
    }

    // -- sniffing ------------------------------------------------------------

    #[test]
    fn real_fixture_sniffs_clean() {
        let info = sniff_mp4(TINY_MP4).expect("ffmpeg fixture must parse");
        assert_eq!((info.width, info.height), (64, 36));
        assert!((info.duration_secs - 2.0).abs() < 0.2, "{info:?}");
    }

    #[test]
    fn real_fixture_with_aac_audio_sniffs_clean() {
        let info = sniff_mp4(TINY_MP4_AAC).expect("aac fixture must parse");
        assert_eq!((info.width, info.height), (64, 36));
    }

    #[test]
    fn synthetic_mp4_sniffs_with_expected_facts() {
        let info = sniff_mp4(&synthetic(95, 1920, 1080, 77, 31)).unwrap();
        assert_eq!(
            info,
            Mp4Info {
                duration_secs: 95.0,
                width: 1920,
                height: 1080
            }
        );
    }

    #[test]
    fn non_mp4_bytes_are_rejected() {
        for bytes in [
            b"".as_slice(),
            b"<!doctype html><p>not video</p>".as_slice(),
            &[0u8; 64],
            // WebM/Matroska magic.
            &[0x1a, 0x45, 0xdf, 0xa3, 0, 0, 0, 0, 0, 0, 0, 0],
        ] {
            assert!(sniff_mp4(bytes).is_err(), "{bytes:?} must not sniff as MP4");
        }
    }

    #[test]
    fn missing_moov_or_mvhd_is_rejected() {
        let ftyp_only = boxed(b"ftyp", b"isom\0\0\0\0");
        assert!(sniff_mp4(&ftyp_only).unwrap_err().contains("moov"));

        let mut no_mvhd = boxed(b"ftyp", b"isom\0\0\0\0");
        no_mvhd.extend_from_slice(&boxed(b"moov", &trak(b"vide", &stsd(&visual_entry(b"avc1", 64, 36, &avcc(100, 30))))));
        assert!(sniff_mp4(&no_mvhd).unwrap_err().contains("mvhd"));
    }

    #[test]
    fn wrong_video_codec_is_rejected() {
        // Patch the synthetic's sample-entry 4CC to HEVC.
        let entry = visual_entry(b"hvc1", 64, 36, &avcc(100, 30));
        let mut moov_content = mvhd_v0(1000, 2000);
        moov_content.extend_from_slice(&trak(b"vide", &stsd(&entry)));
        let mut bytes = boxed(b"ftyp", b"isom\0\0\0\0");
        bytes.extend_from_slice(&boxed(b"moov", &moov_content));
        let err = sniff_mp4(&bytes).unwrap_err();
        assert!(err.contains("hvc1"), "{err}");
    }

    #[test]
    fn disallowed_profile_and_level_are_rejected() {
        // High 10 (profile 110).
        assert!(sniff_mp4(&synthetic(2, 64, 36, 110, 30))
            .unwrap_err()
            .contains("profile_idc 110"));
        // Level 4.1.
        assert!(sniff_mp4(&synthetic(2, 64, 36, 100, 41))
            .unwrap_err()
            .contains("level 4.1"));
    }

    #[test]
    fn high_profile_extension_bytes_are_enforced_when_present() {
        // avcC with extension declaring 4:2:2 chroma.
        let mut avcc_content = vec![1, 100, 0, 30, 0xff, 0xe0, 0];
        avcc_content.extend_from_slice(&[0xfc | 2, 0xf8, 0xf8, 0]); // chroma 2
        let entry = visual_entry(b"avc1", 64, 36, &boxed(b"avcC", &avcc_content));
        let mut moov_content = mvhd_v0(1000, 2000);
        moov_content.extend_from_slice(&trak(b"vide", &stsd(&entry)));
        let mut bytes = boxed(b"ftyp", b"isom\0\0\0\0");
        bytes.extend_from_slice(&boxed(b"moov", &moov_content));
        assert!(sniff_mp4(&bytes).unwrap_err().contains("4:2:0"));
    }

    #[test]
    fn audio_track_rules() {
        // mp4a stereo passes structurally (esds absent → best-effort skip).
        let mut moov_content = mvhd_v0(1000, 2000);
        moov_content
            .extend_from_slice(&trak(b"vide", &stsd(&visual_entry(b"avc1", 64, 36, &avcc(77, 30)))));
        moov_content.extend_from_slice(&trak(b"soun", &stsd(&audio_entry(b"mp4a", 2))));
        let mut ok = boxed(b"ftyp", b"isom\0\0\0\0");
        ok.extend_from_slice(&boxed(b"moov", &moov_content));
        assert!(sniff_mp4(&ok).is_ok());

        // Non-mp4a audio rejected.
        let mut moov_content = mvhd_v0(1000, 2000);
        moov_content
            .extend_from_slice(&trak(b"vide", &stsd(&visual_entry(b"avc1", 64, 36, &avcc(77, 30)))));
        moov_content.extend_from_slice(&trak(b"soun", &stsd(&audio_entry(b"Opus", 2))));
        let mut bad = boxed(b"ftyp", b"isom\0\0\0\0");
        bad.extend_from_slice(&boxed(b"moov", &moov_content));
        assert!(sniff_mp4(&bad).unwrap_err().contains("Opus"));

        // > 2 channels rejected.
        let mut moov_content = mvhd_v0(1000, 2000);
        moov_content
            .extend_from_slice(&trak(b"vide", &stsd(&visual_entry(b"avc1", 64, 36, &avcc(77, 30)))));
        moov_content.extend_from_slice(&trak(b"soun", &stsd(&audio_entry(b"mp4a", 6))));
        let mut bad = boxed(b"ftyp", b"isom\0\0\0\0");
        bad.extend_from_slice(&boxed(b"moov", &moov_content));
        assert!(sniff_mp4(&bad).unwrap_err().contains("6 channels"));
    }

    #[test]
    fn no_video_track_is_rejected() {
        let mut moov_content = mvhd_v0(1000, 2000);
        moov_content.extend_from_slice(&trak(b"soun", &stsd(&audio_entry(b"mp4a", 1))));
        let mut bytes = boxed(b"ftyp", b"isom\0\0\0\0");
        bytes.extend_from_slice(&boxed(b"moov", &moov_content));
        assert!(sniff_mp4(&bytes).unwrap_err().contains("no video track"));
    }

    // -- rule mapping ---------------------------------------------------------

    #[test]
    fn valid_upload_is_clean() {
        let v = validate_hashed(TINY_MP4);
        assert!(v.errors.is_empty(), "{:?}", v.errors);
        assert!(v.warnings.is_empty(), "{:?}", v.warnings);
    }

    #[test]
    fn e_asset_size_over_20mib_short_circuits() {
        let big = vec![0u8; MAX_ASSET_BYTES + 1];
        let v = validate_asset(&sha256_hex(&big), &big);
        assert_eq!(error_codes(&v), vec!["E-ASSET-SIZE"]);
    }

    #[test]
    fn e_asset_hash_on_mismatch_and_malformed_sha() {
        let wrong = "a".repeat(64);
        let v = validate_asset(&wrong, TINY_MP4);
        assert!(error_codes(&v).contains(&"E-ASSET-HASH"), "{:?}", v.errors);

        for malformed in ["", "abc", &"A".repeat(64), &"g".repeat(64)] {
            let v = validate_asset(malformed, TINY_MP4);
            assert!(
                error_codes(&v).contains(&"E-ASSET-HASH"),
                "sha {malformed:?} must be E-ASSET-HASH"
            );
        }
    }

    #[test]
    fn e_asset_type_on_garbage() {
        let garbage = b"definitely not an mp4 file at all";
        let v = validate_hashed(garbage);
        assert_eq!(error_codes(&v), vec!["E-ASSET-TYPE"]);
    }

    #[test]
    fn e_asset_duration_and_w_asset_long() {
        let over = synthetic(121, 640, 360, 77, 30);
        let v = validate_hashed(&over);
        assert_eq!(error_codes(&v), vec!["E-ASSET-DURATION"]);

        let long = synthetic(95, 640, 360, 77, 30);
        let v = validate_hashed(&long);
        assert!(v.errors.is_empty(), "{:?}", v.errors);
        assert_eq!(warning_codes(&v), vec!["W-ASSET-LONG"]);

        // At the hard boundary (exactly 120s): no error, but still > 90 warn.
        let at_cap = synthetic(120, 640, 360, 77, 30);
        let v = validate_hashed(&at_cap);
        assert!(v.errors.is_empty());
        assert_eq!(warning_codes(&v), vec!["W-ASSET-LONG"]);
    }

    #[test]
    fn w_asset_res_over_720p() {
        let v = validate_hashed(&synthetic(30, 1920, 1080, 100, 40));
        assert!(v.errors.is_empty(), "{:?}", v.errors);
        assert_eq!(warning_codes(&v), vec!["W-ASSET-RES"]);

        // Exactly 1280×720: clean.
        let v = validate_hashed(&ok_synthetic());
        assert!(warning_codes(&v).is_empty(), "{:?}", v.warnings);
    }

    #[test]
    fn hash_and_type_errors_are_collected_together() {
        let garbage = b"junk body";
        let v = validate_asset(&"b".repeat(64), garbage);
        let codes = error_codes(&v);
        assert!(codes.contains(&"E-ASSET-HASH"), "{codes:?}");
        assert!(codes.contains(&"E-ASSET-TYPE"), "{codes:?}");
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // SHA-256 of the empty string — the canonical test vector.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    // -- storage --------------------------------------------------------------

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                slug TEXT NOT NULL
            );
            INSERT INTO threads (id, project_id, slug) VALUES ('t1', 'p1', 'oauth-flow');
            ",
        )
        .unwrap();
        conn
    }

    fn tmp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "conceptify-assets-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn save_asset_stores_idempotently_under_thread_dir() {
        let conn = test_conn();
        let root = tmp_root("save");
        let sha = sha256_hex(TINY_MP4);

        let saved = save_asset(&conn, &root, "t1", &sha, TINY_MP4).unwrap();
        assert_eq!(saved.project_id, "p1");
        assert_eq!(saved.sha256, sha);
        assert_eq!(saved.bytes, TINY_MP4.len() as u64);
        assert_eq!(saved.url, format!("cfy-asset://localhost/t1/{sha}.mp4"));
        assert!(!saved.already_existed);
        assert!(saved.warnings.is_empty());

        let file = asset_file_path(&root, "p1", "oauth-flow", &sha);
        assert_eq!(fs::read(&file).unwrap(), TINY_MP4);
        // GC-by-construction: the asset lives inside the thread dir that
        // thread deletion removes recursively.
        assert!(file.starts_with(artifacts::thread_dir(&root, "p1", "oauth-flow")));

        // Idempotent re-upload: 200-equivalent, file not rewritten.
        let mtime = fs::metadata(&file).unwrap().modified().unwrap();
        let again = save_asset(&conn, &root, "t1", &sha, TINY_MP4).unwrap();
        assert!(again.already_existed);
        assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), mtime);

        // No stray temp file.
        let entries: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec![format!("{sha}.mp4")]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn save_asset_rejects_before_storing_anything() {
        let conn = test_conn();
        let root = tmp_root("reject");
        let sha = "c".repeat(64);

        let err = save_asset(&conn, &root, "t1", &sha, b"garbage").unwrap_err();
        let AssetError::Rejected(errors) = err else {
            panic!("expected Rejected, got {err:?}");
        };
        assert!(errors.iter().any(|i| i.code == "E-ASSET-HASH"));
        assert!(!assets_dir(&root, "p1", "oauth-flow").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn save_asset_unknown_thread() {
        let conn = test_conn();
        let root = tmp_root("ghost");
        let sha = sha256_hex(TINY_MP4);
        assert!(matches!(
            save_asset(&conn, &root, "ghost", &sha, TINY_MP4),
            Err(AssetError::ThreadNotFound(_))
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn deleting_the_thread_dir_garbage_collects_assets() {
        let conn = test_conn();
        let root = tmp_root("gc");
        let sha = sha256_hex(TINY_MP4);
        save_asset(&conn, &root, "t1", &sha, TINY_MP4).unwrap();
        let file = asset_file_path(&root, "p1", "oauth-flow", &sha);
        assert!(file.is_file());

        // What `delete_thread` does (best-effort recursive removal of the
        // thread dir) removes the assets with it — zero dedicated GC code.
        fs::remove_dir_all(artifacts::thread_dir(&root, "p1", "oauth-flow")).unwrap();
        assert!(!file.exists());

        let _ = fs::remove_dir_all(&root);
    }
}

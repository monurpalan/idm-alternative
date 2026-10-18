//! ZIP archives for testing selective extraction: a small writer that can produce what real
//! archives contain (stored and deflated entries, data descriptors, ZIP64 records, encrypted
//! or unsupported entries, hostile names), and `GET /zip/{name}` serving a sample archive
//! with or without range support.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::{OriginalUri, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::content;
use crate::server::{AppState, RangeResult, parse_range};

/// Stored (no compression).
pub const STORED: u16 = 0;
/// Deflate.
pub const DEFLATE: u16 = 8;

/// One entry to write.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    /// The uncompressed content.
    pub data: Vec<u8>,
    /// [`STORED`] or [`DEFLATE`] compress for real; any other method stores the data as is
    /// under that method number, for an entry a reader cannot extract.
    pub method: u16,
    /// Marks the entry encrypted (the data is not actually encrypted).
    pub encrypted: bool,
    /// Sizes and CRC go in a data descriptor after the data, zeros in the local header.
    pub descriptor: bool,
}

impl Entry {
    pub fn new(name: &str, data: impl Into<Vec<u8>>, method: u16) -> Self {
        Self {
            name: name.to_owned(),
            data: data.into(),
            method,
            encrypted: false,
            descriptor: false,
        }
    }

    /// A directory entry (`name` should end with `/`).
    pub fn dir(name: &str) -> Self {
        Self::new(name, Vec::new(), STORED)
    }
}

/// Writes `entries` as a ZIP archive. With `zip64`, every size and offset goes in ZIP64
/// extra fields and the archive ends with a ZIP64 end of central directory record and
/// locator, as tools write for archives past 4 GiB. `prefix` comes before the archive (like
/// the program part of a self-extracting archive) and offsets do not account for it.
pub fn write(entries: &[Entry], zip64: bool, prefix: &[u8], comment: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    let mut central = Vec::new();
    for e in entries {
        let offset = (out.len() - prefix.len()) as u64;
        let crc = crc32fast::hash(&e.data);
        let packed = match e.method {
            DEFLATE => {
                let mut enc =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                enc.write_all(&e.data).ok();
                enc.finish().unwrap_or_default()
            }
            _ => e.data.clone(),
        };
        let flags: u16 = u16::from(e.encrypted) | (u16::from(e.descriptor) << 3) | (1 << 11);
        let name = e.name.as_bytes();

        // Local header.
        let mut extra = Vec::new();
        if zip64 {
            extra.extend_from_slice(&1u16.to_le_bytes());
            extra.extend_from_slice(&16u16.to_le_bytes());
            let (u, c) = if e.descriptor {
                (0, 0)
            } else {
                (e.data.len() as u64, packed.len() as u64)
            };
            extra.extend_from_slice(&u.to_le_bytes());
            extra.extend_from_slice(&c.to_le_bytes());
        }
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&(if zip64 { 45u16 } else { 20 }).to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&e.method.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0x21, 0]); // time, date
        let (l_crc, l_c, l_u) = match (e.descriptor, zip64) {
            (true, _) => (0, 0, 0),
            (false, true) => (crc, u32::MAX, u32::MAX),
            (false, false) => (crc, packed.len() as u32, e.data.len() as u32),
        };
        out.extend_from_slice(&l_crc.to_le_bytes());
        out.extend_from_slice(&l_c.to_le_bytes());
        out.extend_from_slice(&l_u.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&extra);
        out.extend_from_slice(&packed);
        if e.descriptor {
            out.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            if zip64 {
                out.extend_from_slice(&(packed.len() as u64).to_le_bytes());
                out.extend_from_slice(&(e.data.len() as u64).to_le_bytes());
            } else {
                out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
                out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            }
        }

        // Central directory record.
        let mut extra = Vec::new();
        if zip64 {
            extra.extend_from_slice(&1u16.to_le_bytes());
            extra.extend_from_slice(&24u16.to_le_bytes());
            extra.extend_from_slice(&(e.data.len() as u64).to_le_bytes());
            extra.extend_from_slice(&(packed.len() as u64).to_le_bytes());
            extra.extend_from_slice(&offset.to_le_bytes());
        }
        let (c, u, o) = if zip64 {
            (u32::MAX, u32::MAX, u32::MAX)
        } else {
            (packed.len() as u32, e.data.len() as u32, offset as u32)
        };
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&0x031eu16.to_le_bytes()); // made by: Unix, 3.0
        central.extend_from_slice(&(if zip64 { 45u16 } else { 20 }).to_le_bytes());
        central.extend_from_slice(&flags.to_le_bytes());
        central.extend_from_slice(&e.method.to_le_bytes());
        central.extend_from_slice(&[0, 0, 0x21, 0]);
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&c.to_le_bytes());
        central.extend_from_slice(&u.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&o.to_le_bytes());
        central.extend_from_slice(name);
        central.extend_from_slice(&extra);
    }
    let cd_offset = (out.len() - prefix.len()) as u64;
    let cd_size = central.len() as u64;
    out.extend_from_slice(&central);
    let count = entries.len() as u64;
    if zip64 {
        let record = (out.len() - prefix.len()) as u64;
        out.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
        out.extend_from_slice(&44u64.to_le_bytes());
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&45u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&record.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
    }
    let (n, size, offset) = if zip64 {
        (u16::MAX, u32::MAX, u32::MAX)
    } else {
        (count as u16, cd_size as u32, cd_offset as u32)
    };
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    out.extend_from_slice(comment);
    out
}

/// The entries of the archive `GET /zip/{name}` serves for `seed`: text, a large
/// deflated entry, a stored one, a directory, an encrypted and an LZMA entry, and one whose
/// name climbs out of the destination folder.
pub fn sample_entries(seed: u64) -> Vec<Entry> {
    let mut random = vec![0u8; 1 << 20];
    content::fill(seed, 0, &mut random);
    // Compressible but not trivially: text lines with a counter.
    let text: String = (0..60_000)
        .map(|i| format!("line {i} of the sample archive {seed}\n"))
        .collect();
    let mut big = text.clone().into_bytes();
    big.extend_from_slice(&random);
    vec![
        Entry::new("readme.txt", format!("sample archive {seed}\n"), DEFLATE),
        Entry::dir("data/"),
        Entry {
            descriptor: true,
            ..Entry::new("data/big.bin", big, DEFLATE)
        },
        Entry::new("data/raw.bin", random, STORED),
        Entry::new("docs/notes.txt", text, DEFLATE),
        Entry {
            encrypted: true,
            ..Entry::new("secret.txt", "hidden", STORED)
        },
        Entry::new("packed.lzma", "not really lzma", 14),
        Entry::new("../evil.txt", "should never be written", STORED),
    ]
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct Params {
    seed: u64,
    #[serde(deserialize_with = "crate::scenario::flag")]
    ranges: bool,
    #[serde(deserialize_with = "crate::scenario::flag")]
    zip64: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            seed: 1,
            ranges: true,
            zip64: false,
        }
    }
}

/// The sample archive for `seed` (see [`sample_entries`]).
pub fn sample(seed: u64, zip64: bool) -> Vec<u8> {
    write(&sample_entries(seed), zip64, &[], b"idma sample")
}

pub(crate) async fn serve(
    State(app): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<Params>,
    headers: HeaderMap,
) -> Response {
    let data = sample(p.seed, p.zip64);
    let size = data.len() as u64;
    let (mut start, mut end, mut status) = (0, size, StatusCode::OK);
    if p.ranges
        && let Some(range) = headers.get(header::RANGE).and_then(|v| v.to_str().ok())
    {
        match parse_range(range, size) {
            RangeResult::Satisfiable(s, e) => {
                (start, end, status) = (s, e, StatusCode::PARTIAL_CONTENT)
            }
            RangeResult::Unsatisfiable => {
                return (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{size}"))],
                )
                    .into_response();
            }
            RangeResult::Ignore => {}
        }
    }
    let url = app.url(&uri.to_string());
    url.gets.fetch_add(1, Ordering::Relaxed);
    url.bytes_sent.fetch_add(end - start, Ordering::Relaxed);
    let mut out = HeaderMap::new();
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    out.insert(header::ETAG, HeaderValue::from_static("\"sample\""));
    if p.ranges {
        out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }
    if status == StatusCode::PARTIAL_CONTENT
        && let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{}/{size}", end - 1))
    {
        out.insert(header::CONTENT_RANGE, v);
    }
    let body = data[start as usize..end as usize].to_vec();
    (status, out, body).into_response()
}

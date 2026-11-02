//! Real files of the kinds Shardload checks by their structure, made from a seed: ZIP, gzip,
//! xz, PNG, PDF, MP4 and Matroska (with CRC-32 elements). `GET /sample/{name}?kind=&seed=`
//! serves one with ranges and an ETag; `corrupt_at=N` flips the byte at N in the first
//! response that sends it (or, with `corrupt_get=K`, in the K-th request to the URL, so a
//! client that looks at the file first gets it in its download), so a download gets it
//! damaged and a request for that range again gets it right.

use std::io::Write;

use futures_util::StreamExt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::{OriginalUri, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::content;
use crate::server::{AppState, RangeResult, parse_range};

/// The kinds [`sample`] makes.
pub const KINDS: [&str; 8] = ["zip", "gz", "xz", "png", "pdf", "mp4", "mp4end", "mkv"];

/// A sample of `kind` (see [`KINDS`]) for `seed`, about a megabyte or two.
pub fn sample(kind: &str, seed: u64) -> Option<Vec<u8>> {
    Some(match kind {
        "zip" => crate::zip::sample(seed, false),
        "gz" => gzip(seed),
        "xz" => xz(seed),
        "png" => png(seed),
        "pdf" => pdf(seed),
        "mp4" => mp4(seed, false),
        "mp4end" => mp4(seed, true),
        "mkv" => mkv(seed),
        _ => return None,
    })
}

/// Text that compresses, with some seeded bytes that do not.
fn data(seed: u64, len: usize) -> Vec<u8> {
    let mut out: Vec<u8> = (0..)
        .flat_map(|i| format!("line {i} of sample {seed}\n").into_bytes())
        .take(len / 2)
        .collect();
    let mut random = vec![0u8; len - out.len()];
    content::fill(seed, 0, &mut random);
    out.extend_from_slice(&random);
    out
}

fn gzip(seed: u64) -> Vec<u8> {
    // Two members, as `cat a.gz b.gz` makes.
    let mut out = Vec::new();
    for part in 0..2 {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&data(seed + part, 600 << 10)).expect("gzip");
        out.extend(gz.finish().expect("gzip"));
    }
    out
}

fn xz(seed: u64) -> Vec<u8> {
    let mut xz =
        lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).expect("xz");
    xz.write_all(&data(seed, 512 << 10)).expect("xz");
    xz.finish().expect("xz")
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let mut crc = crc32fast::Hasher::new();
    crc.update(kind);
    crc.update(body);
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}

/// 512×512 RGB, its pixels split over several IDAT chunks.
fn png(seed: u64) -> Vec<u8> {
    let (w, h) = (512u32, 512u32);
    let mut raw = Vec::with_capacity((h * (w * 3 + 1)) as usize);
    let mut row = vec![0u8; (w * 3) as usize];
    for y in 0..h {
        content::fill(seed, u64::from(y) * u64::from(w), &mut row);
        raw.push(0);
        raw.extend_from_slice(&row);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&raw).expect("zlib");
    let pixels = z.finish().expect("zlib");
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    for part in pixels.chunks(64 << 10) {
        chunk(&mut out, b"IDAT", part);
    }
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A few pages whose contents are Flate streams, with a classic cross-reference table.
fn pdf(seed: u64) -> Vec<u8> {
    let pages = 8;
    let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    let mut object = |out: &mut Vec<u8>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    object(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..pages).map(|i| format!("{} 0 R ", 3 + 2 * i)).collect();
    let pages_dict = format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>");
    object(&mut out, pages_dict.as_bytes());
    for i in 0..pages {
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R >>",
            4 + 2 * i
        );
        object(&mut out, page.as_bytes());
        let mut text = Vec::new();
        for line in 0..400 {
            text.extend(
                format!("BT /F1 9 Tf 20 {line} Td (page {i} line {line} seed {seed}) Tj ET\n")
                    .bytes(),
            );
        }
        let mut noise = vec![0u8; 16 << 10];
        content::fill(seed + i, 0, &mut noise);
        text.extend(noise.iter().map(|b| b'a' + b % 26));
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&text).expect("zlib");
        let data = z.finish().expect("zlib");
        let mut stream = format!(
            "<< /Length {} /Filter /FlateDecode >>\nstream\n",
            data.len()
        )
        .into_bytes();
        stream.extend_from_slice(&data);
        stream.extend_from_slice(b"\nendstream");
        object(&mut out, &stream);
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).bytes());
    for o in &offsets {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .bytes(),
    );
    out
}

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

/// `ftyp`, a `moov` for a 90 s, 1280x720 video track, and a megabyte of `mdat`; with
/// `moov_last`, the `moov` after the media, as files not made for streaming have it.
fn mp4(seed: u64, moov_last: bool) -> Vec<u8> {
    let stco = mp4_box(b"stco", &[0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]);
    let stbl = mp4_box(b"stbl", &stco);
    let minf = mp4_box(b"minf", &stbl);
    // Version and flags, pre_defined, then the handler type.
    let mut hdlr = vec![0u8; 25];
    hdlr[8..12].copy_from_slice(b"vide");
    let mdia = [mp4_box(b"mdhd", &[0u8; 24]), mp4_box(b"hdlr", &hdlr), minf].concat();
    // The picture size ends tkhd, 16.16 fixed point.
    let mut tkhd = vec![0u8; 84];
    tkhd[76..80].copy_from_slice(&(1280u32 << 16).to_be_bytes());
    tkhd[80..84].copy_from_slice(&(720u32 << 16).to_be_bytes());
    let trak = mp4_box(
        b"trak",
        &[mp4_box(b"tkhd", &tkhd), mp4_box(b"mdia", &mdia)].concat(),
    );
    // Version 0: times, then the timescale and the duration: 90 000 / 1000 s.
    let mut mvhd = vec![0u8; 100];
    mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
    mvhd[16..20].copy_from_slice(&90_000u32.to_be_bytes());
    let moov = mp4_box(b"moov", &[mp4_box(b"mvhd", &mvhd), trak].concat());
    let mut media = vec![0u8; 1 << 20];
    content::fill(seed, 0, &mut media);
    let ftyp = mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso2mp41");
    let mdat = mp4_box(b"mdat", &media);
    if moov_last {
        [ftyp, mdat, moov].concat()
    } else {
        [ftyp, moov, mdat].concat()
    }
}

/// An EBML element: its ID bytes, an 8-byte size, the data.
fn element(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.push(0x01);
    out.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
    out.extend_from_slice(body);
    out
}

/// A master element starting with a CRC-32 of the rest, as mkvmerge writes them.
fn with_crc(id: &[u8], children: &[u8]) -> Vec<u8> {
    let crc = crc32fast::hash(children).to_le_bytes();
    element(id, &[&[0xBF, 0x84][..], &crc, children].concat())
}

/// Info, Tracks and four Clusters of a quarter megabyte each, all with CRC-32s.
fn mkv(seed: u64) -> Vec<u8> {
    let header = element(
        &[0x1A, 0x45, 0xDF, 0xA3],
        &element(&[0x42, 0x82], b"matroska"),
    );
    let info = with_crc(
        &[0x15, 0x49, 0xA9, 0x66],
        &element(&[0x2A, 0xD7, 0xB1], &[0x0F, 0x42, 0x40]),
    );
    let track = element(
        &[0xAE],
        &[element(&[0xD7], &[1]), element(&[0x83], &[1])].concat(),
    );
    let tracks = with_crc(&[0x16, 0x54, 0xAE, 0x6B], &track);
    let mut segment = [info, tracks].concat();
    for i in 0..4u64 {
        let mut block = vec![0u8; 256 << 10];
        content::fill(seed + i, 0, &mut block);
        let children = [element(&[0xE7], &[i as u8]), element(&[0xA3], &block)].concat();
        segment.extend(with_crc(&[0x1F, 0x43, 0xB6, 0x75], &children));
    }
    [header, element(&[0x18, 0x53, 0x80, 0x67], &segment)].concat()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub(crate) struct Params {
    kind: String,
    seed: u64,
    corrupt_at: Option<u64>,
    corrupt_get: Option<u64>,
    /// Bytes per second each response is sent at.
    rate: Option<crate::scenario::Size>,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            kind: "zip".into(),
            seed: 1,
            corrupt_at: None,
            corrupt_get: None,
            rate: None,
        }
    }
}

pub(crate) async fn serve(
    State(app): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<Params>,
    headers: HeaderMap,
) -> Response {
    let Some(mut data) = sample(&p.kind, p.seed) else {
        return (StatusCode::NOT_FOUND, "unknown kind").into_response();
    };
    let size = data.len() as u64;
    let (mut start, mut end, mut status) = (0, size, StatusCode::OK);
    if let Some(range) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
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
    let nth = url.gets.fetch_add(1, Ordering::Relaxed) + 1;
    url.bytes_sent.fetch_add(end - start, Ordering::Relaxed);
    if let Some(at) = p.corrupt_at.filter(|at| (start..end).contains(at))
        && match p.corrupt_get {
            Some(k) => k == nth,
            None => !url.corrupted.swap(true, Ordering::Relaxed),
        }
    {
        data[at as usize] ^= 0xFF;
    }
    let mut out = HeaderMap::new();
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    out.insert(header::ETAG, HeaderValue::from_static("\"sample\""));
    out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if status == StatusCode::PARTIAL_CONTENT
        && let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{}/{size}", end - 1))
    {
        out.insert(header::CONTENT_RANGE, v);
    }
    let body = data[start as usize..end as usize].to_vec();
    match p.rate.map(|r| r.0).filter(|r| *r > 0) {
        None => (status, out, body).into_response(),
        Some(rate) => (
            status,
            out,
            axum::body::Body::from_stream(paced(body, rate)),
        )
            .into_response(),
    }
}

/// `body` sent at `rate` bytes per second, in pieces of a tenth of a second.
fn paced(
    body: Vec<u8>,
    rate: u64,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    let piece = (rate / 10).max(1) as usize;
    let pieces: Vec<bytes::Bytes> = body
        .chunks(piece)
        .map(bytes::Bytes::copy_from_slice)
        .collect();
    futures_util::stream::iter(pieces).then(|c| async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        Ok(c)
    })
}

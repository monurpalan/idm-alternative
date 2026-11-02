//! Checking files by their structure: every kind sound as made, damaged where one byte was
//! changed (with that byte in a damaged range), never called damaged when it is not, and no
//! crash or hang on hostile input.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use shardload_engine::structure::{Verdict, check};
use shardload_testserver::samples::{KINDS, sample};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join("shardload-engine-tests")
        .join(format!("{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(&d).expect("dir");
    d
}

fn verdict(path: &Path, bytes: &[u8]) -> Verdict {
    std::fs::write(path, bytes).expect("write");
    check(path, &AtomicBool::new(false)).expect("check")
}

fn position(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Where a changed byte must be found, per kind: inside compressed data, a CRC-covered chunk,
/// a box's size.
fn flip_at(kind: &str, file: &[u8]) -> usize {
    let find =
        |needle: &[u8]| position(file, needle).unwrap_or_else(|| panic!("{kind}: no {needle:?}"));
    match kind {
        // Inside data/big.bin's deflated data.
        "zip" => find(b"data/big.bin") + 200,
        "gz" | "xz" => file.len() / 2,
        // Inside the second IDAT.
        "png" => find(b"IDAT") + (64 << 10) + 500,
        // Inside the third page's content stream.
        "pdf" => {
            let object = find(b"8 0 obj");
            object
                + position(
                    &file[object..],
                    b"stream
",
                )
                .expect("stream")
                + 1000
        }
        // The size of `moov`.
        "mp4" => find(b"moov") - 2,
        // Inside the second cluster's block.
        "mkv" => {
            let first = find(&[0x1F, 0x43, 0xB6, 0x75]);
            first + 4 + (256 << 10) + 2000
        }
        _ => unreachable!(),
    }
}

#[test]
fn each_kind_is_sound_and_a_changed_byte_is_found() {
    let d = dir("structure");
    for kind in KINDS {
        let good = sample(kind, 1).expect(kind);
        let path = d.join(format!("a.{kind}"));
        let v = verdict(&path, &good);
        assert!(matches!(v, Verdict::Sound(_)), "{kind} sound: {v:?}");
        let at = flip_at(kind, &good);
        let mut bad = good.clone();
        bad[at] ^= 0xFF;
        match verdict(&path, &bad) {
            Verdict::Damaged(_, ranges) => {
                assert!(
                    ranges.iter().any(|(s, e)| (*s..*e).contains(&(at as u64))),
                    "{kind}: byte {at} not in {ranges:?}"
                );
                let covered: u64 = ranges.iter().map(|(s, e)| e - s).sum();
                println!("{kind}: {} bytes, {covered} to fetch again", good.len());
            }
            other => panic!("{kind}: {other:?}"),
        }
    }
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn a_pdf_table_entry_pointing_nowhere_is_found() {
    let d = dir("structure-xref");
    let good = sample("pdf", 1).expect("pdf");
    // The fifth entry's offset, one digit changed.
    let table = position(
        &good, b"xref
",
    )
    .expect("xref");
    let first = table + position(&good[table..], b" 00000 n").expect("entry") - 10;
    let entry = first + 20 * 4 + 7;
    let mut bad = good.clone();
    bad[entry] = if bad[entry] == b'9' {
        b'1'
    } else {
        bad[entry] + 1
    };
    match verdict(&d.join("x.pdf"), &bad) {
        Verdict::Damaged(_, ranges) => assert!(
            ranges
                .iter()
                .any(|(s, e)| (*s..*e).contains(&(entry as u64))),
            "{entry} not in {ranges:?}"
        ),
        other => panic!("{other:?}"),
    }
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn other_files_are_not_judged() {
    let d = dir("structure-other");
    for bytes in [&b"plain text"[..], b"", b"PK", &[0x1F, 0x8B], b"%PDF-1.7\n"] {
        let v = verdict(&d.join("x"), bytes);
        assert!(
            !matches!(v, Verdict::Damaged(..)) || bytes.len() <= 2,
            "{bytes:?}: {v:?}"
        );
    }
    // A PDF whose stream length is a reference and whose table is a stream: nothing judged.
    let pdf = b"%PDF-1.5\n1 0 obj\n<< /Length 2 0 R /Filter /FlateDecode >>\nstream\nxyz\nendstream\nendobj\nstartxref\n9\n%%EOF\n";
    assert!(!matches!(
        verdict(&d.join("p.pdf"), pdf),
        Verdict::Damaged(..)
    ));
    // An MP4 with zeros after its last box, as some writers leave.
    let mut mp4 = sample("mp4", 2).expect("mp4");
    mp4.extend_from_slice(&[0u8; 4096]);
    assert!(matches!(verdict(&d.join("z.mp4"), &mp4), Verdict::Sound(_)));
    std::fs::remove_dir_all(&d).ok();
}

/// A tiny random generator, so the test needs no crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

#[test]
fn hostile_input_neither_crashes_nor_hangs() {
    let d = dir("structure-fuzz");
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let started = Instant::now();
    for round in 0..300 {
        let kind = KINDS[round % KINDS.len()];
        let mut bytes = sample(kind, 3).expect(kind);
        match rng.below(4) {
            // Bytes changed here and there.
            0 => {
                for _ in 0..=rng.below(16) {
                    let at = rng.below(bytes.len());
                    bytes[at] = rng.next() as u8;
                }
            }
            // Cut short.
            1 => bytes.truncate(rng.below(bytes.len())),
            // A run of garbage at the start of its structure.
            2 => {
                let at = rng.below(bytes.len().min(4096));
                for b in bytes.iter_mut().skip(at).take(64) {
                    *b = rng.next() as u8;
                }
            }
            // Sizes and lengths turned huge.
            _ => {
                for _ in 0..8 {
                    let at = rng.below(bytes.len().saturating_sub(4));
                    bytes[at..at + 4].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xF0]);
                }
            }
        }
        let path = d.join(format!("f{round}"));
        let one = Instant::now();
        let _ = verdict(&path, &bytes);
        assert!(
            one.elapsed() < Duration::from_secs(10),
            "{kind} round {round} took too long"
        );
    }
    println!(
        "300 hostile files in {:.1} s",
        started.elapsed().as_secs_f64()
    );
    std::fs::remove_dir_all(&d).ok();
}

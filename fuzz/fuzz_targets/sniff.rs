#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (head, name) = data.split_at(data.len().min(64));
    shardload_engine::fuzz::sniff(head, &String::from_utf8_lossy(name));
});

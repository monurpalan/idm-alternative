#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    shardload_engine::fuzz::hls(text);
    let _ = shardload_media::parse("https://example.com/a/index.m3u8", text);
});

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| shardload_engine::fuzz::checksum(text));

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    shardload_engine::fuzz::file_name(text);
    shardload_engine::fuzz::chosen_name(text);
    shardload_engine::fuzz::safe_path(text);
});

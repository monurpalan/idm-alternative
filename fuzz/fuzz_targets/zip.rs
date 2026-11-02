#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    shardload_engine::fuzz::zip(data, &shardload_fuzz::scratch());
});

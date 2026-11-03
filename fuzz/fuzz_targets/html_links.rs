#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|html: &str| shardload_manager::grabber::fuzz_links(html));

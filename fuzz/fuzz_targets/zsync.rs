#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(control) = shardload_engine::zsync::Control::parse(data) {
        let _ = control.target("https://example.com/a/disk.iso.zsync");
    }
});

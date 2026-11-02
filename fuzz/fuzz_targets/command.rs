#![no_main]
use libfuzzer_sys::fuzz_target;
use shardload_manager::command::{check_headers, parse_command, parse_header_lines};

fuzz_target!(|text: &str| {
    if let Ok(pasted) = parse_command(text) {
        let _ = check_headers(&pasted.headers);
    }
    if let Ok(headers) = parse_header_lines(text) {
        let _ = check_headers(&headers);
    }
});

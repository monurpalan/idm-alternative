//! Inputs that once made the command parser fail under fuzzing (`fuzz/regressions/command/`),
//! parsed again: an error is fine, a panic is not.

use shardload_manager::command::{check_headers, parse_command, parse_header_lines};

#[test]
fn pasted_commands() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/regressions/command");
    for entry in std::fs::read_dir(dir).expect("the regression inputs") {
        let data = std::fs::read(entry.expect("an input").path()).expect("an input");
        let Ok(text) = std::str::from_utf8(&data) else {
            continue;
        };
        if let Ok(pasted) = parse_command(text) {
            let _ = check_headers(&pasted.headers);
        }
        let _ = parse_header_lines(text);
    }
}

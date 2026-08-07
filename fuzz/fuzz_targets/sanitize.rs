#![no_main]
//! sanitize() is the barrier between server-supplied text and the terminal.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    let clean = librespeed_cli::output::sanitize(data);
    // No control character may survive: those are exactly what lets a server
    // drive the terminal or forge a --list entry.
    for c in clean.chars() {
        let c = c as u32;
        assert!(
            !(c < 0x20 || c == 0x7f || (0x80..=0x9f).contains(&c)),
            "control character {c:#x} survived sanitize"
        );
    }
});

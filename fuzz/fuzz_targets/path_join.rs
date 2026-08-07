#![no_main]
//! path_join/path_clean reimplement Go's path.Join to keep backend URLs
//! identical to the Go client. Both halves are attacker-influenced.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: (&str, &str)| {
    let (a, b) = data;
    let joined = librespeed_cli::util::path_join(a, b);
    // Joining must never produce a traversal that escapes an absolute base.
    if a.starts_with('/') {
        assert!(!joined.starts_with(".."), "escaped absolute base: {joined:?}");
    }
});

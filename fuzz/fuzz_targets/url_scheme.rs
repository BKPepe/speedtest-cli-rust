#![no_main]
//! Server URLs come off the wire and go through hand-written scheme rewriting
//! that mimics Go's url.URL, so odd inputs (`://x`, `http:///`, embedded
//! credentials, fragments) must not panic.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    for force in [
        librespeed_cli::speedtest::ForceScheme::Nothing,
        librespeed_cli::speedtest::ForceScheme::Https,
        librespeed_cli::speedtest::ForceScheme::Http,
    ] {
        let rewritten = librespeed_cli::speedtest::apply_scheme(data, force);
        // Whatever comes out must at least be parseable or cleanly rejected.
        let _ = url::Url::parse(&rewritten);
    }
});

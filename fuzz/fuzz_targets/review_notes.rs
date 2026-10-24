#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| waymux::fuzzing::review_notes(data));

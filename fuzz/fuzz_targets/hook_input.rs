#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| waymux::fuzzing::hook_input(data));

#[test]
fn diskann3_backend_public_api_rejects_same_generation_forged_exact_fetch() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/diskann3_backend_forged_same_generation_exact_fetch.rs");
}

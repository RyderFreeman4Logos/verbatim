#[test]
fn public_api_compile_fail_contracts() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/diskann3_backend_forged_same_generation_exact_fetch.rs");
    cases.compile_fail("tests/ui/diskann3_backend_external_impl.rs");
    cases.compile_fail("tests/ui/hybrid_fusion_error_not_serializable.rs");
}

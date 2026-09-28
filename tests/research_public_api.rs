#[test]
fn fitted_residual_is_a_safe_public_measurement_api() {
    let reference = [0.25_f32, -0.5, 0.75, -0.125];
    let scaled = reference.map(|sample| sample * 0.5);

    assert!(shr_synth::research::fitted_residual_db(&scaled, &reference) < -200.0);
    assert_eq!(shr_synth::research::fitted_residual_db(&[], &[]), 0.0);
}

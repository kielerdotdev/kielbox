use rcr_dpdfnet::{Denoiser, Model};
fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| f32::from_le_bytes(*v))
        .collect()
}
#[test]
fn spectrum_and_recurrent_state_match_upstream_onnx() {
    let inputs = floats(include_bytes!("fixtures/model-inputs.f32"));
    let expected = floats(include_bytes!("fixtures/model-outputs.f32"));
    let final_state = floats(include_bytes!("fixtures/model-final-state.f32"));
    let mut model = Model::bundled().unwrap();
    let mut state = Model::initial_state();
    let mut next = state.clone();
    let mut maximum = 0f32;
    for (input, expected) in inputs
        .as_chunks::<962>()
        .0
        .iter()
        .zip(expected.as_chunks::<962>().0)
    {
        let mut output = [0.; 962];
        model
            .process_spectrum(input, &state, &mut output, &mut next)
            .unwrap();
        for (a, b) in output.iter().zip(expected) {
            maximum = maximum.max((a - b).abs());
        }
        std::mem::swap(&mut state, &mut next);
    }
    let state_error = state
        .iter()
        .zip(final_state)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    eprintln!("spectrum maximum error={maximum}; state maximum error={state_error}");
    assert!(maximum < 0.003, "spectrum parity error {maximum}");
    assert!(state_error < 0.003, "state parity error {state_error}");
}
#[test]
fn pcm_matches_reference_and_irregular_chunks_keep_exact_length() {
    let input = floats(include_bytes!("fixtures/audio-input.f32"));
    let expected = floats(include_bytes!("fixtures/audio-output.f32"));
    let mut denoiser = Denoiser::new().unwrap();
    let mut output = Vec::new();
    for chunk in input.chunks(317) {
        denoiser
            .process(chunk, |v| {
                output.extend_from_slice(v);
                Ok(())
            })
            .unwrap();
    }
    denoiser
        .finish(|v| {
            output.extend_from_slice(v);
            Ok(())
        })
        .unwrap();
    assert_eq!(output.len(), input.len());
    let maximum = output
        .iter()
        .zip(&expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    eprintln!("PCM maximum error={maximum}");
    assert!(maximum < 0.001, "PCM parity error {maximum}");
    denoiser.reset();
    let mut reset = Vec::new();
    denoiser
        .process(&input, |v| {
            reset.extend_from_slice(v);
            Ok(())
        })
        .unwrap();
    denoiser
        .finish(|v| {
            reset.extend_from_slice(v);
            Ok(())
        })
        .unwrap();
    assert_eq!(output, reset);
}

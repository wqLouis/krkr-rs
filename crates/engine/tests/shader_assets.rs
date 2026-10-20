//! Tests for the effect programs this crate ships (`engine::shader::builtin`).
//!
//! The programs are committed as GLSL sources and compiled SPIR-V side by side.
//! These tests are the host-side contract: every blob is a module this build
//! accepts, and every program registers with the descriptor the shaders were
//! written for. They need no device, so they run on every platform that runs the
//! test suite — including the one that builds the APK.

use api::shader::{EFFECT_PUSH_CONSTANT_BYTES, EffectUniforms, ShaderModule, ShaderStage};
use engine::shader::builtin;
use engine::shader::{ProgramDesc, ShaderDataError, ShaderError, ShaderLibrary};

#[test]
fn every_committed_stage_is_a_module_this_build_accepts() {
    let vertex = ShaderModule::vertex(builtin::VERTEX);
    assert_eq!(vertex.stage, ShaderStage::Vertex);
    vertex.validate().expect("the shared vertex stage is valid");

    for (name, bytes) in [
        ("crossfade", builtin::CROSSFADE_FRAGMENT),
        ("scroll", builtin::SCROLL_FRAGMENT),
    ] {
        let fragment = ShaderModule::fragment(bytes);
        assert_eq!(fragment.stage, ShaderStage::Fragment);
        fragment
            .validate()
            .unwrap_or_else(|error| panic!("{name} fragment stage is invalid: {error}"));
    }

    // The blocks the pipeline layout reserves: 64 bytes of push constants and
    // the 16-word uniform block both programs declare.
    assert_eq!(EFFECT_PUSH_CONSTANT_BYTES, 64);
    assert_eq!(builtin::UNIFORM_WORDS, 16);
    assert_eq!(builtin::OPACITY_UNIFORM, 15);
    assert_eq!(builtin::PARAM_UNIFORM, 8);
    // The opacity word must lie inside the uniform block. Both words are
    // constants, so the check runs at compile time: a layout that does not
    // hold fails the build instead of the test.
    const {
        assert!(
            builtin::OPACITY_UNIFORM < builtin::UNIFORM_WORDS,
            "the opacity word lies inside the block"
        );
    }
}

#[test]
fn the_crossfade_program_registers_with_two_sources() {
    let mut library = ShaderLibrary::new();
    let id = library
        .load(builtin::crossfade())
        .expect("register crossfade");
    let program = library.program(id).expect("registered program");

    assert_eq!(program.name(), "crossfade");
    assert_eq!(program.vertex(), builtin::VERTEX);
    assert_eq!(program.fragment(), builtin::CROSSFADE_FRAGMENT);
    assert_eq!(program.bindings(), &builtin::CROSSFADE_BINDINGS);
    assert_eq!(program.bindings().len(), 2);
    assert_eq!(program.uniform_words(), builtin::UNIFORM_WORDS);
    assert_eq!(program.opacity_uniform(), Some(builtin::OPACITY_UNIFORM));

    // The descriptor the backend would build from it is valid as it stands.
    program
        .pipeline_desc(api::blend::BlendMode::SourceOver)
        .validate()
        .expect("the registered descriptor is a valid pipeline descriptor");
}

#[test]
fn the_scroll_program_registers_one_source_per_pass() {
    let mut library = ShaderLibrary::new();
    let id = library.load(builtin::scroll()).expect("register scroll");
    let program = library.program(id).expect("registered program");

    assert_eq!(program.name(), "scroll");
    assert_eq!(program.bindings(), &builtin::SCROLL_BINDINGS);
    assert_eq!(
        program.bindings().len(),
        1,
        "a scroll pass places one source"
    );
    assert_eq!(program.uniform_words(), builtin::UNIFORM_WORDS);
    assert_eq!(program.opacity_uniform(), Some(builtin::OPACITY_UNIFORM));
}

#[test]
fn the_two_programs_are_distinct_registrations() {
    let mut library = ShaderLibrary::new();
    let crossfade = library.load(builtin::crossfade()).expect("register");
    let scroll = library.load(builtin::scroll()).expect("register");
    assert_ne!(crossfade, scroll);
    assert_eq!(library.len(), 2);

    // Re-registering the shipped descriptors is idempotent, so a startup path
    // that loads them twice does not grow the table.
    assert_eq!(
        library.load(builtin::crossfade()).expect("register again"),
        crossfade
    );
    assert_eq!(
        library.load(builtin::scroll()).expect("register again"),
        scroll
    );
    assert_eq!(library.len(), 2);
}

#[test]
fn a_truncated_shipped_program_is_refused() {
    let mut library = ShaderLibrary::new();
    let truncated = ProgramDesc::new(
        "crossfade",
        builtin::VERTEX,
        &builtin::CROSSFADE_FRAGMENT[..16],
    );
    let error = library
        .load(truncated)
        .expect_err("a truncated module is refused");
    assert!(
        matches!(
            error,
            ShaderError::Data(ShaderDataError::TruncatedModule {
                stage: ShaderStage::Fragment,
                len: 16
            })
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn the_uniform_block_the_programs_declare_is_usable() {
    // The words a draw of either program carries: two texture rectangles, one
    // parameters vec4 and the opacity vec4 the renderer overwrites.
    let mut words = [0.0f32; builtin::UNIFORM_WORDS as usize];
    words[0..4].copy_from_slice(&[0.0, 0.0, 1.0, 1.0]);
    words[4..8].copy_from_slice(&[0.0, 0.0, 1.0, 1.0]);
    words[builtin::PARAM_UNIFORM as usize] = 0.5;
    words[builtin::OPACITY_UNIFORM as usize] = 1.0;

    let uniforms = EffectUniforms::from_words(&words).expect("a 16-word block is valid");
    assert_eq!(uniforms.len(), builtin::UNIFORM_WORDS);
    assert_eq!(uniforms.byte_len(), 64);
    assert_eq!(
        uniforms.words()[builtin::PARAM_UNIFORM as usize],
        0.5,
        "the phase weight reaches the shader"
    );
}

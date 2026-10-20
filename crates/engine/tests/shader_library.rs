//! Integration tests for the generic shader library (`engine::shader`).
//!
//! The library is plain middle-layer state: it validates a program descriptor,
//! issues ids and answers lookups. Nothing here needs a device, so the whole
//! contract runs on a headless host — the same code path the renderer uses.
//!
//! Two properties matter for the layers above: registration is **idempotent**
//! (a startup path that runs twice must not grow the table) and ids are
//! **stable and never reused** (a stale id must be a typed miss, never another
//! program).

use api::blend::BlendMode;
use api::shader::{
    BindingDesc, EffectUniforms, MAX_EFFECT_TEXTURES, MAX_EFFECT_UNIFORMS, SamplerKind,
    ShaderDataError, ShaderModule, ShaderStage, VertexLayout,
};
use engine::shader::{LoadedProgram, ProgramDesc, ShaderError, ShaderLibrary};
use render::{RenderPipeline, RenderState, begin_render_state};
use testkit::headless_window;

/// A headless render pipeline and its state: enough for the release paths
/// (`unload`/`clear` take them because a released pipeline belongs to a
/// backend), and enough to prove the headless backend refuses to build one.
struct Backend {
    pipeline: RenderPipeline,
    state: RenderState,
}

fn headless() -> Backend {
    let pipeline = RenderPipeline::null();
    let (_surface, state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    Backend { pipeline, state }
}

/// A minimal valid SPIR-V module: the five-word header, version 1.0.
const MODULE: [u8; 20] = [
    0x03, 0x02, 0x23, 0x07, // magic, little-endian
    0x00, 0x00, 0x01, 0x00, // version 1.0
    0x00, 0x00, 0x00, 0x00, // generator
    0x01, 0x00, 0x00, 0x00, // bound
    0x00, 0x00, 0x00, 0x00, // schema
];

/// A second valid module, so a stage's bytes can be told apart.
const MODULE_OTHER: [u8; 20] = [
    0x03, 0x02, 0x23, 0x07, 0x00, 0x00, 0x01, 0x00, 0x11, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

/// The bindings of a two-source program: source 1 and source 2.
const TWO_SOURCES: [BindingDesc; 2] = [
    BindingDesc::texture(1, SamplerKind::LinearClamp),
    BindingDesc::texture(2, SamplerKind::LinearClamp),
];

/// A two-source, one-`vec4` program named `name`.
fn program<'a>(name: &'a str) -> ProgramDesc<'a> {
    let mut desc = ProgramDesc::new(name, &MODULE, &MODULE);
    desc.bindings = &TWO_SOURCES;
    desc.uniform_words = 4;
    desc.opacity_uniform = Some(0);
    desc
}

#[test]
fn a_program_registers_and_reads_back() {
    let mut library = ShaderLibrary::new();
    assert!(library.is_empty(), "a new library is empty");

    let id = library.load(program("crossfade")).expect("register");
    assert_eq!(library.len(), 1);
    assert!(library.contains(id), "the issued id is registered");

    let loaded = library.program(id).expect("registered program");
    assert_eq!(loaded.id(), id);
    assert_eq!(loaded.name(), "crossfade");
    assert_eq!(loaded.vertex(), &MODULE);
    assert_eq!(loaded.fragment(), &MODULE);
    assert_eq!(loaded.vertex_layout(), VertexLayout::UnitQuad);
    assert_eq!(loaded.bindings(), &TWO_SOURCES);
    assert_eq!(loaded.uniform_words(), 4);
    assert_eq!(loaded.opacity_uniform(), Some(0));
}

#[test]
fn an_identical_program_registers_once() {
    let mut library = ShaderLibrary::new();
    let first = library.load(program("crossfade")).expect("register");
    let second = library.load(program("crossfade")).expect("register again");
    assert_eq!(first, second, "an identical descriptor returns the same id");
    assert_eq!(library.len(), 1, "registration is idempotent");
}

#[test]
fn a_changed_program_is_a_different_program() {
    let mut library = ShaderLibrary::new();
    let base = library.load(program("base")).expect("register");

    // Each descriptor changes exactly one field of the base program.
    let mut other_vertex = program("base");
    other_vertex.vertex = ShaderModule::vertex(&MODULE_OTHER);
    let mut other_fragment = program("base");
    other_fragment.fragment = ShaderModule::fragment(&MODULE_OTHER);
    let single_source = [BindingDesc::texture(1, SamplerKind::NearestClamp)];
    let mut fewer_bindings = program("base");
    fewer_bindings.bindings = &single_source;
    let mut more_uniforms = program("base");
    more_uniforms.uniform_words = 8;
    let mut no_opacity = program("base");
    no_opacity.opacity_uniform = None;

    for desc in [
        program("renamed"),
        other_vertex,
        other_fragment,
        fewer_bindings,
        more_uniforms,
        no_opacity,
    ] {
        let id = library.load(desc).expect("register a changed program");
        assert_ne!(id, base, "a changed field is a different program");
    }
    assert_eq!(library.len(), 7, "base plus six distinct programs");
}

#[test]
fn ids_are_stable_and_never_reused() {
    let mut library = ShaderLibrary::new();
    let first = library.load(program("crossfade")).expect("register");
    assert_eq!(
        library.load(program("crossfade")).expect("register again"),
        first
    );

    let mut backend = headless();
    let unloaded = library
        .unload(&backend.pipeline, &mut backend.state, first)
        .expect("unload");
    assert_eq!(unloaded.id(), first);
    assert_eq!(unloaded.name(), "crossfade");
    assert!(
        library.program(first).is_none(),
        "an unloaded id is no longer registered"
    );
    assert!(library.is_empty());

    let again = library.load(program("crossfade")).expect("register again");
    assert_ne!(again, first, "an id is never reused after unload");
    assert!(library.program(again).is_some());
}

#[test]
fn unloading_an_unknown_id_is_a_typed_error() {
    let mut library = ShaderLibrary::new();
    let mut backend = headless();
    let id = library.load(program("crossfade")).expect("register");
    library
        .unload(&backend.pipeline, &mut backend.state, id)
        .expect("first unload");
    let error = library
        .unload(&backend.pipeline, &mut backend.state, id)
        .expect_err("a second unload must fail");
    assert!(
        matches!(&error, ShaderError::UnknownShader(seen) if *seen == id),
        "unexpected error: {error}"
    );
    assert!(error.to_string().contains("unknown shader"));
}

#[test]
fn clearing_empties_the_library_without_reusing_ids() {
    let mut library = ShaderLibrary::new();
    let first = library.load(program("one")).expect("register");
    let second = library.load(program("two")).expect("register");
    assert_eq!(library.len(), 2);
    assert_eq!(
        library
            .programs()
            .map(LoadedProgram::name)
            .collect::<Vec<_>>(),
        vec!["one", "two"],
        "programs are listed in load order"
    );

    let mut backend = headless();
    library.clear(&backend.pipeline, &mut backend.state);
    assert!(library.is_empty());
    assert!(library.program(first).is_none());
    assert!(library.program(second).is_none());

    let again = library.load(program("one")).expect("register after clear");
    assert_ne!(again, first);
    assert_ne!(again, second);
}

#[test]
fn the_library_keeps_the_code_it_registered() {
    let mut module = MODULE;
    let mut library = ShaderLibrary::new();
    let id = {
        let desc = ProgramDesc::new("owned", &module, &MODULE);
        library.load(desc).expect("register")
    };
    module[8] = 0x33;
    assert_ne!(module.as_slice(), MODULE, "the caller's copy changed");
    let loaded = library.program(id).expect("registered program");
    assert_eq!(
        loaded.vertex(),
        &MODULE,
        "the registration owns a copy of the stage code"
    );
}

#[test]
fn registration_refuses_a_bad_stage_module() {
    let mut library = ShaderLibrary::new();

    let error = library
        .load(ProgramDesc::new("empty", &[], &MODULE))
        .expect_err("an empty module is refused");
    assert!(
        matches!(
            &error,
            ShaderError::Data(ShaderDataError::EmptyModule {
                stage: ShaderStage::Vertex
            })
        ),
        "unexpected error: {error}"
    );

    let mut odd = [0u8; 21];
    odd[..20].copy_from_slice(&MODULE);
    let error = library
        .load(ProgramDesc::new("odd", &MODULE, &odd))
        .expect_err("a misaligned module is refused");
    assert!(
        matches!(
            &error,
            ShaderError::Data(ShaderDataError::MisalignedModule {
                stage: ShaderStage::Fragment,
                len: 21
            })
        ),
        "unexpected error: {error}"
    );

    let mut bad_magic = MODULE;
    bad_magic[0] = 0xff;
    let error = library
        .load(ProgramDesc::new("magic", &MODULE, &bad_magic))
        .expect_err("a bad magic is refused");
    assert!(
        matches!(
            &error,
            ShaderError::Data(ShaderDataError::BadSpirVMagic {
                stage: ShaderStage::Fragment,
                ..
            })
        ),
        "unexpected error: {error}"
    );
    assert!(library.is_empty(), "a refused program is not registered");
}

#[test]
fn registration_refuses_an_unnamed_program() {
    let mut library = ShaderLibrary::new();
    let error = library
        .load(ProgramDesc::new("", &MODULE, &MODULE))
        .expect_err("a nameless program is refused");
    assert!(
        matches!(&error, ShaderError::Data(ShaderDataError::EmptyName)),
        "unexpected error: {error}"
    );
}

#[test]
fn registration_refuses_binding_budgets_and_duplicates() {
    let mut library = ShaderLibrary::new();

    let five = [
        BindingDesc::texture(1, SamplerKind::LinearClamp),
        BindingDesc::texture(2, SamplerKind::LinearClamp),
        BindingDesc::texture(3, SamplerKind::LinearClamp),
        BindingDesc::texture(4, SamplerKind::LinearClamp),
        BindingDesc::texture(5, SamplerKind::LinearClamp),
    ];
    let mut too_many = program("five");
    too_many.bindings = &five;
    let error = library
        .load(too_many)
        .expect_err("five bindings is too many");
    assert!(
        matches!(
            &error,
            ShaderError::Data(ShaderDataError::TooManyBindings {
                count: 5,
                max: MAX_EFFECT_TEXTURES
            })
        ),
        "unexpected error: {error}"
    );

    let repeated = [
        BindingDesc::texture(1, SamplerKind::LinearClamp),
        BindingDesc::texture(1, SamplerKind::NearestClamp),
    ];
    let mut duplicate = program("duplicate");
    duplicate.bindings = &repeated;
    let error = library
        .load(duplicate)
        .expect_err("a repeated binding index is refused");
    assert!(
        matches!(
            &error,
            ShaderError::Data(ShaderDataError::DuplicateBinding { binding: 1 })
        ),
        "unexpected error: {error}"
    );

    // The widest legal binding set is accepted, not truncated.
    let mut widest = program("widest");
    widest.bindings = &five[..MAX_EFFECT_TEXTURES];
    library.load(widest).expect("four bindings is the limit");
}

#[test]
fn registration_refuses_uniform_budgets_and_alignment() {
    let mut library = ShaderLibrary::new();

    let mut too_many = program("many-words");
    too_many.uniform_words = (MAX_EFFECT_UNIFORMS + 4) as u32;
    let error = library
        .load(too_many)
        .expect_err("a uniform block over budget is refused");
    assert!(
        matches!(
            &error,
            ShaderError::Data(ShaderDataError::UniformOverflow {
                max: MAX_EFFECT_UNIFORMS,
                ..
            })
        ),
        "unexpected error: {error}"
    );

    let mut unaligned = program("unaligned");
    unaligned.uniform_words = 6;
    let error = library
        .load(unaligned)
        .expect_err("a half-vec4 block is refused");
    assert!(
        matches!(
            &error,
            ShaderError::Data(ShaderDataError::UniformLayout { words: 6 })
        ),
        "unexpected error: {error}"
    );

    // The declared budget itself is accepted.
    let mut full = program("full-words");
    full.uniform_words = MAX_EFFECT_UNIFORMS as u32;
    full.opacity_uniform = Some(MAX_EFFECT_UNIFORMS as u32 - 1);
    library.load(full).expect("the budget is usable");
}

#[test]
fn the_uniform_block_checks_its_bounds_and_alignment() {
    let block = EffectUniforms::from_words(&[0.25, 0.5, 0.75, 1.0]).expect("a vec4 block");
    assert_eq!(block.len(), 4);
    assert_eq!(block.byte_len(), 16);
    assert_eq!(block.words(), &[0.25, 0.5, 0.75, 1.0]);

    assert_eq!(
        EffectUniforms::from_words(&[0.0, 1.0]),
        Err(ShaderDataError::UniformLayout { words: 2 })
    );
    let too_many = [0.0f32; MAX_EFFECT_UNIFORMS + 4];
    assert_eq!(
        EffectUniforms::from_words(&too_many),
        Err(ShaderDataError::UniformOverflow {
            words: MAX_EFFECT_UNIFORMS + 4,
            max: MAX_EFFECT_UNIFORMS
        })
    );
    assert!(EffectUniforms::new().is_empty());
    assert_eq!(EffectUniforms::from_words(&[]), Ok(EffectUniforms::new()));
}

#[test]
fn the_opacity_word_must_lie_inside_the_block() {
    let mut library = ShaderLibrary::new();
    let mut desc = program("opacity");
    desc.uniform_words = 4;
    desc.opacity_uniform = Some(4);
    let error = library
        .load(desc)
        .expect_err("an opacity word outside the block is refused");
    assert!(
        matches!(
            &error,
            ShaderError::Data(ShaderDataError::OpacityUniformOutOfRange { index: 4, words: 4 })
        ),
        "unexpected error: {error}"
    );

    let mut none = program("no-opacity");
    none.opacity_uniform = None;
    library
        .load(none)
        .expect("a program may read no composed opacity");
}

#[test]
fn a_registered_program_yields_a_pipeline_descriptor() {
    let mut library = ShaderLibrary::new();
    let id = library.load(program("crossfade")).expect("register");
    let loaded = library.program(id).expect("registered program");

    let desc = loaded.pipeline_desc(BlendMode::Additive);
    desc.validate().expect("the registered program is valid");
    assert_eq!(desc.name, "crossfade");
    assert_eq!(desc.vertex.stage, ShaderStage::Vertex);
    assert_eq!(desc.fragment.stage, ShaderStage::Fragment);
    assert_eq!(desc.vertex.code.bytes(), &MODULE);
    assert_eq!(desc.fragment.code.bytes(), &MODULE);
    assert_eq!(desc.vertex_layout, VertexLayout::UnitQuad);
    assert_eq!(desc.blend, BlendMode::Additive);
    assert_eq!(desc.bindings, &TWO_SOURCES);
    assert_eq!(desc.uniform_words, 4);
    assert_eq!(desc.opacity_uniform, Some(0));
}

#[test]
fn a_descriptor_maps_to_its_pipeline_descriptor() {
    let desc = program("direct");
    let pipeline = desc.pipeline_desc(BlendMode::SourceOver);
    assert_eq!(pipeline.name, desc.name);
    assert_eq!(pipeline.bindings, desc.bindings);
    assert_eq!(pipeline.uniform_words, desc.uniform_words);
    assert_eq!(pipeline.opacity_uniform, desc.opacity_uniform);
    pipeline
        .validate()
        .expect("a valid program is a valid pipeline");

    // Validation is the same rule on both forms of the descriptor.
    let mut broken = program("broken");
    broken.uniform_words = 6;
    assert_eq!(
        broken.validate(),
        broken.pipeline_desc(BlendMode::SourceOver).validate(),
        "the program and pipeline descriptors share one validation rule"
    );
}

#[test]
fn the_shader_surface_is_reachable_through_one_module() {
    // The library re-exports the data half of the contract, so a caller never
    // has to name the shared DTO crate to describe a program.
    let binding: engine::shader::BindingDesc = BindingDesc::texture(1, SamplerKind::LinearRepeat);
    assert_eq!(binding.sampler, SamplerKind::LinearRepeat);
    assert_eq!(
        engine::shader::MAX_EFFECT_TEXTURES,
        MAX_EFFECT_TEXTURES,
        "the re-exported budget is the contract's"
    );
    let uniforms = engine::shader::EffectUniforms::from_words(&[0.0; 4]).expect("uniforms");
    assert_eq!(uniforms.byte_len(), 16);
    let module: engine::shader::ShaderModule<'_> = ShaderModule::fragment(&MODULE);
    assert_eq!(module.stage, engine::shader::ShaderStage::Fragment);
}

/// The headless backend reports that it runs no effect pipelines and refuses to
/// build one, so a caller can keep its own CPU path instead of drawing nothing.
#[test]
fn the_headless_backend_refuses_to_build_a_pipeline() {
    let mut library = ShaderLibrary::new();
    let mut backend = headless();
    let id = library.load(program("crossfade")).expect("register");

    let error = library
        .pipeline(
            &backend.pipeline,
            &mut backend.state,
            id,
            BlendMode::SourceOver,
        )
        .expect_err("the headless backend has no pipelines");
    assert!(
        matches!(&error, ShaderError::Unsupported),
        "unexpected error: {error}"
    );
    assert_eq!(library.pipeline_count(), 0);
    assert_eq!(library.pipelines().count(), 0);

    // An id this library no longer holds is a typed miss before the backend is
    // asked.
    library
        .unload(&backend.pipeline, &mut backend.state, id)
        .expect("unload the program");
    let error = library
        .pipeline(
            &backend.pipeline,
            &mut backend.state,
            id,
            BlendMode::SourceOver,
        )
        .expect_err("an unloaded id is a miss");
    assert!(
        matches!(&error, ShaderError::UnknownShader(seen) if *seen == id),
        "unexpected error: {error}"
    );
}

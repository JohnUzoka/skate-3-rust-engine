//! Naga validation for the retail shaders, with no GPU and no game app.
//!
//! Worth having because the interesting failure modes here are static: a slot
//! record read under non-uniform control flow, a binding number that drifted out
//! of step with `AsBindGroup`, or a struct whose WGSL layout stopped matching the
//! bytes Rust encodes. All of those are compile errors that Naga will name, and
//! none of them need a window.
//!
//! Bevy's own vertex interfaces are used for real, from the vendored sources.
//! Lighting resources and functions the retail shaders only *call* are interface
//! fixtures — the point is to type-check our code, not re-validate Bevy's.
use naga_oil::compose::{
    ComposableModuleDescriptor, Composer, NagaModuleDescriptor, ShaderDefValue,
};
use std::collections::HashMap;

/// Each world page's size in texels, which `WorldMaterial::specialize` sets with
/// `WORLD_EXPLICIT_LOD` (University's).
const PAGE_SIZES: [(u32, u32); 8] = [
    (256, 256),
    (16, 16),
    (512, 512),
    (1024, 1024),
    (128, 128),
    (128, 256),
    (256, 128),
    (512, 256),
];

/// Composes one of our shaders against the Bevy interface and validates it.
fn validate(source: &str, extras: &[&str]) -> naga::Module {
    validate_with(source, extras, naga::valid::Capabilities::all())
}

/// `validate`, granting naga only `capabilities`, as wgpu grants them by the device's features.
fn validate_with(
    source: &str,
    extras: &[&str],
    capabilities: naga::valid::Capabilities,
) -> naga::Module {
    validate_numbered(source, extras, &[], capabilities)
}

/// `validate_with`, also setting the numeric defs in `numbers`, as `WorldMaterial::specialize`
/// sets `WORLD_FEATURES`.
fn validate_numbered(
    source: &str,
    extras: &[&str],
    numbers: &[(&str, u32)],
    capabilities: naga::valid::Capabilities,
) -> naga::Module {
    let mut defs = HashMap::from([("MATERIAL_BIND_GROUP".into(), ShaderDefValue::UInt(3))]);
    for &(name, value) in numbers {
        defs.insert(name.into(), ShaderDefValue::UInt(value));
    }
    if extras.contains(&"WORLD_EXPLICIT_LOD") {
        for (page, (width, height)) in PAGE_SIZES.into_iter().enumerate() {
            defs.insert(format!("WORLD_PAGE_{page}_W"), ShaderDefValue::UInt(width));
            defs.insert(format!("WORLD_PAGE_{page}_H"), ShaderDefValue::UInt(height));
        }
    }
    for &name in [
        // Bevy's mesh pipeline sets this for every mesh with positions
        // (bevy_pbr mesh.rs); forward_io's Vertex only declares it then.
        "VERTEX_POSITIONS",
        "VERTEX_UVS_A",
        "VERTEX_UVS_B",
        "VERTEX_TANGENTS",
        "VERTEX_COLORS",
        "VERTEX_NORMALS",
        "VERTEX_OUTPUT_INSTANCE_INDEX",
    ]
    .iter()
    .chain(extras)
    {
        defs.insert(name.into(), ShaderDefValue::Bool(true));
    }
    let mut composer = Composer::default().with_capabilities(capabilities);
    let fixtures = [
        (
            "fullscreen",
            include_str!("../../../vendor/bevy_core_pipeline/src/fullscreen_vertex_shader/fullscreen.wgsl").to_string(),
        ),
        (
            "forward",
            include_str!("../../../vendor/bevy_pbr/src/render/forward_io.wgsl").to_string(),
        ),
        (
            "prepass",
            include_str!("../../../vendor/bevy_pbr/src/prepass/prepass_io.wgsl").to_string(),
        ),
        (
            "mesh",
            "#define_import_path bevy_pbr::mesh_bindings\n\
             struct Mesh { material_and_lightmap_bind_group_slot: u32, tag: u32 }\n\
             @group(2) @binding(0) var<storage> mesh: array<Mesh>;"
                .to_string(),
        ),
        (
            "frame",
            "#define_import_path bevy_pbr::mesh_view_bindings\n\
             struct View {view_from_world:mat4x4<f32>,clip_from_world:mat4x4<f32>,viewport:vec4<f32>,world_position:vec3<f32>,padding:f32}\n\
             struct Light {direction_to_light:vec3<f32>,flags:u32}\n\
             struct Lights {n_directional_lights:u32,directional_lights:array<Light,10>}\n\
             @group(0) @binding(0) var<uniform> view:View;\n\
             @group(0) @binding(1) var<storage> lights:Lights;"
                .to_string(),
        ),
        (
            "shadows",
            "#define_import_path bevy_pbr::shadows\n\
             fn fetch_directional_shadow(id:u32,p:vec4<f32>,n:vec3<f32>,z:f32)->f32 {return 1.0;}"
                .to_string(),
        ),
        (
            "motion",
            "#define_import_path bevy_pbr::pbr_prepass_functions\n\
             fn calculate_motion_vector(p:vec4<f32>,q:vec4<f32>)->vec2<f32> {return vec2<f32>(0.0);}"
                .to_string(),
        ),
        (
            "transformations",
            "#define_import_path bevy_pbr::view_transformations\n\
             fn position_world_to_clip(p:vec3<f32>)->vec4<f32> {return vec4<f32>(p,1.0);}\n\
             fn frag_coord_to_ndc(f:vec4<f32>)->vec3<f32> {return f.xyz;}"
                .to_string(),
        ),
        (
            "mesh_functions",
            "#define_import_path bevy_pbr::mesh_functions\n\
             fn get_world_from_local(i:u32)->mat4x4<f32> {return mat4x4<f32>();}\n\
             fn mesh_position_local_to_world(m:mat4x4<f32>,p:vec4<f32>)->vec4<f32> {return m*p;}\n\
             fn mesh_normal_local_to_world(n:vec3<f32>,i:u32)->vec3<f32> {return n;}\n\
             fn mesh_tangent_local_to_world(m:mat4x4<f32>,t:vec4<f32>,i:u32)->vec4<f32> {return t;}"
                .to_string(),
        ),
        (
            "bindings",
            include_str!("retail_material_bindings.wgsl").to_string(),
        ),
        (
            "character_common",
            include_str!("retail_character_common.wgsl").to_string(),
        ),
    ];
    for (path, source) in fixtures {
        if let Err(error) = composer.add_composable_module(ComposableModuleDescriptor {
            source: &source,
            file_path: path,
            shader_defs: defs.clone(),
            ..Default::default()
        }) {
            panic!("{}", error.emit_to_string(&composer));
        }
    }
    let module = composer
        .make_naga_module(NagaModuleDescriptor {
            source,
            file_path: "retail",
            shader_defs: defs,
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&composer)));
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), capabilities)
        .validate(&module)
        .unwrap();
    module
}

/// The world shader is the one at risk: `slot` is a per-vertex attribute, so
/// every family branch is non-uniform control flow and implicit-derivative
/// sampling inside one is invalid WGSL. If this passes, the explicit gradients
/// are complete. `WorldMaterial::specialize` sets each variant def on its own,
/// so every combination of them is a pipeline the game can build, with either
/// probe or neither. It never gives `WORLD_FETCH` with `WORLD_PACKED_VARYINGS`,
/// whose varyings the fetch variant replaces, but the pair must still compose.
/// Under `WORLD_PAGE_ARRAY` the page index into the array of pages is
/// non-uniform too. naga accepts that only with its
/// `SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING` capability,
/// which wgpu grants with the adapter feature of the same name; this validator
/// has every capability.
#[test]
fn world_shader_validates_under_non_uniform_material_slots() {
    const DEFS: [&str; 7] = [
        "WORLD_ALPHA_CUTOFF",
        "WORLD_WATER",
        "WORLD_HOIST",
        "WORLD_PACKED_VARYINGS",
        "WORLD_EXPLICIT_LOD",
        "WORLD_FETCH",
        "WORLD_PAGE_ARRAY",
    ];
    const PROBES: [Option<&str>; 6] = [
        None,
        Some("WORLD_PROBE_VARYINGS"),
        Some("WORLD_PROBE_GEOMETRY"),
        Some("WORLD_PROBE_TEXELS"),
        Some("WORLD_PROBE_NO_TEXTURES"),
        Some("WORLD_PROBE_ONE_PAGE"),
    ];
    for probe in PROBES {
        for mask in 0..1u32 << DEFS.len() {
            let extras: Vec<&str> = DEFS
                .iter()
                .enumerate()
                .filter(|(bit, _)| mask & 1 << bit != 0)
                .map(|(_, name)| *name)
                .chain(probe)
                .collect();
            validate(include_str!("retail_world.wgsl"), &extras);
        }
    }
}

/// `SKATE_WORLD_FEATURES` gives the main pass `WORLD_FEATURES`, a mask of the fragment's parts
/// (retail_render.rs `world_features`), on top of the other defs. A class's mask can be any
/// combination of parts, so every mask must compose, with the water families and without, and
/// under `SKATE_WORLD_SLOT_WATERFALL`, `SKATE_WORLD_SHORT_LIVES`, `SKATE_WORLD_LEAN` and
/// `SKATE_WORLD_IMPLICIT`, the last never with the water families (`WorldMaterial::specialize`).
#[test]
fn world_shader_validates_under_every_feature_mask() {
    const EXTRAS: [&[&str]; 9] = [
        &[],
        &[
            "WORLD_ALPHA_CUTOFF",
            "WORLD_WATER",
            "WORLD_PACKED_VARYINGS",
            "WORLD_PAGE_ARRAY",
            "WORLD_PAGE_UNIFORM",
        ],
        &["WORLD_HOIST", "WORLD_EXPLICIT_LOD", "WORLD_FETCH"],
        &[
            "WORLD_ALPHA_CUTOFF",
            "WORLD_WATER",
            "WORLD_PACKED_VARYINGS",
            "WORLD_PAGE_ARRAY",
            "WORLD_PAGE_UNIFORM",
            "WORLD_SLOT_WATERFALL",
        ],
        &["WORLD_SHORT_LIVES"],
        &["WORLD_HOIST", "WORLD_EXPLICIT_LOD", "WORLD_FETCH", "WORLD_SHORT_LIVES"],
        &[
            "WORLD_ALPHA_CUTOFF",
            "WORLD_WATER",
            "WORLD_PACKED_VARYINGS",
            "WORLD_PAGE_ARRAY",
            "WORLD_PAGE_UNIFORM",
            "WORLD_SLOT_WATERFALL",
            "WORLD_SHORT_LIVES",
        ],
        &[
            "WORLD_ALPHA_CUTOFF",
            "WORLD_WATER",
            "WORLD_PACKED_VARYINGS",
            "WORLD_PAGE_ARRAY",
            "WORLD_PAGE_UNIFORM",
            "WORLD_SLOT_WATERFALL",
            "WORLD_SHORT_LIVES",
            "WORLD_LEAN",
        ],
        &[
            "WORLD_PACKED_VARYINGS",
            "WORLD_PAGE_ARRAY",
            "WORLD_PAGE_UNIFORM",
            "WORLD_SLOT_WATERFALL",
            "WORLD_SHORT_LIVES",
            "WORLD_LEAN",
            "WORLD_IMPLICIT_LOD",
        ],
    ];
    for extras in EXTRAS {
        for features in 0..=super::world_features::ALL {
            validate_numbered(
                include_str!("retail_world.wgsl"),
                extras,
                &[("WORLD_FEATURES", features)],
                naga::valid::Capabilities::all(),
            );
        }
    }
}

/// `FrameStateData::encode` writes the bytes the shaders read as `FrameState`.
/// A field added on one side only reads zeros or the next field's values, which
/// nothing reports, so pin the WGSL layout to the encoded order and length.
#[test]
fn frame_state_layout_matches_the_encoded_bytes() {
    let module = validate(include_str!("retail_world.wgsl"), &[]);
    let (members, span) = module
        .types
        .iter()
        .find_map(|(_, ty)| match &ty.inner {
            naga::TypeInner::Struct { members, span }
                if ty.name.as_deref().is_some_and(|name| name.contains("FrameState")) =>
            {
                Some((members, *span))
            }
            _ => None,
        })
        .expect("the world shader reads FrameState");
    let offsets: Vec<(&str, u32)> = members
        .iter()
        .map(|member| (member.name.as_deref().unwrap_or_default(), member.offset))
        .collect();
    assert_eq!(offsets, [("shadow", 0), ("clock", 16), ("pca", 32), ("casters", 144)]);
    assert_eq!(span as usize, super::FrameStateData::SIZE);
}

/// The dome is one draw with no material table, so what is worth checking is the
/// binding numbers: they are written out by hand here and have to stay in step
/// with `SkyMaterial`'s `AsBindGroup` derive.
#[test]
fn sky_shader_validates() {
    validate(include_str!("retail_sky.wgsl"), &[]);
}

/// A presentation pass upscale (graphics_menu.rs `Upscaler` and `Sharpener`), against Bevy UI's
/// own vertex output: its bindings are written by hand and have to stay in step with the
/// material's `AsBindGroup` derive.
fn validate_ui_material(source: &str, file_path: &str) {
    let mut composer = Composer::default();
    if let Err(error) = composer.add_composable_module(ComposableModuleDescriptor {
        source: include_str!("../../../vendor/bevy_ui_render/src/ui_vertex_output.wgsl"),
        file_path: "ui_vertex_output.wgsl",
        ..Default::default()
    }) {
        panic!("{}", error.emit_to_string(&composer));
    }
    let module = composer
        .make_naga_module(NagaModuleDescriptor { source, file_path, ..Default::default() })
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&composer)));
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .unwrap_or_else(|e| panic!("{file_path} doesn't validate: {e:?}"));
}

#[test]
fn fsr_easu_shader_validates() {
    validate_ui_material(include_str!("fsr_easu.wgsl"), "fsr_easu.wgsl");
}

#[test]
fn fsr_rcas_shader_validates() {
    validate_ui_material(include_str!("fsr_rcas.wgsl"), "fsr_rcas.wgsl");
}

/// The depth pipelines are where a second vertex layout is in play, and a
/// mismatch there aborts pipeline creation rather than degrading. Cover each
/// combination Bevy and `specialize` can ask for: cutting classes sample a page,
/// directional cascades emulate unclipped depth on adapters without depth clip
/// control, and the trimmed layouts keep the UV and material index only for the
/// alpha test. Under `SKATE_VIS_CULL` the view prepass marks triangles, after the
/// depth test where there's no alpha test, after the alpha test under
/// `SKATE_VIS_CULL_AFTER_TEST`, and never emulates unclipped depth.
#[test]
fn depth_shader_validates_in_every_shadow_configuration() {
    for trim in [&[][..], &["WORLD_DEPTH_TRIM"], &["WORLD_DEPTH_TRIM", "WORLD_DEPTH_POSITION_ONLY"]] {
        for unclipped in [&[][..], &["UNCLIPPED_DEPTH_ORTHO_EMULATION"]] {
            for fragment in [
                &[][..],
                &["WORLD_ALPHA_CUTOFF"],
                &["WORLD_EXPLICIT_LOD"],
                &["WORLD_ALPHA_CUTOFF", "WORLD_EXPLICIT_LOD"],
            ] {
                if trim.contains(&"WORLD_DEPTH_POSITION_ONLY") && fragment.contains(&"WORLD_ALPHA_CUTOFF") {
                    continue;
                }
                for pages in [&[][..], &["WORLD_PAGE_ARRAY"]] {
                    for mark in [
                        &[][..],
                        &["WORLD_MARK"],
                        &["WORLD_MARK", "WORLD_MARK_EARLY"],
                        &["WORLD_MARK", "WORLD_MARK_AFTER_TEST"],
                    ] {
                        if !mark.is_empty() && !unclipped.is_empty()
                            || mark.contains(&"WORLD_MARK_EARLY")
                                && fragment.contains(&"WORLD_ALPHA_CUTOFF")
                            || mark.contains(&"WORLD_MARK_AFTER_TEST")
                                && !fragment.contains(&"WORLD_ALPHA_CUTOFF")
                        {
                            continue;
                        }
                        validate(
                            include_str!("retail_depth.wgsl"),
                            &[trim, unclipped, fragment, pages, mark].concat(),
                        );
                    }
                }
            }
        }
    }
}

/// `SKATE_WORLD_PAGE_UNIFORM`: under `WORLD_PAGE_UNIFORM` each page lookup first
/// tries its subgroup's first page (retail_material_bindings.wgsl `sample_page`).
/// wgpu grants subgroup operations in fragment stages with `Features::SUBGROUP`,
/// and in vertex stages only with `SUBGROUP_VERTEX`, which `specialize` doesn't
/// ask for: it gives the def to fragment stages alone. Here both stages have it,
/// and the vertex stage's subgroups are withheld, so no vertex stage may reach a
/// lookup. Covers the main pass's variants and probes and the depth pipelines'
/// alpha test.
#[test]
fn page_uniform_needs_subgroups_in_fragment_stages_only() {
    let capabilities = naga::valid::Capabilities::all()
        - naga::valid::Capabilities::SUBGROUP_VERTEX_STAGE;
    let uniform = ["WORLD_PAGE_ARRAY", "WORLD_PAGE_UNIFORM"];
    for lod in [&[][..], &["WORLD_EXPLICIT_LOD"]] {
        for cutoff in [&[][..], &["WORLD_ALPHA_CUTOFF"]] {
            for varyings in [&["WORLD_PACKED_VARYINGS"][..], &["WORLD_FETCH"], &[]] {
                for probe in [
                    &[][..],
                    &["WORLD_PROBE_VARYINGS"],
                    &["WORLD_PROBE_GEOMETRY"],
                    &["WORLD_PROBE_TEXELS"],
                    &["WORLD_PROBE_NO_TEXTURES"],
                    &["WORLD_PROBE_ONE_PAGE"],
                ] {
                    validate_with(
                        include_str!("retail_world.wgsl"),
                        &[&uniform[..], lod, cutoff, varyings, probe].concat(),
                        capabilities,
                    );
                }
            }
            for trim in [&[][..], &["WORLD_DEPTH_TRIM"]] {
                validate_with(
                    include_str!("retail_depth.wgsl"),
                    &[&uniform[..], lod, cutoff, trim].concat(),
                    capabilities,
                );
            }
        }
    }
}

/// `SKATE_WORLD_SLOT_WATERFALL`: under `WORLD_SLOT_WATERFALL` the main pass shades
/// one material at a time, the subgroup's first (retail_world.wgsl `fragment`).
/// `specialize` gives the def to the main pass's fragment stage alone, so, as for
/// `WORLD_PAGE_UNIFORM`, the vertex stage's subgroups are withheld. Covers the
/// main pass's variants and probes, with each way of binding the pages.
#[test]
fn slot_waterfall_needs_subgroups_in_fragment_stages_only() {
    let capabilities = naga::valid::Capabilities::all()
        - naga::valid::Capabilities::SUBGROUP_VERTEX_STAGE;
    for pages in [&[][..], &["WORLD_PAGE_ARRAY"], &["WORLD_PAGE_ARRAY", "WORLD_PAGE_UNIFORM"]] {
        for lod in [&[][..], &["WORLD_EXPLICIT_LOD", "WORLD_HOIST"]] {
            for cutoff in [&[][..], &["WORLD_ALPHA_CUTOFF"]] {
                for varyings in [&["WORLD_PACKED_VARYINGS"][..], &["WORLD_FETCH"], &[]] {
                    for probe in [
                        &[][..],
                        &["WORLD_PROBE_VARYINGS"],
                        &["WORLD_PROBE_GEOMETRY"],
                        &["WORLD_PROBE_TEXELS"],
                        &["WORLD_PROBE_NO_TEXTURES"],
                        &["WORLD_PROBE_ONE_PAGE"],
                        &["WORLD_PROBE_ONE_PASS"],
                    ] {
                        validate_with(
                            include_str!("retail_world.wgsl"),
                            &[&["WORLD_SLOT_WATERFALL"][..], pages, lod, cutoff, varyings, probe]
                                .concat(),
                            capabilities,
                        );
                    }
                }
            }
        }
    }
}

/// `SKATE_WORLD_GPR48` renames the world's one fragment entry point, and only that: the stage's
/// inputs are those of `fragment`, so the same vertex stage feeds it.
#[test]
fn gpr48_renames_the_world_fragment_alone() {
    for varyings in [&["WORLD_PACKED_VARYINGS"][..], &["WORLD_FETCH"], &[]] {
        let defs = [&["WORLD_PAGE_ARRAY", "WORLD_SLOT_WATERFALL"][..], varyings].concat();
        let plain = validate(include_str!("retail_world.wgsl"), &defs);
        let capped = validate(
            include_str!("retail_world.wgsl"),
            &[&defs[..], &["WORLD_GPR48"]].concat(),
        );
        let fragments = |module: &naga::Module| -> Vec<String> {
            module
                .entry_points
                .iter()
                .filter(|e| e.stage == naga::ShaderStage::Fragment)
                .map(|e| e.name.clone())
                .collect()
        };
        assert_eq!(fragments(&plain), ["fragment"]);
        assert_eq!(fragments(&capped), ["fragment_gpr48"]);
        assert_eq!(
            stage_interface(&capped, naga::ShaderStage::Fragment),
            stage_interface(&plain, naga::ShaderStage::Fragment),
            "under {varyings:?}"
        );
    }
}

/// What a texture lookup in `function` reads: the dimension of the image, or of each image in
/// the binding array, that `image` names.
fn image_dim(
    module: &naga::Module,
    function: &naga::Function,
    image: naga::Handle<naga::Expression>,
) -> Option<naga::ImageDimension> {
    let mut expression = image;
    loop {
        match function.expressions[expression] {
            naga::Expression::Access { base, .. } | naga::Expression::AccessIndex { base, .. } => {
                expression = base;
            }
            naga::Expression::GlobalVariable(global) => {
                let mut ty = module.global_variables[global].ty;
                if let naga::TypeInner::BindingArray { base, .. } = module.types[ty].inner {
                    ty = base;
                }
                return match module.types[ty].inner {
                    naga::TypeInner::Image { dim, .. } => Some(dim),
                    _ => None,
                };
            }
            _ => return None,
        }
    }
}

/// `module`'s texture lookups that take their level of detail from the quad, and those that
/// take gradients, each by the dimension of the image they read.
fn lookups_by_level(
    module: &naga::Module,
) -> (Vec<Option<naga::ImageDimension>>, Vec<Option<naga::ImageDimension>>) {
    let functions = module
        .functions
        .iter()
        .map(|(_, function)| function)
        .chain(module.entry_points.iter().map(|entry| &entry.function));
    let (mut implicit, mut gradient) = (Vec::new(), Vec::new());
    for function in functions {
        for (_, expression) in function.expressions.iter() {
            if let naga::Expression::ImageSample { image, level, .. } = expression {
                match level {
                    naga::SampleLevel::Auto => implicit.push(image_dim(module, function, *image)),
                    naga::SampleLevel::Gradient { .. } => {
                        gradient.push(image_dim(module, function, *image))
                    }
                    _ => {}
                }
            }
        }
    }
    (implicit, gradient)
}

/// `SKATE_WORLD_LEAN` and `SKATE_WORLD_IMPLICIT` give the main pass `WORLD_LEAN` and
/// `WORLD_IMPLICIT_LOD`, never the depth pipelines (`WorldMaterial::specialize`), and the second
/// never the water families. Covers each way of binding the pages, with the fragment stage's
/// subgroups alone, as the main pass has them. Under `WORLD_IMPLICIT_LOD` every page lookup
/// takes its level from the quad, and only the environment's cube keeps its gradients; without
/// it, none does.
#[test]
fn lean_and_implicit_lod_reach_the_main_pass_lookups() {
    let capabilities = naga::valid::Capabilities::all()
        - naga::valid::Capabilities::SUBGROUP_VERTEX_STAGE;
    let cube = Some(naga::ImageDimension::Cube);
    let page = Some(naga::ImageDimension::D2);
    for pages in [&[][..], &["WORLD_PAGE_ARRAY"], &["WORLD_PAGE_ARRAY", "WORLD_PAGE_UNIFORM"]] {
        for lives in [&[][..], &["WORLD_SHORT_LIVES"]] {
            for waterfall in [&[][..], &["WORLD_SLOT_WATERFALL"]] {
                for cutoff in [&[][..], &["WORLD_ALPHA_CUTOFF"]] {
                    let defs = [&["WORLD_PACKED_VARYINGS"][..], pages, lives, waterfall, cutoff]
                        .concat();
                    let source = include_str!("retail_world.wgsl");
                    validate_with(
                        source,
                        &[&defs[..], &["WORLD_LEAN", "WORLD_WATER"]].concat(),
                        capabilities,
                    );
                    let lean = validate_with(
                        source,
                        &[&defs[..], &["WORLD_LEAN"]].concat(),
                        capabilities,
                    );
                    let (implicit, gradient) = lookups_by_level(&lean);
                    assert!(implicit.is_empty(), "under {defs:?}");
                    assert!(gradient.contains(&page), "under {defs:?}");
                    for extra in [&["WORLD_IMPLICIT_LOD"][..], &["WORLD_LEAN", "WORLD_IMPLICIT_LOD"]]
                    {
                        let module =
                            validate_with(source, &[&defs[..], extra].concat(), capabilities);
                        let (implicit, gradient) = lookups_by_level(&module);
                        assert!(implicit.contains(&page), "under {defs:?} and {extra:?}");
                        assert!(implicit.iter().all(|dim| *dim == page), "under {defs:?}");
                        assert!(gradient.iter().all(|dim| *dim == cube), "under {defs:?}");
                    }
                }
            }
        }
    }
}

/// Each vertex input location of a shader's `vertex` entry point, against a
/// description of the type it expects to receive there.
fn vertex_locations(module: &naga::Module) -> HashMap<u32, String> {
    let entry = module
        .entry_points
        .iter()
        .find(|e| e.name == "vertex")
        .expect("shader must own its vertex entry point");
    let mut found = HashMap::new();
    for argument in &entry.function.arguments {
        if let Some(naga::Binding::Location { location, .. }) = argument.binding {
            found.insert(location, format!("{:?}", module.types[argument.ty].inner));
        } else if let naga::TypeInner::Struct { members, .. } = &module.types[argument.ty].inner {
            for member in members {
                if let Some(naga::Binding::Location { location, .. }) = member.binding {
                    found.insert(location, format!("{:?}", module.types[member.ty].inner));
                }
            }
        }
    }
    found
}

/// The depth pipelines read the same vertex buffer as the main pass, so the two
/// shaders have to agree with `WorldMaterial::specialize` about which attribute
/// each location carries. Disagreeing is what made `prepass_pipeline` fail to
/// build: Bevy's own prepass shaders put `COLOR` on location 7. A trimmed layout
/// declares only some of them, and wgpu builds no pipeline whose vertex stage
/// reads a location its layout lacks, so each variant must read exactly the
/// locations `specialize` declares for it.
#[test]
fn depth_shader_vertex_locations_match_the_world_shader() {
    let world = vertex_locations(&validate(include_str!("retail_world.wgsl"), &[]));
    for (extras, declared) in [
        (&[][..], &[0, 1, 2, 3, 4, 5, 6][..]),
        (&["WORLD_DEPTH_TRIM"], &[0, 2, 5]),
        (&["WORLD_DEPTH_TRIM", "WORLD_DEPTH_POSITION_ONLY"], &[0]),
    ] {
        let depth = vertex_locations(&validate(include_str!("retail_depth.wgsl"), extras));
        for (location, ty) in &depth {
            assert_eq!(
                world.get(location),
                Some(ty),
                "location {location} differs from the world shader's vertex layout under {extras:?}"
            );
        }
        let mut read: Vec<u32> = depth.into_keys().collect();
        read.sort_unstable();
        assert_eq!(read, declared, "the vertex stage under {extras:?}");
    }
}

/// The locations one stage hands the next: a vertex stage's outputs or a
/// fragment stage's inputs, each with its type and interpolation.
fn stage_interface(module: &naga::Module, stage: naga::ShaderStage) -> HashMap<u32, String> {
    let entry = module
        .entry_points
        .iter()
        .find(|e| e.stage == stage)
        .expect("shader must own the stage's entry point");
    let mut pending: Vec<(Option<&naga::Binding>, naga::Handle<naga::Type>)> = match stage {
        naga::ShaderStage::Vertex => {
            entry.function.result.iter().map(|r| (r.binding.as_ref(), r.ty)).collect()
        }
        _ => entry.function.arguments.iter().map(|a| (a.binding.as_ref(), a.ty)).collect(),
    };
    let mut found = HashMap::new();
    while let Some((binding, ty)) = pending.pop() {
        match (binding, &module.types[ty].inner) {
            (Some(naga::Binding::Location { location, interpolation, sampling, .. }), inner) => {
                found.insert(*location, format!("{inner:?} {interpolation:?} {sampling:?}"));
            }
            (None, naga::TypeInner::Struct { members, .. }) => {
                pending.extend(members.iter().map(|m| (m.binding.as_ref(), m.ty)));
            }
            _ => {}
        }
    }
    found
}

/// wgpu builds a pipeline only if each location its fragment stage reads is one
/// its vertex stage writes, with the same type and interpolation.
/// `WorldMaterial::specialize` gives both stages the defs that shape what passes
/// between them, and the fragment stage more of its own. Each case is a pipeline
/// it can build: the shader, the defs both stages get, those only the fragment
/// stage gets, and how many locations that stage reads.
#[test]
fn fragment_inputs_are_vertex_outputs_in_every_world_pipeline() {
    const FRAGMENT: &[&str] =
        &["WORLD_ALPHA_CUTOFF", "WORLD_WATER", "WORLD_HOIST", "WORLD_EXPLICIT_LOD"];
    let world = include_str!("retail_world.wgsl");
    let depth = include_str!("retail_depth.wgsl");
    let cases: [(&str, &[&str], &[&str], usize); 16] = [
        (world, &[], FRAGMENT, 7),
        (world, &["WORLD_PACKED_VARYINGS"], FRAGMENT, 5),
        (world, &[], &["WORLD_PROBE_VARYINGS"], 7),
        (world, &["WORLD_PACKED_VARYINGS"], &["WORLD_PROBE_VARYINGS"], 5),
        (world, &["WORLD_PROBE_GEOMETRY"], FRAGMENT, 0),
        (world, &["WORLD_FETCH"], FRAGMENT, 1),
        (world, &["WORLD_FETCH"], &["WORLD_PROBE_VARYINGS"], 1),
        (world, &["WORLD_FETCH", "WORLD_PROBE_GEOMETRY"], FRAGMENT, 1),
        (depth, &["UNCLIPPED_DEPTH_ORTHO_EMULATION"], &["WORLD_ALPHA_CUTOFF", "WORLD_EXPLICIT_LOD"], 3),
        (depth, &["WORLD_DEPTH_TRIM"], &["WORLD_ALPHA_CUTOFF", "WORLD_EXPLICIT_LOD"], 2),
        (
            depth,
            &["WORLD_DEPTH_TRIM", "UNCLIPPED_DEPTH_ORTHO_EMULATION"],
            &["WORLD_ALPHA_CUTOFF"],
            3,
        ),
        (
            depth,
            &["WORLD_DEPTH_TRIM", "WORLD_DEPTH_POSITION_ONLY", "UNCLIPPED_DEPTH_ORTHO_EMULATION"],
            &["WORLD_EXPLICIT_LOD"],
            1,
        ),
        // `SKATE_VIS_CULL`'s marking view prepass: an opaque class's stage reads only where
        // its triangle's mark is, and a cutout's that besides its alpha test's.
        (
            depth,
            &["WORLD_DEPTH_TRIM", "WORLD_DEPTH_POSITION_ONLY", "WORLD_MARK"],
            &["WORLD_MARK_EARLY"],
            1,
        ),
        (
            depth,
            &["WORLD_DEPTH_TRIM", "WORLD_MARK"],
            &["WORLD_ALPHA_CUTOFF", "WORLD_EXPLICIT_LOD"],
            3,
        ),
        (
            depth,
            &["WORLD_DEPTH_TRIM", "WORLD_MARK", "WORLD_CUTOUT_FLAT"],
            &["WORLD_ALPHA_CUTOFF", "WORLD_EXPLICIT_LOD"],
            4,
        ),
        // `SKATE_VIS_CULL_AFTER_TEST` moves the cutout's mark, and reads nothing more.
        (
            depth,
            &["WORLD_DEPTH_TRIM", "WORLD_MARK", "WORLD_CUTOUT_FLAT"],
            &["WORLD_ALPHA_CUTOFF", "WORLD_MARK_AFTER_TEST"],
            4,
        ),
    ];
    for (source, both, fragment_only, reads) in cases {
        let vertex = stage_interface(&validate(source, both), naga::ShaderStage::Vertex);
        let fragment = stage_interface(
            &validate(source, &[both, fragment_only].concat()),
            naga::ShaderStage::Fragment,
        );
        for (location, read) in &fragment {
            assert_eq!(
                vertex.get(location),
                Some(read),
                "fragment location {location} under {both:?} and {fragment_only:?}"
            );
        }
        assert_eq!(fragment.len(), reads, "fragment locations under {both:?} and {fragment_only:?}");
    }
}

#[test]
fn character_shaders_validate() {
    validate(include_str!("retail_character.wgsl"), &[]);
    validate(include_str!("retail_character_depth.wgsl"), &[]);
    validate(
        include_str!("retail_character_depth.wgsl"),
        &[
            "PREPASS_FRAGMENT",
            "NORMAL_PREPASS",
            "NORMAL_PREPASS_OR_DEFERRED_PREPASS",
            "MOTION_VECTOR_PREPASS",
            "UNCLIPPED_DEPTH_ORTHO_EMULATION",
        ],
    );
}

/// The vertex entry point declares locations 0..=6, and `specialize` pins the
/// mesh attributes to those same locations in every main-pass variant. A
/// mismatch is silent corruption rather than an error, so assert the shader's
/// own view of its inputs.
#[test]
fn world_vertex_inputs_match_the_pinned_attribute_locations() {
    for extras in [&[][..], &["WORLD_PACKED_VARYINGS"], &["WORLD_PROBE_GEOMETRY"]] {
        let module = validate(include_str!("retail_world.wgsl"), extras);
        let mut locations: Vec<u32> = vertex_locations(&module).into_keys().collect();
        locations.sort_unstable();
        assert_eq!(locations, [0, 1, 2, 3, 4, 5, 6], "under {extras:?}");
    }
}

/// `specialize` declares only the position for a fetch pipeline's vertex stage,
/// and the fragment stage reads `primitive_index`, which needs
/// `Features::SHADER_PRIMITIVE_INDEX` (retail_render.rs `log_fetch_support`).
#[test]
fn fetch_variant_reads_the_position_and_primitive_index() {
    let world = vertex_locations(&validate(include_str!("retail_world.wgsl"), &[]));
    for extras in [&["WORLD_FETCH"][..], &["WORLD_FETCH", "WORLD_PROBE_GEOMETRY"]] {
        let module = validate(include_str!("retail_world.wgsl"), extras);
        let read = vertex_locations(&module);
        assert_eq!(read.keys().copied().collect::<Vec<_>>(), [0], "under {extras:?}");
        assert_eq!(read.get(&0), world.get(&0), "the position under {extras:?}");
        let fragment = module
            .entry_points
            .iter()
            .find(|e| e.stage == naga::ShaderStage::Fragment)
            .expect("shader must own its fragment entry point");
        let reads_primitive_index = fragment.function.arguments.iter().any(|argument| {
            match &module.types[argument.ty].inner {
                naga::TypeInner::Struct { members, .. } => members.iter().any(|member| {
                    member.binding
                        == Some(naga::Binding::BuiltIn(naga::BuiltIn::PrimitiveIndex))
                }),
                _ => false,
            }
        });
        assert!(reads_primitive_index, "the fragment stage under {extras:?}");
    }
}

#[test]
fn exposure_and_tone_shaders_validate() {
    validate(include_str!("retail_exposure.wgsl"), &[]);
    validate(include_str!("retail_tone.wgsl"), &[]);
    validate(include_str!("retail_tone.wgsl"), &["TONE_GAMMA"]);
}

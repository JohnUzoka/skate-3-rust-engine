//! Retail world materials for merged geometry.
//!
//! The old renderer gave every material its own bind group, so a material was a
//! draw. Here the material table is GPU data: `params` and `slots` are storage
//! arrays indexed by the `.skate` per-vertex material index, so one draw covers
//! every material in its slab (RFC 1 D1/D2).
//!
//! Only *textures* are slabbed. A `texture_2d_array` has a finite layer count
//! and every layer must share dimensions, so textures are grouped into pages by
//! size and a slab is sealed when a page fills. Parameters have no such limit,
//! which is why they are not what defines a slab.
use bevy::{
    asset::{AssetPath, RenderAssetUsages, embedded_asset, embedded_path, uuid_handle},
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    prelude::*,
    render::extract_component::ExtractComponent,
    render::{
        Render, RenderApp, RenderSystems,
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_asset::RenderAssets,
        renderer::RenderQueue,
        storage::GpuShaderStorageBuffer,
        render_resource::{
            AsBindGroup, AsBindGroupError, BufferUsages, CompareFunction, Extent3d, Face,
            FragmentState, RenderPipelineDescriptor,
            SpecializedMeshPipelineError, TextureDimension, TextureFormat, TextureViewDescriptor,
            TextureViewDimension,
        },
        storage::ShaderStorageBuffer,
    },
    shader::{ShaderDefVal, ShaderRef},
};
use skate_data::skate_map::{SkateMap, Texture, TextureBlock};
use std::collections::{BTreeMap, HashMap};

use crate::map_render::AssetSink;

/// Pages per slab. A `texture_2d_array` needs uniform layer dimensions, and
/// retail packages use more than a handful of sizes, so a single page would
/// force wholesale rescaling. Eight keeps the sampled-texture count inside the
/// 16-per-stage floor while covering the observed size spread.
pub(crate) const PAGE_CLASSES: usize = 8;

/// Conservative `max_texture_array_layers`. The scene is staged off the main
/// thread with no render device in hand, so this is a constant rather than a
/// queried limit; every desktop target we care about allows at least this many.
const MAX_PAGE_LAYERS: usize = 2048;

/// scene.hlsl's retail world ALPHAREF is 30, not the portable 0.5.
const ALPHA_REF: f32 = 30. / 255.;

/// Shader family of the retail `environment.transparent` materials
/// (`transparentenvironment_defaultPS` in `shaders_final.big`): chain-link
/// fences, wire mesh, grilles. Diffuse^2 times the shadowed lightmap, scaled by
/// the diffuse alpha (ALU 43), plus the lightmap-masked specular, with no normal
/// map and no `kd`; the output alpha is the diffuse alpha squared (ALU 53). See
/// the `fam == 16u` terms of `retail_world.wgsl`.
pub(crate) const TRANSPARENT_ENVIRONMENT_FAMILY: u32 = 16;

/// Shared per-frame state: shadow floor, animation clock and authored ocean PCA.
/// Map-independent, so it is a fixed handle rather than a staged asset.
pub(crate) const FRAME_STATE: Handle<ShaderStorageBuffer> =
    uuid_handle!("7f3a1c58-9d21-4e0b-9a44-2c6b8e51d7a3");

// ---------------------------------------------------------------------------
// GPU data
// ---------------------------------------------------------------------------

/// Per-material shading constants. Byte layout must match `WorldParams` in
/// `retail_material_bindings.wgsl`: thirteen `vec4<f32>`, 208 bytes.
#[derive(Clone, Debug, Default)]
pub(crate) struct WorldParams {
    /// family, texture flags, alpha cutoff (-1 for opaque), exposure
    pub mode: Vec4,
    /// Diagnostic solid foliage colour; w=0 keeps retail shading.
    pub foliage_debug: Vec4,
    /// macro UV scale, opacity, detail UV scale, material multiplier
    pub surface: Vec4,
    /// tree LM scale/floor/tint, proxy multiplier: reference day capture
    pub family: Vec4,
    pub fog_ramp: Vec4,
    pub fog_color: Vec4,
    pub shadow_color: Vec4,
    pub sun_direction: Vec4,
    /// Wear-only visual tuning; artwork retains unit opacity.
    pub decal: Vec4,
    pub water: [Vec4; 4],
}

impl WorldParams {
    pub(crate) const SIZE: usize = 13 * 16;

    fn encode(&self, out: &mut Vec<u8>) {
        for row in [
            self.mode,
            self.foliage_debug,
            self.surface,
            self.family,
            self.fog_ramp,
            self.fog_color,
            self.shadow_color,
            self.sun_direction,
            self.decal,
            self.water[0],
            self.water[1],
            self.water[2],
            self.water[3],
        ] {
            for component in row.to_array() {
                out.extend_from_slice(&component.to_le_bytes());
            }
        }
    }

    /// The parts of the world fragment this material reaches (`world_features`), from the
    /// family and flags in `mode` as the shader reads them.
    fn features(&self) -> u32 {
        world_features::of(self.mode.x as u32, self.mode.y as u32)
    }
}

/// The parts of the world fragment a material can reach, a bit each, as retail_world.wgsl's
/// `WORLD_FEATURES` reads them. Each bit is the condition the fragment tests on the family and
/// flags before it enters that part, so a pipeline none of whose materials has a bit can leave
/// that part out and still shade each of them the same (`SKATE_WORLD_FEATURES`,
/// render_options.rs).
pub(crate) mod world_features {
    /// Family 8 or below, 13 or 16: the skater's shadow.
    pub const SHADOW: u32 = 1;
    /// Flag 1, with family 6 or below, or 13: the normal map, and the tangent frame for it.
    pub const NORMAL: u32 = 1 << 1;
    /// Flag 2, with any family but 2: the detail map.
    pub const DETAIL: u32 = 1 << 2;
    /// Flag 128, with family 5, 6 or 13: the constant detail texel.
    pub const LOAD_DETAIL: u32 = 1 << 3;
    /// Flag 4: the macro overlay.
    pub const MACRO: u32 = 1 << 4;
    /// Flag 8, with family 3 or 4: the decal.
    pub const DECAL: u32 = 1 << 5;
    /// Flag 16: the specular map.
    pub const SPECULAR: u32 = 1 << 6;
    /// Flag 64, with family 5, 6 or 13: the environment cube.
    pub const ENVIRONMENT: u32 = 1 << 7;
    pub const ALL: u32 = (1 << 8) - 1;

    /// The parts a material of `family` with `flags` (`WorldParams::mode`) reaches.
    pub fn of(family: u32, flags: u32) -> u32 {
        let reflective = matches!(family, 5 | 6 | 13);
        let mut features = 0;
        let mut add = |reached: bool, feature: u32| {
            if reached {
                features |= feature;
            }
        };
        add(family <= 8 || family == 13 || family == super::TRANSPARENT_ENVIRONMENT_FAMILY, SHADOW);
        add(flags & 1 != 0 && (family <= 6 || family == 13), NORMAL);
        add(flags & 2 != 0 && family != 2, DETAIL);
        add(flags & 128 != 0 && reflective, LOAD_DETAIL);
        add(flags & 4 != 0, MACRO);
        add(flags & 8 != 0 && matches!(family, 3 | 4), DECAL);
        add(flags & 16 != 0, SPECULAR);
        add(flags & 64 != 0 && reflective, ENVIRONMENT);
        features
    }
}

/// Where each texture channel of one material lives, as `MaterialSlots` in the
/// shader: eight `u32`, 32 bytes.
///
/// Each channel packs `clamp << 31 | class << 16 | layer`. `ABSENT` is all ones,
/// whose class reads back above `PAGE_CLASSES` and so lands in the shader's
/// default branch; callers still gate on the presence bits in `mode.y`.
#[derive(Clone, Copy, Debug)]
struct MaterialSlots([u32; 8]);

const ABSENT: u32 = 0xffff_ffff;

impl Default for MaterialSlots {
    fn default() -> Self {
        Self([ABSENT; 8])
    }
}

impl MaterialSlots {
    const SIZE: usize = 32;

    fn encode(&self, out: &mut Vec<u8>) {
        for slot in self.0 {
            out.extend_from_slice(&slot.to_le_bytes());
        }
    }
}

fn pack_slot(class: usize, layer: usize, clamp: bool) -> u32 {
    debug_assert!(class < PAGE_CLASSES && layer < MAX_PAGE_LAYERS);
    (u32::from(clamp) << 31) | ((class as u32) << 16) | layer as u32
}

/// Shared frame state, matching `FrameState` in the bindings module: 160 bytes.
///
/// `shadow.w` gates every dynamic-shadow read in the world shader. Shadows are
/// out of scope for v1 (RFC 1 D5), so it stays zero and the shader never touches
/// the cascade bindings; re-enabling them is a write to this field.
///
/// `casters` bounds what the baked world's shadow light draws, so the world
/// shader can skip lookups that could only come back lit (bindings
/// `near_casters`, retail_shadow_bounds.rs `caster_bounds`). Zero, the default,
/// bounds nothing and every lookup is made.
#[derive(Resource, Clone, Default, ExtractResource)]
pub(crate) struct FrameStateData {
    pub shadow: Vec4,
    pub clock: Vec4,
    pub pca: [Vec4; 7],
    pub casters: Vec4,
}

impl FrameStateData {
    const SIZE: usize = 10 * 16;

    /// Eases the shadow floor towards the local probe's ambient term. Carried
    /// over with the character lighting that feeds it.
    pub(crate) fn approach(&mut self, target: Vec3, dt: f32) {
        let target = target.clamp(Vec3::ZERO, Vec3::ONE);
        let value = if self.shadow.w == 0. {
            target
        } else {
            // Adapter smoothing, not a recovered native constant. Cap a hitch's
            // contribution so one long frame cannot cause a darkness step.
            self.shadow
                .truncate()
                .lerp(target, 1. - (-dt.clamp(0., 0.05) / 0.35).exp())
        };
        self.shadow = value.extend(1.);
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::SIZE);
        for row in std::iter::once(self.shadow)
            .chain(std::iter::once(self.clock))
            .chain(self.pca)
            .chain(std::iter::once(self.casters))
        {
            for component in row.to_array() {
                out.extend_from_slice(&component.to_le_bytes());
            }
        }
        out
    }
}

/// Marks a camera that wants the retail tone curve rather than Bevy's.
///
/// `retail_exposure` meters this view and runs the tone pass on it. The world
/// shader emits linear radiance premultiplied by the baseline exposure of 2.5,
/// which the tone curve divides back out, so a camera without this marker
/// renders that unmapped signal straight into an 8-bit surface and clips
/// everything bright to white.
#[derive(Component, ExtractComponent, Clone, Copy, Default, Debug)]
pub(crate) struct RetailTone;

// ---------------------------------------------------------------------------
// Render classes
// ---------------------------------------------------------------------------

/// Pipeline state that cannot vary per vertex, and therefore cannot be merged
/// into one draw.
///
/// Alpha testing is a class, even though the cutoff itself lives in `mode.z`
/// per material. A shader that can `discard` cannot resolve depth before it
/// runs, so the hardware stops writing depth early and nothing rejects the
/// fragments hidden behind it. Sharing one draw between opaque and alpha-tested
/// geometry therefore imposed alpha-tested overdraw on the whole world: the
/// opaque pass shaded 2.16x the pixels it kept. Keeping the classes apart lets
/// the opaque variant compile without `discard` and reject hidden fragments
/// before shading them. The split only costs a draw where one slab holds both
/// kinds. Under a depth prepass the cutout classes lose their `discard` in the
/// main pass as well (`WorldMaterial::specialize`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum RenderClass {
    Opaque,
    OpaqueTwoSided,
    Cutout,
    CutoutTwoSided,
    Blended,
    BlendedTwoSided,
}

impl RenderClass {
    /// `cutout` only separates the opaque classes. A blended material resolves
    /// coverage by blending, and its cutoff is honoured by the same shader
    /// variant either way, so splitting it would buy nothing.
    fn new(blended: bool, cutout: bool, two_sided: bool) -> Self {
        match (blended, cutout, two_sided) {
            (true, _, false) => Self::Blended,
            (true, _, true) => Self::BlendedTwoSided,
            (false, true, false) => Self::Cutout,
            (false, true, true) => Self::CutoutTwoSided,
            (false, false, false) => Self::Opaque,
            (false, false, true) => Self::OpaqueTwoSided,
        }
    }

    pub(crate) fn two_sided(self) -> bool {
        matches!(
            self,
            Self::OpaqueTwoSided | Self::CutoutTwoSided | Self::BlendedTwoSided
        )
    }

    /// Whether this class's shader variant keeps the `discard`, and so gives up
    /// early depth writes.
    fn discards(self) -> bool {
        !matches!(self, Self::Opaque | Self::OpaqueTwoSided)
    }

    /// Whether this class is alpha tested without blending. Only these classes
    /// can let a depth prepass decide their coverage (`WorldMaterial::specialize`);
    /// Bevy leaves blended geometry out of the prepass.
    fn cutout(self) -> bool {
        matches!(self, Self::Cutout | Self::CutoutTwoSided)
    }

    /// Whether this class is blended. Bevy draws blended geometry back to front, a draw at a
    /// time, so how the leaves split decides the order its surfaces cover each other in.
    fn blended(self) -> bool {
        matches!(self, Self::Blended | Self::BlendedTwoSided)
    }

    fn alpha_mode(self) -> AlphaMode {
        match self {
            // Genuinely opaque: no cutoff reaches the shader, so Bevy must not
            // set MAY_DISCARD either.
            Self::Opaque | Self::OpaqueTwoSided => AlphaMode::Opaque,
            // `Mask` so Bevy sets MAY_DISCARD; the threshold is per-material in
            // `mode.z`.
            Self::Cutout | Self::CutoutTwoSided => AlphaMode::Mask(ALPHA_REF),
            Self::Blended | Self::BlendedTwoSided => AlphaMode::Blend,
        }
    }

    pub(crate) const ALL: [Self; 6] = [
        Self::Opaque,
        Self::OpaqueTwoSided,
        Self::Cutout,
        Self::CutoutTwoSided,
        Self::Blended,
        Self::BlendedTwoSided,
    ];
}

// ---------------------------------------------------------------------------
// Material asset
// ---------------------------------------------------------------------------

/// One slab's worth of world materials, plus the pipeline state of one class
/// and whether it draws water.
///
/// Every binding is shared between the variants of a slab, so they cost a bind
/// group each, not a copy of the data.
#[derive(Asset, TypePath, Clone, Debug)]
pub(crate) struct WorldMaterial {
    pub bindings: WorldBindings,
    pub class: RenderClass,
    pub water: bool,
    /// The shader variant `RenderOptions` asks for (render_options.rs `world_variant`).
    pub variant: crate::render_options::WorldVariant,
    /// Each page's width and height in texels, for `SKATE_WORLD_LOD`'s levels of detail.
    pub page_sizes: [[u32; 2]; PAGE_CLASSES],
    /// The parts of the world fragment that some material drawn with this one reaches
    /// (`world_features`), for `SKATE_WORLD_FEATURES`.
    pub features: u32,
}

/// What a world material binds, as `AsBindGroup` derives it. `WorldMaterial` binds it as it
/// is, or under `SKATE_WORLD_PAGE_ARRAY` with the eight pages as one array at binding 3.
#[derive(AsBindGroup, Clone, Debug)]
pub(crate) struct WorldBindings {
    #[storage(0, read_only)]
    pub params: Handle<ShaderStorageBuffer>,
    #[storage(1, read_only)]
    pub slots: Handle<ShaderStorageBuffer>,
    #[storage(2, read_only)]
    pub frame_state: Handle<ShaderStorageBuffer>,
    #[texture(3, dimension = "2d_array")]
    #[sampler(11)]
    pub page_0: Handle<Image>,
    #[texture(4, dimension = "2d_array")]
    pub page_1: Handle<Image>,
    #[texture(5, dimension = "2d_array")]
    pub page_2: Handle<Image>,
    #[texture(6, dimension = "2d_array")]
    pub page_3: Handle<Image>,
    #[texture(7, dimension = "2d_array")]
    pub page_4: Handle<Image>,
    #[texture(8, dimension = "2d_array")]
    pub page_5: Handle<Image>,
    #[texture(9, dimension = "2d_array")]
    pub page_6: Handle<Image>,
    #[texture(10, dimension = "2d_array")]
    pub page_7: Handle<Image>,
    #[texture(12, dimension = "cube_array")]
    pub cubes: Handle<Image>,
    /// `SKATE_WORLD_FETCH`'s copy of the world's vertices and triangles; a placeholder when the
    /// map builds none (render_options.rs `world_fetch_geometry`).
    #[storage(13, read_only)]
    pub fetch_vertices: Handle<ShaderStorageBuffer>,
    #[storage(14, read_only)]
    pub fetch_triangles: Handle<ShaderStorageBuffer>,
    /// `SKATE_VIS_CULL`'s marks: a word for each world triangle, set by the view's depth prepass
    /// (retail_depth.wgsl `WORLD_MARK`) and read and cleared by bevy_pbr's triangle cull. Only
    /// fragment stages see it: wgpu lets a vertex stage write storage only under a feature,
    /// `VERTEX_WRITABLE_STORAGE`, that nothing here needs.
    /// `WorldMaterial` leaves the binding out where the marks aren't bound (`vis_cull_supported`),
    /// and a map that builds none gets a placeholder of a word.
    #[storage(15, visibility(fragment))]
    pub marks: Handle<ShaderStorageBuffer>,
}

/// The bindings of the eight pages, in page order (`WorldBindings`).
const PAGE_BINDINGS: std::ops::RangeInclusive<u32> = 3..=10;

/// The binding of `SKATE_VIS_CULL`'s marks (`WorldBindings::marks`).
const MARKS_BINDING: u32 = 15;

impl WorldMaterial {
    fn with_pages(
        params: Handle<ShaderStorageBuffer>,
        slots: Handle<ShaderStorageBuffer>,
        pages: &[Handle<Image>; PAGE_CLASSES],
        page_sizes: [[u32; 2]; PAGE_CLASSES],
        cubes: Handle<Image>,
        fetch: &FetchBuffers,
        marks: &Handle<ShaderStorageBuffer>,
        class: RenderClass,
        water: bool,
        features: u32,
    ) -> Self {
        Self {
            bindings: WorldBindings {
                params,
                slots,
                frame_state: FRAME_STATE,
                page_0: pages[0].clone(),
                page_1: pages[1].clone(),
                page_2: pages[2].clone(),
                page_3: pages[3].clone(),
                page_4: pages[4].clone(),
                page_5: pages[5].clone(),
                page_6: pages[6].clone(),
                page_7: pages[7].clone(),
                cubes,
                fetch_vertices: fetch.vertices.clone(),
                fetch_triangles: fetch.triangles.clone(),
                marks: marks.clone(),
            },
            class,
            water,
            variant: crate::render_options::RenderOptions::from_env().world,
            page_sizes,
            features,
        }
    }
}

/// `WorldBindings`'s bind group, but under `SKATE_WORLD_PAGE_ARRAY` (render_options.rs
/// `world_page_array`) its eight page bindings become one binding of eight. Bevy's material
/// bind group allocator binds one resource a binding, so then the material builds its bind
/// group itself (`AsBindGroupError::CreateBindGroupDirectly`, bevy_pbr `prepare_asset`). The
/// marks' binding is left out unless `SKATE_VIS_CULL` can mark (`vis_cull_supported`), so a
/// session without it binds no buffer every pass may write, which wgpu would fence each pass for.
impl AsBindGroup for WorldMaterial {
    type Data = WorldMaterialKey;
    type Param = <WorldBindings as AsBindGroup>::Param;

    fn label() -> &'static str {
        "WorldMaterial"
    }

    fn bind_group_data(&self) -> Self::Data {
        self.into()
    }

    fn unprepared_bind_group(
        &self,
        layout: &bevy::render::render_resource::BindGroupLayout,
        render_device: &bevy::render::renderer::RenderDevice,
        param: &mut bevy::ecs::system::SystemParamItem<'_, '_, Self::Param>,
        force_no_bindless: bool,
    ) -> Result<bevy::render::render_resource::UnpreparedBindGroup, AsBindGroupError> {
        if crate::render_options::world_page_array() {
            return Err(AsBindGroupError::CreateBindGroupDirectly);
        }
        let mut unprepared =
            self.bindings.unprepared_bind_group(layout, render_device, param, force_no_bindless)?;
        if !vis_cull_supported() {
            unprepared.bindings.retain(|(index, _)| *index != MARKS_BINDING);
        }
        Ok(unprepared)
    }

    fn as_bind_group(
        &self,
        layout_descriptor: &bevy::render::render_resource::BindGroupLayoutDescriptor,
        render_device: &bevy::render::renderer::RenderDevice,
        pipeline_cache: &bevy::render::render_resource::PipelineCache,
        param: &mut bevy::ecs::system::SystemParamItem<'_, '_, Self::Param>,
    ) -> Result<bevy::render::render_resource::PreparedBindGroup, AsBindGroupError> {
        use bevy::render::render_resource::{
            BindGroupEntry, BindingResource, OwnedBindingResource, PreparedBindGroup,
            UnpreparedBindGroup,
        };
        let layout = pipeline_cache.get_bind_group_layout(layout_descriptor);
        let UnpreparedBindGroup { mut bindings } =
            self.bindings.unprepared_bind_group(&layout, render_device, param, false)?;
        if !vis_cull_supported() {
            bindings.retain(|(index, _)| *index != MARKS_BINDING);
        }
        let array = crate::render_options::world_page_array();
        let mut pages = Vec::with_capacity(PAGE_CLASSES);
        for binding in PAGE_BINDINGS {
            if let Some((_, OwnedBindingResource::TextureView(_, view))) =
                bindings.iter().find(|(index, _)| *index == binding)
            {
                pages.push(&**view);
            }
        }
        if array && pages.len() != PAGE_CLASSES {
            return Err(AsBindGroupError::RetryNextUpdate);
        }
        let mut entries: Vec<BindGroupEntry> = bindings
            .iter()
            .filter(|(index, _)| !array || !PAGE_BINDINGS.contains(index))
            .map(|(index, resource)| BindGroupEntry {
                binding: *index,
                resource: resource.get_binding(),
            })
            .collect();
        if array {
            entries.push(BindGroupEntry {
                binding: *PAGE_BINDINGS.start(),
                resource: BindingResource::TextureViewArray(&pages),
            });
        }
        let bind_group = render_device.create_bind_group(Self::label(), &layout, &entries);
        Ok(PreparedBindGroup { bindings, bind_group })
    }

    fn bind_group_layout_entries(
        render_device: &bevy::render::renderer::RenderDevice,
        force_no_bindless: bool,
    ) -> Vec<bevy::render::render_resource::BindGroupLayoutEntry> {
        let mut entries =
            WorldBindings::bind_group_layout_entries(render_device, force_no_bindless);
        if !vis_cull_supported() {
            entries.retain(|entry| entry.binding != MARKS_BINDING);
        }
        if crate::render_options::world_page_array() {
            let first = *PAGE_BINDINGS.start();
            entries.retain(|entry| entry.binding == first || !PAGE_BINDINGS.contains(&entry.binding));
            if let Some(entry) = entries.iter_mut().find(|entry| entry.binding == first) {
                entry.count = std::num::NonZeroU32::new(PAGE_CLASSES as u32);
            }
        }
        entries
    }
}

/// Only the state `specialize` needs: face culling, whether the fragment
/// shader keeps its `discard`, whether a depth prepass can stand in for that
/// `discard`, whether it has the water families, the shader variant, and the
/// parts of the fragment its materials reach.
///
/// Bevy's own pipeline key carries the alpha mode, but it selects blend state
/// rather than shader defs, so the `discard` has to be keyed here.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct WorldMaterialKey {
    two_sided: bool,
    discards: bool,
    cutout: bool,
    water: bool,
    variant: crate::render_options::WorldVariant,
    page_sizes: [[u32; 2]; PAGE_CLASSES],
    features: u32,
}

impl From<&WorldMaterial> for WorldMaterialKey {
    fn from(material: &WorldMaterial) -> Self {
        Self {
            two_sided: material.class.two_sided(),
            discards: material.class.discards(),
            cutout: material.class.cutout(),
            water: material.water,
            variant: material.variant,
            page_sizes: material.page_sizes,
            // Without the variant the masks change nothing, so materials that differ only in
            // them, like `SKATE_WORLD_SHADOW_SPLIT`'s groups, share their pipelines.
            features: if material.variant.features {
                material.features
            } else {
                world_features::ALL
            },
        }
    }
}

// Use the same path derivation as embedded_asset!: alternate binary targets have
// a different crate namespace even though they share these source files.
fn retail_shader(path: &str) -> ShaderRef {
    AssetPath::from(embedded_path!(path))
        .with_source("embedded")
        .into()
}

impl Material for WorldMaterial {
    fn vertex_shader() -> ShaderRef {
        retail_shader("retail_world.wgsl")
    }

    fn fragment_shader() -> ShaderRef {
        retail_shader("retail_world.wgsl")
    }

    // Shadow maps are built from the prepass, and `specialize` below declares one
    // vertex layout for every pipeline. Both prepass stages are ours so that the
    // layout and the shader agree about where `material_index` lives.
    fn prepass_vertex_shader() -> ShaderRef {
        retail_shader("retail_depth.wgsl")
    }

    fn prepass_fragment_shader() -> ShaderRef {
        retail_shader("retail_depth.wgsl")
    }

    fn alpha_mode(&self) -> AlphaMode {
        self.class.alpha_mode()
    }

    fn specialize(
        _: &bevy::pbr::MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &bevy::mesh::MeshVertexBufferLayoutRef,
        key: bevy::pbr::MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The material index is a vertex attribute, so the layout is ours to
        // declare rather than Bevy's mesh pipeline's. The packed attributes are
        // the same floats to the shaders (skate_world.rs `ATTRIBUTE_PACKED_NORMAL`).
        use crate::skate_world::{
            ATTRIBUTE_DECAL_UV, ATTRIBUTE_MATERIAL_INDEX, ATTRIBUTE_PACKED_NORMAL,
            ATTRIBUTE_PACKED_TANGENT,
        };
        let (normal, decal_uv, tangent) = if layout.0.contains(ATTRIBUTE_PACKED_NORMAL) {
            (ATTRIBUTE_PACKED_NORMAL, ATTRIBUTE_DECAL_UV, ATTRIBUTE_PACKED_TANGENT)
        } else {
            (Mesh::ATTRIBUTE_NORMAL, Mesh::ATTRIBUTE_COLOR, Mesh::ATTRIBUTE_TANGENT)
        };
        // Bevy marks prepass and shadow pipelines with `PREPASS_PIPELINE`.
        let prepass = descriptor
            .vertex
            .shader_defs
            .contains(&"PREPASS_PIPELINE".into());
        let variant = key.bind_group_data.variant;
        // `SKATE_PREPASS_PROBE` and `SKATE_VIS_CULL` are for the view's depth prepass alone. Bevy
        // keys a directional light's shadow pipelines `UNCLIPPED_DEPTH_ORTHO`, and both the game's
        // shadow casters are suns (retail_character.rs).
        let view_prepass =
            prepass && !key.mesh_key.contains(bevy::pbr::MeshPipelineKey::UNCLIPPED_DEPTH_ORTHO);
        let prepass_probe = if view_prepass { variant.prepass_probe } else { 0 };
        // Its `4` draws the cutout classes with no alpha test: no fragment stage, as the opaque
        // classes' depth pipelines have none. The view prepass writes no colour. Its `5` keeps
        // the stage but compiles it without the test (below), and its `6` with a test that
        // never discards.
        if prepass_probe == 4
            && descriptor
                .fragment
                .as_ref()
                .is_some_and(|fragment| fragment.targets.iter().all(Option::is_none))
        {
            descriptor.fragment = None;
        }
        // Under `SKATE_WORLD_FETCH` the main pass's vertex stage reads only the position: the
        // fragment stage fetches the rest itself (retail_world.wgsl `fetch_triangle`).
        let fetch = !prepass && variant.fetch;
        // A depth-only pipeline trimmed by `SKATE_WORLD_TRIM` declares only the attributes its
        // stages read: NVK fetches every one a pipeline declares. The UV and material index are
        // for the alpha test, which runs only in a fragment stage, and Bevy gives the depth
        // pipelines one only for the classes that can discard (or to emulate unclipped depth).
        let trimmed = prepass && variant.trim;
        let texcoords =
            !trimmed || (key.bind_group_data.discards && descriptor.fragment.is_some());
        // `SKATE_VIS_CULL`: the view's depth prepass marks each triangle that gets a fragment
        // into its depth test, and bevy_pbr's triangle cull keeps only the marked ones for the
        // main pass (retail_depth.wgsl `WORLD_MARK`). Its `1` marks in every class, its `2` in
        // the opaque ones alone, whose depth pipelines get a fragment stage for it: one that
        // reads no attribute more than before.
        let marking = view_prepass
            && vis_cull_supported()
            && match variant.vis_cull {
                1 => true,
                2 => !key.bind_group_data.discards,
                _ => false,
            };
        if marking && descriptor.fragment.is_none() {
            descriptor.fragment = Some(FragmentState {
                shader: descriptor.vertex.shader.clone(),
                shader_defs: descriptor.vertex.shader_defs.clone(),
                entry_point: Some("fragment".into()),
                targets: Vec::new(),
            });
        }
        // Whether a fragment stage here runs the alpha test (`WORLD_ALPHA_CUTOFF`, below).
        // `SKATE_PREPASS_PROBE=4` and `5` leave it out of the view prepass.
        let alpha_test = key.bind_group_data.discards && !matches!(prepass_probe, 4 | 5);
        descriptor.vertex.buffers = vec![if trimmed {
            let mut attributes = vec![Mesh::ATTRIBUTE_POSITION.at_shader_location(0)];
            if texcoords {
                attributes.push(Mesh::ATTRIBUTE_UV_0.at_shader_location(2));
                attributes.push(ATTRIBUTE_MATERIAL_INDEX.at_shader_location(5));
            }
            layout.0.get_layout(&attributes)?
        } else if fetch {
            layout.0.get_layout(&[Mesh::ATTRIBUTE_POSITION.at_shader_location(0)])?
        } else {
            layout.0.get_layout(&[
                Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
                normal.at_shader_location(1),
                Mesh::ATTRIBUTE_UV_0.at_shader_location(2),
                Mesh::ATTRIBUTE_UV_1.at_shader_location(3),
                decal_uv.at_shader_location(4),
                ATTRIBUTE_MATERIAL_INDEX.at_shader_location(5),
                tangent.at_shader_location(6),
            ])?
        }];
        // Every world pipeline shares the material's bind group layout, so each stage declares
        // the pages the way the layout binds them (`WorldMaterial::bind_group_layout_entries`).
        if crate::render_options::world_page_array() {
            descriptor.vertex.shader_defs.push("WORLD_PAGE_ARRAY".into());
            if let Some(fragment) = &mut descriptor.fragment {
                fragment.shader_defs.push("WORLD_PAGE_ARRAY".into());
                // Only fragment stages look pages up, and only those have the device's
                // subgroups for certain (`log_page_uniform_support`).
                if variant.page_uniform && page_uniform_supported() {
                    fragment.shader_defs.push("WORLD_PAGE_UNIFORM".into());
                }
            }
        }
        // Defs both stages see, since they change the interface between them.
        let mut interface: Vec<ShaderDefVal> = Vec::new();
        if trimmed {
            interface.push("WORLD_DEPTH_TRIM".into());
            if !texcoords {
                interface.push("WORLD_DEPTH_POSITION_ONLY".into());
            }
        }
        if marking {
            interface.push("WORLD_MARK".into());
        }
        if !prepass {
            if fetch {
                interface.push("WORLD_FETCH".into());
            }
            if variant.probe == 2 {
                interface.push("WORLD_PROBE_GEOMETRY".into());
            } else if variant.packed && !fetch {
                interface.push("WORLD_PACKED_VARYINGS".into());
            }
        }
        descriptor.vertex.shader_defs.extend(interface.iter().cloned());
        if let Some(fragment) = &mut descriptor.fragment {
            fragment.shader_defs.extend(interface);
            // A stage that can't discard has the depth test, and its write, before it runs, so
            // only the fragments that pass mark. One that can must run first, and marks them all.
            if marking && !alpha_test {
                fragment.shader_defs.push("WORLD_MARK_EARLY".into());
            }
            // `SKATE_VIS_CULL_AFTER_TEST`: one that can marks only the fragments its alpha test
            // keeps. Not the water families': the main pass tests their texel scrolled, and so can
            // draw where every fragment of a triangle failed the prepass's test.
            if marking && alpha_test && variant.mark_after_test && !key.bind_group_data.water {
                fragment.shader_defs.push("WORLD_MARK_AFTER_TEST".into());
            }
            if !prepass && variant.probe == 1 {
                fragment.shader_defs.push("WORLD_PROBE_VARYINGS".into());
            }
            // Only the main pass: the prepass's alpha test decides which fragments it shades.
            if !prepass && variant.probe == 3 {
                fragment.shader_defs.push("WORLD_PROBE_TEXELS".into());
            }
            if !prepass && variant.probe == 4 {
                fragment.shader_defs.push("WORLD_PROBE_NO_TEXTURES".into());
            }
            if !prepass && variant.probe == 5 {
                fragment.shader_defs.push("WORLD_PROBE_ONE_PAGE".into());
            }
            // Only under `WORLD_SLOT_WATERFALL`, whose repeats it drops (retail_world.wgsl).
            if !prepass && variant.probe == 6 {
                fragment.shader_defs.push("WORLD_PROBE_ONE_PASS".into());
            }
            // The same lookup probes in the view prepass's alpha test (retail_depth.wgsl). Its `6`
            // replaces the test with one that reads only the UV and never discards.
            match prepass_probe {
                1 => fragment.shader_defs.push("WORLD_PROBE_TEXELS".into()),
                2 => fragment.shader_defs.push("WORLD_PROBE_NO_TEXTURES".into()),
                3 => fragment.shader_defs.push("WORLD_PROBE_ONE_PAGE".into()),
                6 => fragment.shader_defs.push("WORLD_PROBE_KILL_ONLY".into()),
                _ => {}
            }
            // The prepass's alpha test samples the diffuse page the way the main pass does.
            if variant.explicit_lod {
                fragment.shader_defs.push("WORLD_EXPLICIT_LOD".into());
                for (page, [width, height]) in key.bind_group_data.page_sizes.iter().enumerate() {
                    fragment
                        .shader_defs
                        .push(ShaderDefVal::UInt(format!("WORLD_PAGE_{page}_W"), *width));
                    fragment
                        .shader_defs
                        .push(ShaderDefVal::UInt(format!("WORLD_PAGE_{page}_H"), *height));
                }
            }
        }
        // Retail foliage is two-sided; opaque world triangles retain winding.
        descriptor.primitive.cull_mode = if key.bind_group_data.two_sided {
            None
        } else {
            Some(Face::Back)
        };
        // Under a depth prepass (`SKATE_DEPTH_PREPASS`), the prepass has already
        // run each cutout fragment's alpha test: the same texel, gradients and
        // cutoff (retail_depth.wgsl), at the same position (`@invariant` in
        // both vertex stages). The depth it leaves is that of each pixel's
        // surviving surface. So the main pass draws a cutout fragment only
        // where its depth equals that one, and needs no `discard` of its own.
        // Without it the hardware tests depth before the shader runs, and
        // shades only the cutout fragments that are seen rather than every one
        // rasterized. The water families scroll the texel they test, which
        // the prepass does not, so they keep the test. Bevy marks prepass and
        // shadow pipelines with `PREPASS_PIPELINE`; those keep their `discard`,
        // since they are what resolves coverage.
        let resolved_by_prepass = !prepass
            && key.mesh_key.contains(bevy::pbr::MeshPipelineKey::DEPTH_PREPASS)
            && key.bind_group_data.cutout
            && !key.bind_group_data.water;
        if resolved_by_prepass && let Some(depth) = &mut descriptor.depth_stencil {
            depth.depth_compare = CompareFunction::Equal;
            depth.depth_write_enabled = false;
        }
        // Only the classes that can actually reject a texel compile the
        // `discard`. Without it the hardware resolves depth before the shader
        // runs, so hidden fragments never reach it. `SKATE_PREPASS_PROBE=5`
        // leaves it out of the view prepass, keeping an empty fragment stage,
        // as does its `4` where `SKATE_VIS_CULL` gives the stage back to mark.
        if alpha_test
            && !resolved_by_prepass
            && let Some(fragment) = &mut descriptor.fragment
        {
            fragment.shader_defs.push("WORLD_ALPHA_CUTOFF".into());
            // `SKATE_CUTOUT_FLAT`: a depth pipeline's vertex stage reads the material's diffuse
            // slot and cutoff for the test, and hands them on flat (retail_depth.wgsl). That
            // changes the interface between the stages, so both see it.
            if prepass && variant.cutout_flat {
                fragment.shader_defs.push("WORLD_CUTOUT_FLAT".into());
                descriptor.vertex.shader_defs.push("WORLD_CUTOUT_FLAT".into());
            }
        }
        // The water families are compiled only where a leaf has them, so every
        // other world fragment runs a shader without their code and registers.
        if key.bind_group_data.water
            && let Some(fragment) = &mut descriptor.fragment
        {
            fragment.shader_defs.push("WORLD_WATER".into());
        }
        // `SKATE_WORLD_FEATURES`: the main pass leaves out the parts of the fragment that none
        // of this material's families and channels reach (retail_world.wgsl `FEATURES`). Where
        // they reach every part, the shader is the one it was, so nothing new compiles.
        let features = key.bind_group_data.features;
        if !prepass
            && variant.features
            && features != world_features::ALL
            && let Some(fragment) = &mut descriptor.fragment
        {
            fragment
                .shader_defs
                .push(ShaderDefVal::UInt("WORLD_FEATURES".into(), features));
        }
        // `SKATE_WORLD_SLOT_WATERFALL`: the main pass shades one material at a time
        // (retail_world.wgsl `fragment`); the depth pipelines' alpha test is a shader of its own.
        if !prepass
            && variant.slot_waterfall
            && subgroups_supported()
            && let Some(fragment) = &mut descriptor.fragment
        {
            fragment.shader_defs.push("WORLD_SLOT_WATERFALL".into());
        }
        // `SKATE_WORLD_GPR48`: the opaque classes' main-pass fragment under the name NVK's
        // compiler reads as a cap of 48 registers (retail_world.wgsl `fragment_gpr48`). The name
        // keys the pipeline's shader hash, so the capped binaries are cached apart from the others.
        // The other classes keep their own: the cutout and blended fragments already fit in 48,
        // and the water families' needs 112, which capped would spill five times as much.
        if !prepass
            && variant.gpr48
            && !key.bind_group_data.discards
            && !key.bind_group_data.water
            && let Some(fragment) = &mut descriptor.fragment
        {
            fragment.shader_defs.push("WORLD_GPR48".into());
            fragment.entry_point = Some("fragment_gpr48".into());
        }
        // `SKATE_WORLD_SHORT_LIVES`: the main pass reads each texture where it folds the texel
        // in (retail_world.wgsl `WORLD_SHORT_LIVES`): the same arithmetic on the same values, with
        // fewer of them held at once. Every class takes it; the depth pipelines don't shade.
        if !prepass
            && variant.short_lives
            && let Some(fragment) = &mut descriptor.fragment
        {
            fragment.shader_defs.push("WORLD_SHORT_LIVES".into());
        }
        // `SKATE_WORLD_LEAN`: the main pass leaves out a test nothing sets and, with
        // `WORLD_SHORT_LIVES`, builds a tangent frame from derivatives only where the map supplied
        // none (retail_world.wgsl `WORLD_LEAN`). The same pixels.
        if !prepass
            && variant.lean
            && let Some(fragment) = &mut descriptor.fragment
        {
            fragment.shader_defs.push("WORLD_LEAN".into());
        }
        // `SKATE_WORLD_IMPLICIT`: the main pass's page lookups take their level of detail from
        // the quad (retail_material_bindings.wgsl `WORLD_IMPLICIT_LOD`). The depth pipelines keep
        // their gradients, as does the main pass where something else sets the level
        // (`SKATE_WORLD_LOD`, `SKATE_WORLD_PROBE=3`) and in the water families, whose refraction
        // moves the coordinates after their gradients are taken.
        if !prepass
            && variant.implicit_lod
            && !variant.explicit_lod
            && variant.probe != 3
            && !key.bind_group_data.water
            && let Some(fragment) = &mut descriptor.fragment
        {
            fragment.shader_defs.push("WORLD_IMPLICIT_LOD".into());
        }
        if crate::render_options::world_hoist()
            && let Some(fragment) = &mut descriptor.fragment
        {
            fragment.shader_defs.push("WORLD_HOIST".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Material definitions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Binding {
    pub texture: u32,
    pub uv: u32,
    pub u: u32,
    pub v: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Definition {
    pub shader: String,
    pub family: u32,
    pub flags: u32,
    pub bindings: BTreeMap<String, Binding>,
    pub parameters: BTreeMap<String, Vec<String>>,
}

impl Definition {
    pub(crate) fn parse(bytes: &[u8]) -> Option<Self> {
        struct Reader<'a>(&'a [u8]);
        impl<'a> Reader<'a> {
            fn take(&mut self, n: usize) -> Option<&'a [u8]> {
                let v = self.0.get(..n)?;
                self.0 = &self.0[n..];
                Some(v)
            }
            fn u32(&mut self) -> Option<u32> {
                Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
            }
            fn text(&mut self) -> Option<String> {
                let n = self.u32()? as usize;
                String::from_utf8(self.take(n)?.to_vec()).ok()
            }
        }
        let mut r = Reader(bytes);
        r.take(16)?;
        let shader = r.text()?;
        let stored_family = r.u32()?;
        // Older map packages retained the complete bindings but classified
        // non-flowing water as unknown. Upgrade without re-exporting geometry.
        let family = if matches!(
            shader.as_str(),
            "water.default" | "water.alpha" | "water.skatepark"
        ) {
            33
        } else if stored_family == 0 && shader == "environment.transparent" {
            // Map packages exported before the converter knew this family
            // stored 0 and drew as opaque family 1; classify them on load.
            TRANSPARENT_ENVIRONMENT_FAMILY
        } else {
            stored_family
        };
        let flags = r.u32()?;
        let mut bindings = BTreeMap::new();
        for _ in 0..r.u32()? {
            bindings.insert(
                r.text()?,
                Binding {
                    texture: r.u32()?,
                    uv: r.u32()?,
                    u: r.u32()?,
                    v: r.u32()?,
                },
            );
        }
        let mut parameters = BTreeMap::new();
        for _ in 0..r.u32()? {
            let name = r.text()?;
            let values = (0..r.u32()?)
                .map(|_| r.text())
                .collect::<Option<Vec<_>>>()?;
            // Names/GUIDs do not affect shading or batching.
            if !matches!(name.as_str(), "Name" | "AttribulatorMaterialName") {
                parameters.insert(name, values);
            }
        }
        r.text()?;
        if !r.0.is_empty() {
            return None;
        }
        Some(Self {
            shader,
            family,
            flags,
            bindings,
            parameters,
        })
    }

    /// A definition-free material: diffuse and lightmap out of the portable
    /// `.skate` fields. Custom maps and Skate 2 era exports land here.
    fn portable() -> Self {
        Self {
            shader: String::new(),
            family: 1,
            flags: 0,
            bindings: BTreeMap::new(),
            parameters: BTreeMap::new(),
        }
    }

    pub(crate) fn scalar(&self, name: &str) -> Option<f32> {
        self.parameters
            .get(name)?
            .first()?
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
    }

    pub(crate) fn supported(&self, tuning: &MaterialTuning) -> bool {
        (1..=13).contains(&self.family)
            || self.family == TRANSPARENT_ENVIRONMENT_FAMILY
            || match self.family {
                14 | 32 => tuning.rows.get(&self.shader).is_some_and(|r| !r.is_empty()),
                31 => {
                    tuning.pca_available
                        && tuning.rows.get(&self.shader).is_some_and(|r| r.len() == 3)
                }
                30 => tuning.rows.get(&self.shader).is_some_and(|r| r.len() == 4),
                33 => {
                    tuning.pca_available
                        && self.bindings.contains_key("normal")
                        && self.bindings.contains_key("normal2")
                        && tuning.rows.get(&self.shader).is_some_and(|r| r.len() == 4)
                }
                _ => false,
            }
    }

    /// The families whose branches only the `WORLD_WATER` shader variant has.
    fn water(&self) -> bool {
        matches!(self.family, 14 | 30..=33)
    }
}

// The source labels distinguish weathering from graphics. This is an explicit
// visual tuning choice, not a recovered native material constant. Keep arrows,
// logos, paint, scratches and edge wear at their authored alpha.
fn stain_opacity(texture_label: &str) -> f32 {
    let label = texture_label.to_ascii_lowercase();
    if [
        "grime",
        "grunge",
        "stain",
        "oildirt",
        "drainage",
        "ground_decals",
    ]
    .iter()
    .any(|word| label.contains(word))
    {
        0.35
    } else {
        1.
    }
}

#[derive(Default)]
pub(crate) struct MaterialTuning {
    rows: BTreeMap<String, Vec<[f32; 4]>>,
    pca_available: bool,
    /// Store the texture pages as BC3 rather than RGBA8; see [`compressed_pages`].
    pub(crate) compressed_pages: bool,
    /// Where the map's BC3 pages are kept between launches (page_cache.rs); None: built each time.
    pub(crate) page_cache: Option<crate::page_cache::PageCache>,
}

impl MaterialTuning {
    pub(crate) fn load(root: &std::path::Path) -> Self {
        let path = root.join("private/render-parameters.json");
        match std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<BTreeMap<String, Vec<[f32; 4]>>>(&b).ok())
        {
            Some(rows) if rows.values().flatten().flatten().all(|x| x.is_finite()) => Self {
                rows,
                pca_available: read_pca(root).is_some(),
                compressed_pages: false,
                page_cache: None,
            },
            _ => {
                warn!(
                    "Retail water/scroll tuning unavailable: {}",
                    path.display()
                );
                Self::default()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Texture pages
// ---------------------------------------------------------------------------

/// Under `SKATE_WORLD_FETCH`, logs whether the GPU gives fragment stages `primitive_index`
/// (`SHADER_PRIMITIVE_INDEX`, Vulkan's `geometryShader`), which the variant's main pass needs to
/// find its triangle. Without it those pipelines fail to build and the world goes undrawn.
pub(crate) fn log_fetch_support(world: &World) {
    if !crate::render_options::world_fetch_geometry() {
        return;
    }
    let supported = world
        .get_resource::<bevy::render::renderer::RenderDevice>()
        .is_some_and(|device| {
            device
                .features()
                .contains(bevy::render::settings::WgpuFeatures::SHADER_PRIMITIVE_INDEX)
        });
    eprintln!(
        "[Skate] world fetch: primitive_index {}",
        if supported { "available" } else { "UNAVAILABLE, so the fetch variant draws no world" }
    );
}

/// Whether `SKATE_WORLD_PAGE_UNIFORM` can take effect: the page array is bound and the device
/// has subgroup operations in fragment stages (`Features::SUBGROUP`), without which naga refuses
/// the variant's shaders and the world goes undrawn. Set as a map's scene is prepared, before
/// any world pipeline is specialized (`log_page_uniform_support`).
static PAGE_UNIFORM_SUPPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn page_uniform_supported() -> bool {
    PAGE_UNIFORM_SUPPORTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the device has subgroup operations in fragment stages (`Features::SUBGROUP`), which
/// `SKATE_WORLD_SLOT_WATERFALL` needs, page array or not. Set with `PAGE_UNIFORM_SUPPORTED`.
static SUBGROUPS_SUPPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn subgroups_supported() -> bool {
    SUBGROUPS_SUPPORTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reads whether the device has the subgroups that `SKATE_WORLD_PAGE_UNIFORM` (under
/// `SKATE_WORLD_PAGE_ARRAY`) and `SKATE_WORLD_SLOT_WATERFALL` need, and logs it. Without them
/// those variants change nothing.
pub(crate) fn log_page_uniform_support(world: &World) {
    let subgroups = world
        .get_resource::<bevy::render::renderer::RenderDevice>()
        .is_some_and(|device| {
            device.features().contains(bevy::render::settings::WgpuFeatures::SUBGROUP)
        });
    SUBGROUPS_SUPPORTED.store(subgroups, std::sync::atomic::Ordering::Relaxed);
    let supported = crate::render_options::world_page_array() && subgroups;
    PAGE_UNIFORM_SUPPORTED.store(supported, std::sync::atomic::Ordering::Relaxed);
    if crate::render_options::world_page_array() {
        eprintln!(
            "[Skate] world page uniform: subgroups {}",
            if supported { "available" } else { "UNAVAILABLE, so SKATE_WORLD_PAGE_UNIFORM does nothing" }
        );
    }
    eprintln!(
        "[Skate] world slot waterfall: subgroups {}",
        if subgroups { "available" } else { "UNAVAILABLE, so SKATE_WORLD_SLOT_WATERFALL does nothing" }
    );
}

/// Whether `SKATE_VIS_CULL` can mark: the session builds the marks (render_options.rs
/// `world_marks`), and the device's fragment stages have `primitive_index`
/// (`SHADER_PRIMITIVE_INDEX`), early depth tests they can ask for (`SHADER_EARLY_DEPTH_TEST`)
/// and storage writes (`DownlevelFlags::FRAGMENT_WRITABLE_STORAGE`). Only then does the world
/// material bind the marks, so it is set before Bevy derives the material's bind group layout
/// at `RenderStartup`, and never changes (`RetailRenderPlugin::finish`).
static VIS_CULL_SUPPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether the map on screen is a retail map. `SKATE_VIS_CULL`'s marks only work on retail maps:
/// on a custom map they leave the world undrawn (on the emulator Kenney Park's ramps are hidden
/// with the marks on and drawn with `SKATE_VIS_CULL=0`). Set when a map commits, before its world
/// pipelines compile (map_transition.rs), and read by `map_world_variant` and the cull.
static RETAIL_MAP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Sets whether the map on screen is a retail map, and says so for a custom one.
pub(crate) fn set_retail_map(retail: bool) {
    RETAIL_MAP.store(retail, std::sync::atomic::Ordering::Relaxed);
    if !retail {
        eprintln!(
            "[Skate] SKATE_VIS_CULL marks off: this map is not a retail map, so the world draws without them"
        );
    }
}

pub(crate) fn retail_map() -> bool {
    RETAIL_MAP.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn vis_cull_supported() -> bool {
    VIS_CULL_SUPPORTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reads whether `SKATE_VIS_CULL` can mark (`vis_cull_supported`) from the render device and
/// adapter, and logs it, in a session that builds the marks.
fn read_vis_cull_support(world: &World) {
    if !crate::render_options::world_marks() {
        return;
    }
    use bevy::render::{render_resource::DownlevelFlags, settings::WgpuFeatures};
    let features = world
        .get_resource::<bevy::render::renderer::RenderDevice>()
        .is_some_and(|device| {
            device.features().contains(
                WgpuFeatures::SHADER_PRIMITIVE_INDEX | WgpuFeatures::SHADER_EARLY_DEPTH_TEST,
            )
        });
    let stores = world
        .get_resource::<bevy::render::renderer::RenderAdapter>()
        .is_some_and(|adapter| {
            adapter
                .get_downlevel_capabilities()
                .flags
                .contains(DownlevelFlags::FRAGMENT_WRITABLE_STORAGE)
        });
    VIS_CULL_SUPPORTED.store(features && stores, std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "[Skate] world marks: {}",
        match (features, stores) {
            (true, true) => "bound",
            (false, _) => {
                "UNAVAILABLE (no primitive_index or early depth tests), so SKATE_VIS_CULL does \
                 nothing"
            }
            (true, false) => {
                "UNAVAILABLE (fragment stages can't write storage), so SKATE_VIS_CULL does nothing"
            }
        }
    );
}

/// Whether the texture pages should be BC3: on the Switch, when the GPU has BC textures and
/// `options.txt` doesn't say `rgba_pages`. There the GPU's memory comes out of the same 3 GiB
/// heap as everything else, and University's pages alone come to about 1.4 GiB as RGBA8.
/// Everywhere else they stay RGBA8.
pub(crate) fn compressed_pages(world: &World) -> bool {
    #[cfg(target_os = "horizon")]
    {
        if crate::horizon::rgba_pages() {
            info!("world texture pages: RGBA8, as options.txt asks");
            return false;
        }
        let bc = world
            .get_resource::<bevy::render::renderer::RenderDevice>()
            .is_some_and(|device| {
                device
                    .features()
                    .contains(bevy::render::settings::WgpuFeatures::TEXTURE_COMPRESSION_BC)
            });
        if !bc {
            warn!("world texture pages: RGBA8, because the GPU has no BC textures");
        }
        bc
    }
    #[cfg(not(target_os = "horizon"))]
    {
        let _ = world;
        false
    }
}

/// How a page stores its texels. BC3 (DXT5) is a quarter of RGBA8's size, and the shader reads
/// both the same way. The encoder is the range fit from texpresso (github.com/jansol/texpresso,
/// Jan Solanti, derived from Simon Brown's libsquish; MIT), with uniform channel weights because
/// pages hold normal and mask maps as well as colour. University's textures were DXT on the Xbox
/// 360; measured against their RGBA8, range fit's BC3 scores 49 dB PSNR in RGB and 57 dB in
/// alpha, at about 55 MiB/s on one desktop core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageFormat {
    Rgba8,
    Bc3,
}

impl PageFormat {
    /// BC3 stores 4x4 blocks, and wgpu wants a block-compressed texture's size to be whole
    /// blocks, so a page of any other size stays RGBA8.
    fn for_page(compressed: bool, width: u32, height: u32) -> Self {
        if compressed && width % 4 == 0 && height % 4 == 0 {
            Self::Bc3
        } else {
            Self::Rgba8
        }
    }

    fn texture_format(self) -> TextureFormat {
        match self {
            Self::Rgba8 => TextureFormat::Rgba8Unorm,
            Self::Bc3 => TextureFormat::Bc3RgbaUnorm,
        }
    }

    /// Bytes in one mip level, as wgpu's `create_texture_with_data` reads them: whole 4x4 blocks
    /// for BC3, so a level smaller than a block still takes one.
    fn level_len(self, width: u32, height: u32) -> usize {
        match self {
            Self::Rgba8 => width as usize * height as usize * 4,
            Self::Bc3 => texpresso::Format::Bc3.compressed_size(width as usize, height as usize),
        }
    }

    /// Bytes in one layer of `levels` mip levels.
    fn layer_len(self, width: u32, height: u32, levels: u32) -> usize {
        (0..levels)
            .map(|level| self.level_len((width >> level).max(1), (height >> level).max(1)))
            .sum()
    }

    /// Writes one layer into `out`, from `levels` mip levels of RGBA8 laid out as `mip_chain`
    /// lays them out.
    fn encode(self, rgba: &[u8], width: u32, height: u32, levels: u32, out: &mut [u8]) {
        match self {
            Self::Rgba8 => out.copy_from_slice(rgba),
            Self::Bc3 => {
                let params = texpresso::Params {
                    algorithm: texpresso::Algorithm::RangeFit,
                    weights: texpresso::COLOUR_WEIGHTS_UNIFORM,
                    weigh_colour_by_alpha: false,
                };
                let (mut from, mut to) = (0, 0);
                for level in 0..levels {
                    let (w, h) = ((width >> level).max(1), (height >> level).max(1));
                    let (texels, blocks) = (w as usize * h as usize * 4, self.level_len(w, h));
                    texpresso::Format::Bc3.compress(
                        &rgba[from..from + texels],
                        w as usize,
                        h as usize,
                        params,
                        &mut out[to..to + blocks],
                    );
                    from += texels;
                    to += blocks;
                }
                debug_assert_eq!((from, to), (rgba.len(), out.len()));
            }
        }
    }
}

/// Threads building one page's layers, the calling thread included. std can't count the
/// Switch's cores (`available_parallelism` is unsupported there), so it gets the three that
/// skate_map's decoder uses.
fn page_workers() -> usize {
    if cfg!(target_os = "horizon") {
        3
    } else {
        std::thread::available_parallelism().map_or(1, |n| n.get().min(8))
    }
}

/// How one channel wants its texture sampled. Mirrors the reference role codes:
/// repeat with mips, clamped without, clamped with.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Usage {
    clamp: bool,
    mips: bool,
}

impl Usage {
    const REPEAT: Self = Self {
        clamp: false,
        mips: true,
    };
    const CLAMP: Self = Self {
        clamp: true,
        mips: false,
    };
    const CLAMP_MIPS: Self = Self {
        clamp: true,
        mips: true,
    };
}

/// Deduplicates textures by compressed payload so identical uploads share one layer.
/// Keying on the compressed bytes, not the pixels, is what lets a page-cache hit skip
/// decompression entirely (findings 58 §3.6). Returns a lookup from 1-based source id to
/// canonical 1-based id, with 0 meaning "no texture" in both directions.
fn canonical_texture_ids(textures: &[Texture], blocks: &[TextureBlock]) -> Vec<u32> {
    let mut seen: HashMap<(u32, u32, u32, u32, &[u8]), u32> = HashMap::new();
    let mut ids = vec![0; textures.len() + 1];
    for (index, texture) in textures.iter().enumerate() {
        let id = index as u32 + 1;
        let block = &blocks[index];
        ids[index + 1] = *seen
            .entry((
                texture.width,
                texture.height,
                texture.color_space,
                block.method,
                block.bytes.as_slice(),
            ))
            .or_insert(id);
    }
    ids
}

/// A page under construction: one texture size, one growing list of layers.
struct Page {
    width: u32,
    height: u32,
    layers: Vec<u32>,
    mips: bool,
}

/// Box-filters decoded UNORM faces down to 1x1. The same chain the native
/// renderer used, and what keeps repeating world maps from aliasing at distance.
pub(crate) fn mip_chain(rgba: &[u8], width: u32, height: u32, layers: u32) -> (Vec<u8>, u32) {
    let count = 32 - width.max(height).leading_zeros();
    let mut bytes = Vec::with_capacity(rgba.len() * 4 / 3 + 64);
    for face in rgba
        .chunks_exact((width * height * 4) as usize)
        .take(layers as usize)
    {
        let mut level = face.to_vec();
        let (mut w, mut h) = (width as usize, height as usize);
        bytes.extend_from_slice(&level);
        while w > 1 || h > 1 {
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            let mut next = vec![0; nw * nh * 4];
            for y in 0..nh {
                for x in 0..nw {
                    for c in 0..4 {
                        let mut sum = 0u32;
                        for dy in 0..2 {
                            for dx in 0..2 {
                                sum += level
                                    [((y * 2 + dy).min(h - 1) * w + (x * 2 + dx).min(w - 1)) * 4 + c]
                                    as u32;
                            }
                        }
                        next[(y * nw + x) * 4 + c] = ((sum + 2) / 4) as u8;
                    }
                }
            }
            bytes.extend_from_slice(&next);
            level = next;
            w = nw;
            h = nh;
        }
    }
    (bytes, count)
}

/// Bytes in one face's RGBA8 mip chain down to 1x1, as `mip_chain` lays it out.
fn mip_chain_len(width: u32, height: u32) -> usize {
    let (mut w, mut h) = (width as usize, height as usize);
    let mut len = w * h * 4;
    while w > 1 || h > 1 {
        (w, h) = ((w / 2).max(1), (h / 2).max(1));
        len += w * h * 4;
    }
    len
}

/// Area-average resample, used only for textures whose size did not earn a page.
fn resample(rgba: &[u8], width: u32, height: u32, target_w: u32, target_h: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let (tw, th) = (target_w as usize, target_h as usize);
    let mut out = vec![0u8; tw * th * 4];
    for y in 0..th {
        let (y0, y1) = (y * h / th, (((y + 1) * h + th - 1) / th).min(h).max(y * h / th + 1));
        for x in 0..tw {
            let (x0, x1) = (x * w / tw, (((x + 1) * w + tw - 1) / tw).min(w).max(x * w / tw + 1));
            for c in 0..4 {
                let mut sum = 0u32;
                let mut n = 0u32;
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        sum += rgba[(sy * w + sx) * 4 + c] as u32;
                        n += 1;
                    }
                }
                out[(y * tw + x) * 4 + c] = (sum / n.max(1)) as u8;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Material table
// ---------------------------------------------------------------------------

/// What the scene builder needs to know about one source material.
#[derive(Clone, Copy)]
pub(crate) struct MaterialEntry {
    /// Slab owning this material's textures.
    pub slab: u16,
    pub class: RenderClass,
    /// A water family, which only the `WORLD_WATER` variant of the world
    /// shader can draw.
    pub water: bool,
    /// Under `SKATE_WORLD_SHADOW_SPLIT`, whether the skater's shadow can fall on this opaque or
    /// cutout material (`world_features::SHADOW`), whose leaves are then drawn apart from the
    /// rest of its class. Always false without the split, and for blended materials
    /// (`Request::group`).
    pub shadow_group: bool,
    /// Index into the slab's `params`/`slots` arrays, i.e. the value written into
    /// the per-vertex material attribute.
    index: u32,
}

pub(crate) struct MaterialTable {
    /// One slot per source material index; `None` where the material has no
    /// renderable geometry contribution.
    entries: Vec<Option<MaterialEntry>>,
    /// By slab, class, water and shadow group (`MaterialEntry`).
    materials: HashMap<(u16, RenderClass, bool, bool), Handle<WorldMaterial>>,
    /// The parts of the world fragment each class, water and shadow group's materials reach,
    /// across the whole map (`world_features`).
    features: HashMap<(RenderClass, bool, bool), u32>,
    slabs: u16,
    /// The `SKATE_WORLD_FETCH` vertex copy every material binds, reserved before the
    /// materials and filled once the leaves are merged (skate_world.rs `FetchGeometry`).
    fetch: FetchBuffers,
    /// `SKATE_VIS_CULL`'s marks, which every material binds: reserved like `fetch`, and sized
    /// once the leaves are merged (skate_world.rs `spawn`).
    marks: Handle<ShaderStorageBuffer>,
}

/// The storage buffers `SKATE_WORLD_FETCH` reads: each vertex as four `vec4<u32>`, and each
/// triangle as its three vertices' indices into those and its material (retail_world.wgsl).
#[derive(Clone, Debug)]
pub(crate) struct FetchBuffers {
    pub vertices: Handle<ShaderStorageBuffer>,
    pub triangles: Handle<ShaderStorageBuffer>,
}

impl MaterialTable {
    pub(crate) fn fetch_buffers(&self) -> &FetchBuffers {
        &self.fetch
    }

    pub(crate) fn marks(&self) -> &Handle<ShaderStorageBuffer> {
        &self.marks
    }

    pub(crate) fn entry(&self, source: usize) -> Option<MaterialEntry> {
        *self.entries.get(source)?
    }

    /// The per-vertex material attribute value. Unrenderable materials never
    /// reach a merged mesh, so zero is only a defensive default.
    pub(crate) fn slab_index(&self, source: usize) -> u32 {
        self.entry(source).map_or(0, |entry| entry.index)
    }

    pub(crate) fn material(
        &self,
        slab: u16,
        class: RenderClass,
        water: bool,
        shadow_group: bool,
    ) -> Handle<WorldMaterial> {
        self.materials
            .get(&(slab, class, water, shadow_group))
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn slab_count(&self) -> usize {
        usize::from(self.slabs)
    }

    /// Builds the table from the package's materials. The package keeps its texture payloads
    /// compressed; pixels are decompressed per page layer during upload and dropped, so the
    /// whole decoded set never exists at once (findings 58 §3.6).
    pub(crate) fn build(
        map: &SkateMap,
        tuning: &MaterialTuning,
        sky: &crate::retail_sky::SkyEnvironment,
        materials: &mut impl AssetSink<WorldMaterial>,
        images: &mut impl AssetSink<Image>,
        buffers: &mut impl AssetSink<ShaderStorageBuffer>,
    ) -> Self {
        let _span = info_span!("build_material_table").entered();
        let (textures, blocks) = (&map.textures, &map.texture_blocks);
        let canonical = canonical_texture_ids(textures, blocks);

        // Pass 1: decode every material into a definition plus the channel
        // texture ids it wants, so page sizing can see the whole demand.
        let mut requests: Vec<Option<Request>> = Vec::with_capacity(map.materials.len());
        let mut unsupported = 0usize;
        let mut unsupported_shaders = std::collections::BTreeMap::<String, usize>::new();
        for material in &map.materials {
            let definition = material
                .retail_definition
                .as_deref()
                .and_then(Definition::parse);
            let mut definition = definition.unwrap_or_else(Definition::portable);
            if !definition.supported(tuning) {
                // A hole is worse than approximate shading, and there is no
                // second world material path to fall back to (RFC 1 D8), so an
                // unsupported family renders as plain diffuse plus lightmap.
                unsupported += 1;
                *unsupported_shaders
                    .entry(format!("{} (family {})", definition.shader, definition.family))
                    .or_default() += 1;
                definition.family = 1;
            }
            requests.push(Some(Request::new(
                material,
                &definition,
                tuning,
                sky,
                &canonical,
                textures,
            )));
        }
        if unsupported > 0 {
            warn!(
                "{unsupported} of {} world materials use an unsupported shader family and render as family 1: {unsupported_shaders:?}",
                map.materials.len()
            );
        }

        // Pass 2: choose page sizes from the actual demand, most-used first.
        let mut demand: HashMap<(u32, u32), usize> = HashMap::new();
        for request in requests.iter().flatten() {
            for channel in request.channels.iter().flatten() {
                if channel.cube {
                    continue;
                }
                let texture = &textures[channel.id as usize - 1];
                *demand.entry((texture.width, texture.height)).or_default() += 1;
            }
        }
        let mut sizes: Vec<((u32, u32), usize)> = demand.into_iter().collect();
        sizes.sort_unstable_by_key(|&(size, count)| (std::cmp::Reverse(count), size));
        let page_sizes: Vec<(u32, u32)> = sizes
            .iter()
            .take(PAGE_CLASSES)
            .map(|&(size, _)| size)
            .collect();
        let rescaled: usize = sizes.iter().skip(PAGE_CLASSES).map(|&(_, n)| n).sum();
        if rescaled > 0 {
            warn!(
                "{rescaled} world textures fall outside the {PAGE_CLASSES} most common sizes and were rescaled to the nearest page"
            );
        }

        // What each world material's pipelines must compile, for `SKATE_WORLD_FEATURES`. It is
        // taken across the map rather than by slab, so that every slab's pipelines for a class
        // stay the same ones, as they are without it.
        let mut features: HashMap<(RenderClass, bool, bool), u32> = HashMap::new();
        for request in requests.iter().flatten() {
            *features.entry(request.group()).or_default() |= request.params.features();
        }
        log_world_features(&features);

        // Pass 3: pack materials into slabs, sealing a slab when a page fills. Packing runs
        // to completion before anything uploads, so every slab's page sizes are the map-wide
        // ones chosen in pass 2.
        let mut table = Self {
            entries: vec![None; map.materials.len()],
            materials: HashMap::new(),
            features,
            slabs: 0,
            fetch: FetchBuffers { vertices: buffers.reserve(), triangles: buffers.reserve() },
            marks: buffers.reserve(),
        };
        let mut packed: Vec<Slab> = Vec::new();
        let mut slab = Slab::new(&page_sizes);
        for (source, request) in requests.iter().enumerate() {
            let Some(request) = request else { continue };
            if !slab.fits(request, textures) {
                packed.push(std::mem::replace(&mut slab, Slab::new(&page_sizes)));
            }
            // `seal` is what increments the count, so the slab being filled is
            // always the next index.
            let entry = slab.insert(request, textures, u16::try_from(packed.len()).unwrap_or(u16::MAX));
            table.entries[source] = Some(entry);
        }
        packed.push(slab);
        for slab in packed {
            table.seal(slab, textures, blocks, tuning, materials, images, buffers);
        }
        table
    }

    fn seal(
        &mut self,
        slab: Slab,
        textures: &[Texture],
        blocks: &[TextureBlock],
        tuning: &MaterialTuning,
        materials: &mut impl AssetSink<WorldMaterial>,
        images: &mut impl AssetSink<Image>,
        buffers: &mut impl AssetSink<ShaderStorageBuffer>,
    ) {
        if slab.params.is_empty() {
            return;
        }
        let index = self.slabs;
        self.slabs += 1;

        let mut params_bytes = Vec::with_capacity(slab.params.len() * WorldParams::SIZE);
        for params in &slab.params {
            params.encode(&mut params_bytes);
        }
        let mut slots_bytes = Vec::with_capacity(slab.slots.len() * MaterialSlots::SIZE);
        for slots in &slab.slots {
            slots.encode(&mut slots_bytes);
        }
        // `From<T>` goes through `encase`, which will not take a pre-encoded byte
        // blob, so the buffers are constructed from raw bytes instead.
        let params = buffers.add(ShaderStorageBuffer::new(
            &params_bytes,
            RenderAssetUsages::RENDER_WORLD,
        ));
        let slots = buffers.add(ShaderStorageBuffer::new(
            &slots_bytes,
            RenderAssetUsages::RENDER_WORLD,
        ));

        let pages = std::array::from_fn(|class| {
            slab.upload_page(class, textures, blocks, tuning.compressed_pages, (usize::from(index), tuning.page_cache.as_ref()), images)
        });
        // A class with no textures gets a placeholder page, which nothing samples.
        let page_sizes = std::array::from_fn(|class| {
            slab.pages.get(class).map_or([1, 1], |page| [page.width, page.height])
        });
        let cubes = slab.upload_cubes(textures, blocks, images);
        let shadow_groups: &[bool] = if crate::render_options::world_shadow_split() {
            &[false, true]
        } else {
            &[false]
        };
        for class in RenderClass::ALL {
            for water in [false, true] {
                for &shadow_group in shadow_groups {
                    // A group with no materials draws nothing, so compiles nothing either.
                    let features = self
                        .features
                        .get(&(class, water, shadow_group))
                        .copied()
                        .unwrap_or(world_features::ALL);
                    let material = materials.add(WorldMaterial::with_pages(
                        params.clone(),
                        slots.clone(),
                        &pages,
                        page_sizes,
                        cubes.clone(),
                        &self.fetch,
                        &self.marks,
                        class,
                        water,
                        features,
                    ));
                    self.materials.insert((index, class, water, shadow_group), material);
                }
            }
        }
    }
}

/// Logs the parts of the world fragment each world material's pipelines compile under
/// `SKATE_WORLD_FEATURES` (`world_features`), by class, water and shadow group.
fn log_world_features(features: &HashMap<(RenderClass, bool, bool), u32>) {
    let mut groups: Vec<_> = features.iter().collect();
    groups.sort_unstable();
    let groups: Vec<String> = groups
        .into_iter()
        .map(|(&(class, water, shadow_group), features)| {
            let water = if water { " water" } else { "" };
            let shadow = match (crate::render_options::world_shadow_split(), shadow_group) {
                (false, _) => "",
                (true, false) => " unshadowed",
                (true, true) => " shadowed",
            };
            format!("{class:?}{water}{shadow} {features:#04x}")
        })
        .collect();
    info!("[Skate] world features (of {:#04x}): {}", world_features::ALL, groups.join(", "));
}

/// One material's decoded shading inputs, before page assignment.
struct Request {
    params: WorldParams,
    class: RenderClass,
    water: bool,
    /// diffuse, lightmap, normal, detail, macro, decal, specular, environment
    channels: [Option<Channel>; 8],
}

#[derive(Clone, Copy)]
struct Channel {
    /// Canonical 1-based texture id.
    id: u32,
    usage: Usage,
    cube: bool,
}

impl Request {
    /// The world material this material is drawn with, short of its slab: its class, whether it
    /// is water, and its shadow group (`MaterialEntry::shadow_group`). The blended classes keep
    /// one group, so that their surfaces cover each other in the order they did.
    fn group(&self) -> (RenderClass, bool, bool) {
        let shadow_group = crate::render_options::world_shadow_split()
            && !self.class.blended()
            && self.params.features() & world_features::SHADOW != 0;
        (self.class, self.water, shadow_group)
    }

    fn new(
        material: &skate_data::skate_map::Material,
        definition: &Definition,
        tuning: &MaterialTuning,
        sky: &crate::retail_sky::SkyEnvironment,
        canonical: &[u32],
        textures: &[Texture],
    ) -> Self {
        let lookup = |id: u32, cube: bool| -> Option<u32> {
            let id = *canonical.get(id as usize)?;
            if id == 0 {
                return None;
            }
            let texture = &textures[id as usize - 1];
            // Older .skate exports contain only face zero. Never treat those as
            // a cube: six stacked faces is the only valid encoding.
            if cube && texture.height != texture.width * 6 {
                return None;
            }
            Some(id)
        };
        let fetch = |role: &str, fallback: u32, force_clamp: bool| -> Option<Channel> {
            let binding = definition.bindings.get(role);
            let id = lookup(binding.map_or(fallback, |b| b.texture), false)?;
            let clamped = force_clamp || binding.is_some_and(|b| b.u == 1 && b.v == 1);
            let usage = match (clamped, role) {
                (false, _) => Usage::REPEAT,
                (true, "decal") => Usage::CLAMP_MIPS,
                (true, _) => Usage::CLAMP,
            };
            Some(Channel {
                id,
                usage,
                cube: false,
            })
        };
        let diffuse = fetch("diffuse", material.textures[0], false);
        let lightmap = fetch("lightmap", material.textures[1], true);
        let normal = fetch("normal", material.textures[2], false);
        let detail = fetch(
            if matches!(definition.family, 31 | 33) {
                "normal2"
            } else {
                "detail"
            },
            0,
            false,
        );
        let macro_map = fetch("macrooverlay", 0, false);
        let decal = fetch("decal", 0, definition.family == 3);
        let specular = fetch("specular", 0, false);
        let environment = definition
            .bindings
            .get("environment")
            .and_then(|b| lookup(b.texture, true))
            .map(|id| Channel {
                id,
                usage: Usage::CLAMP_MIPS,
                cube: true,
            });

        let macro_scale = definition.scalar("macroOverlayUVScale").unwrap_or(0.);
        let macro_opacity = definition.scalar("macroOverlayOpacity").unwrap_or(0.);
        let detail_scale = definition.scalar("detailNormalUVScale").unwrap_or(0.);
        let flags = u32::from(normal.is_some())
            | (u32::from(detail.is_some() && detail_scale > 0.) << 1)
            | (u32::from(
                macro_map.is_some()
                    && macro_scale > 0.
                    && (macro_opacity > 0. || definition.family == 31),
            ) << 2)
            | (u32::from(decal.is_some()) << 3)
            | (u32::from(specular.is_some()) << 4)
            | (u32::from(lightmap.is_some()) << 5)
            | (u32::from(environment.is_some()) << 6)
            | (u32::from(detail.is_some()) << 7);

        let blended = material.alpha_mode == 2
            || definition.family == 32
            || (matches!(definition.family, 30 | 33) && definition.shader.ends_with("alpha"));
        // The same condition sets `mode.z` below. A material without it has a
        // cutoff of -1, which no sampled alpha can fall under, so its class must
        // not compile the `discard`.
        let cutout = material.alpha_mode == 1;
        let class = RenderClass::new(blended, cutout, definition.flags & 4 != 0);

        let mut water = [Vec4::ZERO; 4];
        if let Some(rows) = tuning.rows.get(&definition.shader) {
            for (to, from) in water.iter_mut().zip(rows) {
                *to = Vec4::from_array(*from);
            }
        }
        if definition.family == 14 {
            water[1] = Vec4::new(
                definition.scalar("uAnimationSpeed").unwrap_or(0.),
                definition.scalar("vAnimationSpeed").unwrap_or(0.),
                0.,
                0.,
            );
        }

        Self {
            params: WorldParams {
                mode: Vec4::new(
                    definition.family as f32,
                    flags as f32,
                    if cutout { ALPHA_REF } else { -1. },
                    2.5,
                ),
                foliage_debug: Vec4::ZERO,
                surface: Vec4::new(macro_scale, macro_opacity, detail_scale, 1.),
                family: Vec4::new(0.3435, 0.02, 1., 0.45),
                // Authored by the map's sky package; the defaults evaluate to no
                // fog at all, so a map without one is unchanged.
                fog_ramp: sky.fog_ramp,
                fog_color: sky.fog_color,
                shadow_color: Vec4::ZERO,
                sun_direction: sky.sun_direction,
                decal: Vec4::new(
                    stain_opacity(
                        definition
                            .parameters
                            .get("decal")
                            .and_then(|v| v.first())
                            .map(String::as_str)
                            .unwrap_or(""),
                    ),
                    0.,
                    0.,
                    0.,
                ),
                water,
            },
            class,
            water: definition.water(),
            channels: [
                diffuse,
                lightmap,
                normal,
                detail,
                macro_map,
                decal,
                specular,
                environment,
            ],
        }
    }
}

/// Accumulates one slab: its pages, its cube array and its GPU arrays.
struct Slab {
    pages: Vec<Page>,
    /// Canonical texture id to (class, layer) within this slab.
    placed: HashMap<u32, (usize, usize)>,
    cubes: Vec<u32>,
    cube_layer: HashMap<u32, usize>,
    params: Vec<WorldParams>,
    slots: Vec<MaterialSlots>,
}

impl Slab {
    fn new(page_sizes: &[(u32, u32)]) -> Self {
        Self {
            pages: page_sizes
                .iter()
                .map(|&(width, height)| Page {
                    width,
                    height,
                    layers: Vec::new(),
                    mips: false,
                })
                .collect(),
            placed: HashMap::new(),
            cubes: Vec::new(),
            cube_layer: HashMap::new(),
            params: Vec::new(),
            slots: Vec::new(),
        }
    }

    /// The page a texture of this size belongs to: exact match, else the closest
    /// by area so a rescale changes resolution as little as possible.
    fn class_for(&self, width: u32, height: u32) -> Option<usize> {
        if self.pages.is_empty() {
            return None;
        }
        if let Some(index) = self
            .pages
            .iter()
            .position(|p| p.width == width && p.height == height)
        {
            return Some(index);
        }
        let area = u64::from(width) * u64::from(height);
        self.pages
            .iter()
            .enumerate()
            .min_by_key(|(_, p)| {
                (u64::from(p.width) * u64::from(p.height)).abs_diff(area)
            })
            .map(|(index, _)| index)
    }

    /// Layer capacity is the only reason to seal a slab: the page *sizes* are
    /// chosen once for the whole map and every slab shares them.
    fn fits(&self, request: &Request, textures: &[Texture]) -> bool {
        let mut needed = [0usize; PAGE_CLASSES];
        let mut counted: Vec<u32> = Vec::new();
        for channel in request.channels.iter().flatten() {
            if channel.cube || self.placed.contains_key(&channel.id) || counted.contains(&channel.id)
            {
                continue;
            }
            counted.push(channel.id);
            let texture = &textures[channel.id as usize - 1];
            if let Some(class) = self.class_for(texture.width, texture.height) {
                needed[class] += 1;
            }
        }
        self.pages
            .iter()
            .enumerate()
            .all(|(class, page)| page.layers.len() + needed[class] <= MAX_PAGE_LAYERS)
    }

    fn insert(&mut self, request: &Request, textures: &[Texture], slab: u16) -> MaterialEntry {
        let index = self.params.len() as u32;
        let mut slots = MaterialSlots::default();
        for (channel_index, channel) in request.channels.iter().enumerate() {
            let Some(channel) = channel else { continue };
            if channel.cube {
                let layer = match self.cube_layer.get(&channel.id) {
                    Some(&layer) => layer,
                    None => {
                        let layer = self.cubes.len();
                        self.cubes.push(channel.id);
                        self.cube_layer.insert(channel.id, layer);
                        layer
                    }
                };
                // Cubes have their own binding, so the class field is unused.
                slots.0[channel_index] = pack_slot(0, layer, true);
                continue;
            }
            let texture = &textures[channel.id as usize - 1];
            let Some(class) = self.class_for(texture.width, texture.height) else {
                continue;
            };
            let layer = match self.placed.get(&channel.id) {
                Some(&(_, layer)) => layer,
                None => {
                    let page = &mut self.pages[class];
                    page.layers.push(channel.id);
                    let layer = page.layers.len() - 1;
                    self.placed.insert(channel.id, (class, layer));
                    layer
                }
            };
            self.pages[class].mips |= channel.usage.mips;
            slots.0[channel_index] = pack_slot(class, layer, channel.usage.clamp);
        }
        self.params.push(request.params.clone());
        self.slots.push(slots);
        let (class, water, shadow_group) = request.group();
        MaterialEntry {
            slab,
            class,
            water,
            shadow_group,
            index,
        }
    }

    /// `cache`: this slab's index, and the map's page cache (page_cache.rs), which keeps BC3 pages
    /// between launches.
    fn upload_page(
        &self,
        class: usize,
        textures: &[Texture],
        blocks: &[TextureBlock],
        compressed: bool,
        (slab, cache): (usize, Option<&crate::page_cache::PageCache>),
        images: &mut impl AssetSink<Image>,
    ) -> Handle<Image> {
        let Some(page) = self.pages.get(class).filter(|p| !p.layers.is_empty()) else {
            return placeholder_page(images);
        };
        let started = std::time::Instant::now();
        let (width, height) = (page.width, page.height);
        let levels = if page.mips { 32 - width.max(height).leading_zeros() } else { 1 };
        let format = PageFormat::for_page(compressed, width, height);
        let layer_len = format.layer_len(width, height, levels);
        let mib = |bytes: usize| bytes as f64 / (1024. * 1024.);
        let rgba8 = PageFormat::Rgba8.layer_len(width, height, levels) * page.layers.len();
        let mut cached = None;
        if let Some(cache) = cache.filter(|_| format == PageFormat::Bc3) {
            let key = page_key(page, textures, blocks, format, levels);
            let hashed = started.elapsed().as_millis();
            match cache.read(slab, class, key, layer_len * page.layers.len()) {
                Ok(bytes) if !cache.verify() => {
                    info!(
                        "world texture page {class}: {width}x{height}, {} layers, {levels} mip levels, {format:?}, \
                         {:.1} MiB ({:.1} MiB as RGBA8), read from the page cache in {} ms ({hashed} ms of it hashing its textures)",
                        page.layers.len(),
                        mib(bytes.len()),
                        mib(rgba8),
                        started.elapsed().as_millis(),
                    );
                    return images.add(array_image(
                        width,
                        height,
                        page.layers.len() as u32,
                        levels,
                        format.texture_format(),
                        bytes,
                        TextureViewDimension::D2Array,
                    ));
                }
                read => cached = Some((cache, key, read, hashed)),
            }
        }
        // Allocated once at its final size: a page can be hundreds of MiB, and growing it by
        // doubling would hold the old and new buffers at once and keep up to twice its size.
        let mut bytes = vec![0u8; layer_len * page.layers.len()];
        {
            // Each layer is resampled, mipmapped and encoded by itself, so threads take layers
            // in turn, each writing its own part of `bytes`.
            let jobs =
                std::sync::Mutex::new(page.layers.iter().zip(bytes.chunks_exact_mut(layer_len)));
            let work = || loop {
                let Some((&id, out)) = jobs.lock().unwrap().next() else { break };
                let texture = &textures[id as usize - 1];
                // Decompressed here, per layer, and dropped when this iteration ends: the map's
                // whole decoded pixel set never exists at once (findings 58 §3.6).
                let decoded = decode_block(&blocks[id as usize - 1]);
                let resampled;
                let rgba = if texture.width == width && texture.height == height {
                    &decoded
                } else {
                    resampled = resample(&decoded, texture.width, texture.height, width, height);
                    &resampled
                };
                let chain;
                let rgba = if page.mips {
                    chain = mip_chain(rgba, width, height, 1).0;
                    &chain
                } else {
                    rgba
                };
                format.encode(rgba, width, height, levels, out);
            };
            std::thread::scope(|scope| {
                for _ in 1..page_workers().min(page.layers.len()) {
                    scope.spawn(work);
                }
                work();
            });
        }
        info!(
            "world texture page {class}: {width}x{height}, {} layers, {levels} mip levels, {format:?}, \
             {:.1} MiB ({:.1} MiB as RGBA8), built in {} ms",
            page.layers.len(),
            mib(bytes.len()),
            mib(rgba8),
            started.elapsed().as_millis(),
        );
        if let Some((cache, key, read, hashed)) = cached {
            match read {
                Ok(copy) if copy == bytes => {
                    info!("PAGE_CACHE verify: page {class} of slab {slab} is its cached copy, byte for byte");
                }
                Ok(copy) => {
                    let first = copy.iter().zip(&bytes).position(|(a, b)| a != b);
                    warn!("PAGE_CACHE verify: page {class} of slab {slab} differs from its cached copy (first at byte {first:?}); writing it again");
                    cache.write(slab, class, key, &bytes);
                }
                Err(miss) => {
                    info!("PAGE_CACHE page {class} of slab {slab}: {miss} (key {key:016x}, {hashed} ms hashing its textures)");
                    cache.write(slab, class, key, &bytes);
                }
            }
        }
        images.add(array_image(
            width,
            height,
            page.layers.len() as u32,
            levels,
            format.texture_format(),
            bytes,
            TextureViewDimension::D2Array,
        ))
    }

    fn upload_cubes(&self, textures: &[Texture], blocks: &[TextureBlock], images: &mut impl AssetSink<Image>) -> Handle<Image> {
        let Some(&first) = self.cubes.first() else {
            return placeholder_cube(images);
        };
        let size = textures[first as usize - 1].width;
        let kept = self
            .cubes
            .iter()
            .filter(|&&id| textures[id as usize - 1].width == size)
            .count();
        let mut bytes = Vec::with_capacity(kept * 6 * mip_chain_len(size, size));
        let mut levels = 1;
        let mut faces = 0;
        for &id in &self.cubes {
            let texture = &textures[id as usize - 1];
            if texture.width != size {
                // A cube array shares one face size. Mismatches are rare enough
                // to drop rather than rescale; the slot still reads as present,
                // so log it instead of silently changing reflections.
                warn!(
                    "environment cube '{}' is {}px in a {}px array; reflection omitted",
                    texture.name, texture.width, size
                );
                continue;
            }
            let (chain, count) = mip_chain(&decode_block(&blocks[id as usize - 1]), size, size, 6);
            levels = count;
            bytes.extend_from_slice(&chain);
            faces += 6;
        }
        if faces == 0 {
            return placeholder_cube(images);
        }
        info!(
            "world environment cubes: {size}px, {} cubes, {levels} mip levels, {:.1} MiB",
            faces / 6,
            bytes.len() as f64 / (1024. * 1024.)
        );
        images.add(array_image(
            size,
            size,
            faces,
            levels,
            TextureFormat::Rgba8Unorm,
            bytes,
            TextureViewDimension::CubeArray,
        ))
    }
}

/// The page cache's key for `page` (page_cache.rs `key`): its source textures' compressed
/// payloads, and its size, mip levels, format and layer count. Compressed rather than decoded:
/// decompression is deterministic, and hashing the payload is what lets a cache hit skip
/// decompressing the page's textures at all (findings 58 §3.6).
fn page_key(page: &Page, textures: &[Texture], blocks: &[TextureBlock], format: PageFormat, levels: u32) -> u64 {
    let layers: Vec<(u32, u32, &[u8])> = page
        .layers
        .iter()
        .map(|&id| {
            let texture = &textures[id as usize - 1];
            (texture.width, texture.height, blocks[id as usize - 1].bytes.as_slice())
        })
        .collect();
    let params = [
        u64::from(page.width),
        u64::from(page.height),
        u64::from(levels),
        u64::from(page.mips),
        format as u64,
        page.layers.len() as u64,
    ];
    crate::page_cache::key(&params, &layers, page_workers())
}

/// Decompresses one texture payload for a single page layer. A corrupt payload cannot be
/// recovered mid-upload, and the map already passed validation, so this aborts the process
/// with the decoder's message rather than unwinding through the asset sink.
fn decode_block(block: &TextureBlock) -> Vec<u8> {
    skate_data::skate_map::decode_block(block).unwrap_or_else(|error| {
        eprintln!("SKATE world texture decode failed: {error}");
        std::process::abort();
    })
}

/// `format` is a UNORM format, `Rgba8Unorm` or `PageFormat`'s BC3: retail world textures are
/// read raw and squared in the shader, which is the approximate sRGB decode the native renderer
/// used. Uploading them as sRGB would apply the decode twice.
fn array_image(
    width: u32,
    height: u32,
    layers: u32,
    levels: u32,
    format: TextureFormat,
    bytes: Vec<u8>,
    dimension: TextureViewDimension,
) -> Image {
    let mut image = Image::new_uninit(
        Extent3d {
            width,
            height,
            depth_or_array_layers: layers,
        },
        TextureDimension::D2,
        format,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = levels;
    image.data = Some(bytes);
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(dimension),
        ..default()
    });
    // One sampler serves every page; per-channel clamping is a bit in the slot
    // record, applied to the coordinates in the shader.
    let mut sampler = ImageSamplerDescriptor::linear();
    sampler.address_mode_u = ImageAddressMode::Repeat;
    sampler.address_mode_v = ImageAddressMode::Repeat;
    image.sampler = ImageSampler::Descriptor(sampler);
    image
}

/// Unused bindings still need a resource of the right shape.
fn placeholder_page(images: &mut impl AssetSink<Image>) -> Handle<Image> {
    images.add(array_image(
        1,
        1,
        1,
        1,
        TextureFormat::Rgba8Unorm,
        vec![0, 0, 0, 255],
        TextureViewDimension::D2Array,
    ))
}

fn placeholder_cube(images: &mut impl AssetSink<Image>) -> Handle<Image> {
    images.add(array_image(
        1,
        1,
        6,
        1,
        TextureFormat::Rgba8Unorm,
        vec![0, 0, 0, 255].repeat(6),
        TextureViewDimension::CubeArray,
    ))
}

// ---------------------------------------------------------------------------
// Scene detection and plugin
// ---------------------------------------------------------------------------

#[derive(Resource)]
pub(crate) struct RetailScene(pub bool);

impl RetailScene {
    /// Package evidence, never the editable map name. Older Skate 2 exports can
    /// retain native provenance/sky/collision without material definitions.
    pub(crate) fn for_map(map: &SkateMap) -> bool {
        map.materials.iter().any(|m| m.retail_definition.is_some())
            || map
                .extensions
                .iter()
                .any(|e| matches!(&e.tag, b"WMET" | b"RWCM" | b"SKYB"))
            || map.geometry.collision.iter().any(|c| c.native_edges.is_some())
            || map.rails.iter().any(|r| r.native.is_some())
    }
}

#[derive(serde::Deserialize, Resource)]
struct OceanPca {
    hz: f32,
    frames: Vec<[[f32; 4]; 7]>,
}

fn read_pca(root: &std::path::Path) -> Option<OceanPca> {
    let pca = std::fs::read(root.join("private/ocean-pca.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<OceanPca>(&b).ok())?;
    (pca.hz == 30.
        && pca.frames.len() == 30
        && pca.frames.iter().flatten().flatten().all(|v| v.is_finite()))
    .then_some(pca)
}

fn load_pca(mut commands: Commands, config: Res<crate::config::Config>) {
    if let Some(pca) = read_pca(&config.asset_root) {
        info!("RETAIL_OCEAN: loaded 30 authored PCA frames");
        commands.insert_resource(pca);
    }
}

fn initialize_frame_state(mut buffers: ResMut<Assets<ShaderStorageBuffer>>) {
    let mut buffer = ShaderStorageBuffer::new(
        &FrameStateData::default().encode(),
        RenderAssetUsages::RENDER_WORLD,
    );
    // Written in place every frame by `write_frame_state`.
    buffer.buffer_description.usage |= BufferUsages::COPY_DST;
    if let Err(error) = buffers.insert(FRAME_STATE.id(), buffer) {
        // Without this buffer every world bind group is unsatisfiable, so the
        // whole map would render black. Fail loudly.
        error!("could not install the shared frame state buffer: {error}");
    }
}

/// Advances the animation clock and ocean PCA frame. `shadow` stays zero while
/// shadows are out of scope (RFC 1 D5), which keeps the shader off the cascade
/// bindings entirely.
fn advance_frame_state(
    mut state: ResMut<FrameStateData>,
    time: Res<Time>,
    pca: Option<Res<OceanPca>>,
    mut animation: Local<WaterAnimation>,
) {
    state.clock.x = time.elapsed_secs();
    state.clock.z = animation.presentation_frame(time.elapsed_secs_f64());
    if let Some(pca) = pca {
        let frame = usize::from(animation.frame) % pca.frames.len();
        state.pca = pca.frames[frame].map(Vec4::from_array);
        state.clock.y = 1.;
    }
}

/// The timing branch of TU3 0x82790858; initialization is 0x827905B0.
/// The scroll clock advances per update, independently of the supplied dt.
/// The value is published before the >5 reset. PCA advances at most once
/// per update, uses a strict comparison, and its native counter wraps at u16.
/// This preserves the routine's behavior; host update cadence is separate
/// from original-hardware frame pacing.
#[derive(Default)]
struct WaterAnimation {
    scroll: f32,
    remainder: f32,
    frame: u16,
    presentation_tick: u64,
    published_scroll: f32,
}

impl WaterAnimation {
    /// Host adapter: preserve the console presentation cadence independently
    /// of an unlocked PC renderer. The recovered routine below still runs
    /// once per logical presentation tick, with its original f32 operations.
    fn presentation_frame(&mut self, elapsed: f64) -> f32 {
        let target = (elapsed * 30.0) as u64 + 1;
        while self.presentation_tick < target {
            self.published_scroll = self.advance(f32::from_bits(0x3d08_8889));
            self.presentation_tick += 1;
        }
        self.published_scroll
    }

    fn advance(&mut self, dt: f32) -> f32 {
        self.scroll += f32::from_bits(0x3c88_8889);
        let published = self.scroll;
        if self.scroll > 5.0 {
            self.scroll = 0.0;
        }
        self.remainder += dt;
        let period = f32::from_bits(0x3d08_8889);
        if self.remainder > period {
            self.remainder -= period;
            self.frame = self.frame.wrapping_add(1);
        }
        published
    }
}

/// Uploads this frame's state into the one GPU buffer every world material
/// bound at preparation. Replacing the asset's data instead (as before) makes
/// Bevy create a new GPU buffer that no existing material bind group sees, so
/// the clock, ocean PCA and shadow floor stayed at their first values.
fn write_frame_state(
    state: Res<FrameStateData>,
    buffers: Res<RenderAssets<GpuShaderStorageBuffer>>,
    queue: Res<RenderQueue>,
) {
    if let Some(buffer) = buffers.get(FRAME_STATE.id()) {
        queue.write_buffer(&buffer.buffer, 0, &state.encode());
    }
}

#[cfg(test)]
#[path = "retail_shader_tests.rs"]
mod shader_tests;

pub(crate) struct RetailRenderPlugin;

impl Plugin for RetailRenderPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "retail_world.wgsl");
        embedded_asset!(app, "retail_depth.wgsl");
        embedded_asset!(app, "retail_sky.wgsl");
        bevy::shader::load_shader_library!(app, "retail_material_bindings.wgsl");
        app.init_resource::<FrameStateData>()
            .add_plugins((
                MaterialPlugin::<WorldMaterial>::default(),
                MaterialPlugin::<crate::retail_sky::SkyMaterial>::default(),
                crate::retail_character::CharacterLightingPlugin,
                crate::retail_exposure::RetailExposurePlugin,
            ))
            .add_plugins(ExtractResourcePlugin::<FrameStateData>::default())
            .add_systems(Startup, (initialize_frame_state, load_pca))
            .add_systems(Update, advance_frame_state);
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(
                Render,
                write_frame_state.in_set(RenderSystems::PrepareResources),
            );
        }
    }

    // The render device and adapter are in the main world by now (bevy_render's
    // `RenderPlugin::finish` runs first), and the world material's bind group layout is derived
    // later, at `RenderStartup` (bevy_pbr `add_material_bind_group_allocator`).
    fn finish(&self, app: &mut App) {
        read_vis_cull_support(app.world());
    }
}

pub(crate) fn world_changed(
    mut messages: MessageReader<crate::map_transition::WorldChanged>,
) -> bool {
    messages.read().count() != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_chain_len_matches_mip_chain() {
        for (width, height) in [(1, 1), (2, 1), (1, 8), (64, 64), (256, 64), (5, 3), (512, 512)] {
            let face = vec![0u8; width as usize * height as usize * 4];
            let (chain, _) = mip_chain(&face, width, height, 1);
            assert_eq!(chain.len(), mip_chain_len(width, height), "{width}x{height}");
        }
    }

    /// A page's bytes are handed to wgpu as they are, so each layer must be exactly what
    /// `create_texture_with_data` reads: every mip level in turn, BC3 levels in whole 4x4
    /// blocks of 16 bytes even when the level is smaller than a block.
    #[test]
    fn page_layer_len_matches_wgpu_layout() {
        for (width, height) in [(4u32, 4u32), (8, 4), (64, 64), (256, 64), (512, 512), (1024, 256)] {
            let levels = 32 - width.max(height).leading_zeros();
            assert_eq!(
                PageFormat::Rgba8.layer_len(width, height, levels),
                mip_chain_len(width, height),
                "{width}x{height}"
            );
            let blocks: usize = (0..levels)
                .map(|level| {
                    let (w, h) = ((width >> level).max(1), (height >> level).max(1));
                    w.div_ceil(4) as usize * h.div_ceil(4) as usize * 16
                })
                .sum();
            assert_eq!(PageFormat::Bc3.layer_len(width, height, levels), blocks, "{width}x{height}");
        }
        assert_eq!(PageFormat::for_page(true, 64, 64), PageFormat::Bc3);
        assert_eq!(PageFormat::for_page(true, 64, 30), PageFormat::Rgba8);
        assert_eq!(PageFormat::for_page(false, 64, 64), PageFormat::Rgba8);
    }

    /// Each level is encoded from its own part of the chain into its own part of the layer:
    /// give every level a different flat colour, which BC3 keeps exactly when its channels are
    /// 0 or 255, and decode each level back from where wgpu will read it.
    #[test]
    fn bc3_page_keeps_each_level_in_place() {
        let (width, height) = (16u32, 8u32);
        let levels = 32 - width.max(height).leading_zeros();
        let colour = |level: u32| [[255, 0, 0, 255], [0, 255, 0, 0], [0, 0, 255, 255], [255, 255, 0, 0], [0, 255, 255, 255]][level as usize];
        let mut chain = Vec::new();
        for level in 0..levels {
            let (w, h) = ((width >> level).max(1), (height >> level).max(1));
            chain.extend(colour(level).repeat(w as usize * h as usize));
        }
        let mut layer = vec![0; PageFormat::Bc3.layer_len(width, height, levels)];
        PageFormat::Bc3.encode(&chain, width, height, levels, &mut layer);
        let mut at = 0;
        for level in 0..levels {
            let (w, h) = ((width >> level).max(1) as usize, (height >> level).max(1) as usize);
            let len = PageFormat::Bc3.level_len(w as u32, h as u32);
            let mut decoded = vec![0; w * h * 4];
            texpresso::Format::Bc3.decompress(&layer[at..at + len], w, h, &mut decoded);
            assert!(decoded.chunks_exact(4).all(|texel| texel == colour(level)), "level {level}");
            at += len;
        }
        assert_eq!(at, layer.len());
    }

    /// The per-vertex material index only works if no vertex is shared between
    /// triangles of different materials. `main` never had to care: it split
    /// geometry by material, so each partition re-indexed its own vertices.
    /// Merged geometry keeps the source indices, and `@interpolate(flat)` reads
    /// one provoking vertex, so a shared vertex would shade a whole triangle
    /// with a neighbour's material — every symptom of shuffled textures.
    #[test]
    #[ignore = "Requires private installed assets; diagnostic only"]
    fn no_vertex_is_shared_between_materials() {
        let path = std::path::PathBuf::from(
            std::env::var("SKATE_BUDGET_MAP").expect("SKATE_BUDGET_MAP"),
        );
        let map = skate_data::skate_map::SkateMap::load(&path).unwrap();
        let mut conflicts = 0usize;
        let mut corner_disagreements = 0usize;
        for tri in map.geometry.indices.chunks_exact(3) {
            let materials = [
                map.geometry.vertices[tri[0] as usize].material,
                map.geometry.vertices[tri[1] as usize].material,
                map.geometry.vertices[tri[2] as usize].material,
            ];
            if materials[0] != materials[1] || materials[1] != materials[2] {
                corner_disagreements += 1;
            }
        }
        // A vertex referenced by triangles whose corner-0 materials differ.
        let mut owner: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for tri in map.geometry.indices.chunks_exact(3) {
            let material = map.geometry.vertices[tri[0] as usize].material;
            for &index in tri {
                match owner.get(&index) {
                    Some(&existing) if existing != material => conflicts += 1,
                    _ => {
                        owner.insert(index, material);
                    }
                }
            }
        }
        eprintln!(
            "VERTEX_SHARING triangles={} vertices={} corner_disagreements={} cross_material_refs={}",
            map.geometry.indices.len() / 3,
            map.geometry.vertices.len(),
            corner_disagreements,
            conflicts
        );
        assert_eq!(
            corner_disagreements, 0,
            "a triangle spans materials, so corner 0 is not authoritative"
        );
        assert_eq!(
            conflicts, 0,
            "vertices are shared across materials; flat interpolation cannot carry \
             a per-vertex material index without splitting them"
        );
    }

    /// `Vertex::material` is one-based with zero meaning "no material". Reading
    /// it as a direct index shades every surface with the *next* material's
    /// textures, which renders cleanly at full frame rate and is invisible to
    /// draw-count and shader-validation tests - it just puts the wrong texture
    /// on almost every object. The one-based reading is what makes the highest
    /// index reach `materials.len()` rather than stop one short.
    #[test]
    #[ignore = "Requires private installed assets"]
    fn vertex_material_indices_are_one_based() {
        let path = std::path::PathBuf::from(
            std::env::var("SKATE_BUDGET_MAP").expect("SKATE_BUDGET_MAP"),
        );
        let map = skate_data::skate_map::SkateMap::load(&path).unwrap();
        let highest = map
            .geometry
            .vertices
            .iter()
            .map(|v| v.material)
            .max()
            .expect("geometry has vertices");
        let lowest = map
            .geometry
            .vertices
            .iter()
            .map(|v| v.material)
            .filter(|&m| m != 0)
            .min()
            .expect("geometry references a material");
        eprintln!(
            "MATERIAL_BASE materials={} lowest={lowest} highest={highest}",
            map.materials.len()
        );
        assert_eq!(
            highest as usize,
            map.materials.len(),
            "the highest material index should reach materials.len() under a \
             one-based reading; a zero-based reading would stop one short"
        );
        assert_eq!(lowest, 1, "one-based indices start at 1");
    }

    /// Reports what `Binding::texture` actually contains, because the reference
    /// delegated that decision to a caller-supplied closure and the two callers
    /// disagreed: one treats the value as a global 1-based texture id, the other
    /// as a slot into the material's own five-entry list. Guessing wrong
    /// substitutes one real texture for another, which renders cleanly and looks
    /// like the textures were shuffled.
    #[test]
    #[ignore = "Requires private installed assets; diagnostic only"]
    fn binding_texture_ids_are_global_not_material_local() {
        let path = std::path::PathBuf::from(
            std::env::var("SKATE_BUDGET_MAP").expect("SKATE_BUDGET_MAP"),
        );
        let map = skate_data::skate_map::SkateMap::load(&path).unwrap();
        let mut histogram: BTreeMap<u32, usize> = BTreeMap::new();
        let mut roles: BTreeMap<String, (u32, u32, usize)> = BTreeMap::new();
        let mut with_definition = 0usize;
        let mut agrees_with_local = 0usize;
        let mut agrees_with_global = 0usize;
        for material in &map.materials {
            let Some(definition) = material
                .retail_definition
                .as_deref()
                .and_then(Definition::parse)
            else {
                continue;
            };
            with_definition += 1;
            for (role, binding) in &definition.bindings {
                *histogram.entry(binding.texture.min(64)).or_default() += 1;
                let entry = roles.entry(role.clone()).or_insert((u32::MAX, 0, 0));
                entry.0 = entry.0.min(binding.texture);
                entry.1 = entry.1.max(binding.texture);
                entry.2 += 1;
            }
            // If ids are material-local, the diffuse binding indexes
            // `material.textures`; if global, it is already a texture id.
            if let Some(diffuse) = definition.bindings.get("diffuse") {
                if (diffuse.texture as usize) < material.textures.len()
                    && material.textures[diffuse.texture as usize] != 0
                {
                    agrees_with_local += 1;
                }
                if diffuse.texture as usize <= map.textures.len() && diffuse.texture != 0 {
                    agrees_with_global += 1;
                }
            }
        }
        eprintln!(
            "BINDING_IDS materials={} with_definition={} textures={} diffuse_plausible_local={} diffuse_plausible_global={}",
            map.materials.len(),
            with_definition,
            map.textures.len(),
            agrees_with_local,
            agrees_with_global
        );
        for (role, (min, max, count)) in &roles {
            eprintln!("BINDING_ROLE {role} min={min} max={max} count={count}");
        }
        let capped: usize = histogram.iter().filter(|(k, _)| **k >= 64).map(|(_, v)| *v).sum();
        eprintln!(
            "BINDING_SMALL_IDS below_five={} at_or_above_64={}",
            histogram.iter().filter(|(k, _)| **k < 5).map(|(_, v)| *v).sum::<usize>(),
            capped
        );
        assert_eq!(
            agrees_with_local, 0,
            "binding ids are not slots into the material's own texture list"
        );
        assert_eq!(
            agrees_with_global, with_definition,
            "every diffuse binding should be a valid one-based global texture id"
        );
    }

    #[test]
    fn native_water_clock_preserves_boundaries_and_hitches() {
        let mut clock = WaterAnimation::default();
        let period = f32::from_bits(0x3d08_8889);
        assert_eq!(clock.advance(period).to_bits(), 0x3c88_8889);
        assert_eq!(clock.frame, 0); // Equal is not greater.
        clock.advance(period);
        assert_eq!(clock.frame, 1);
        clock.advance(1.0);
        assert_eq!(clock.frame, 2); // A hitch does not run a catch-up loop.
        clock.frame = u16::MAX;
        clock.advance(period);
        assert_eq!(clock.frame, 0);
        clock.scroll = 5.0;
        assert!(clock.advance(0.0) > 5.0); // Publish, then reset.
        assert_eq!(clock.scroll, 0.0);
        assert_eq!(clock.advance(0.0).to_bits(), 0x3c88_8889);
    }

    #[test]
    fn water_animation_is_independent_of_host_render_rate() {
        let mut fast = WaterAnimation::default();
        let mut slow = WaterAnimation::default();
        for i in 0..=400 { fast.presentation_frame(f64::from(i) / 400.0); }
        for i in 0..=30 { slow.presentation_frame(f64::from(i) / 30.0); }
        assert_eq!(fast.presentation_tick, slow.presentation_tick);
        assert_eq!(fast.frame, slow.frame);
        assert_eq!(fast.published_scroll.to_bits(), slow.published_scroll.to_bits());
        assert_eq!(fast.remainder.to_bits(), slow.remainder.to_bits());
    }

    #[test]
    fn gpu_struct_sizes_match_the_shader() {
        let mut bytes = Vec::new();
        WorldParams::default().encode(&mut bytes);
        assert_eq!(bytes.len(), WorldParams::SIZE);
        assert_eq!(WorldParams::SIZE, 208);

        let mut bytes = Vec::new();
        MaterialSlots::default().encode(&mut bytes);
        assert_eq!(bytes.len(), MaterialSlots::SIZE);

        assert_eq!(FrameStateData::default().encode().len(), FrameStateData::SIZE);

    }

    #[test]
    fn slot_packing_round_trips() {
        let packed = pack_slot(5, 1234, true);
        assert_eq!((packed >> 16) & 0xff, 5);
        assert_eq!(packed & 0xffff, 1234);
        assert_ne!(packed & 0x8000_0000, 0);
        assert_ne!(packed, ABSENT);
        assert_eq!((ABSENT >> 16) & 0xff, 0xff);
    }

    #[test]
    fn identical_textures_share_one_layer() {
        let texture = |name: &str, color_space| skate_data::skate_map::Texture {
            name: name.into(),
            width: 1,
            height: 1,
            color_space,
        };
        let block = |pixel| skate_data::skate_map::TextureBlock {
            expected: 4,
            method: 0,
            bytes: vec![pixel, 0, 0, 255],
        };
        let textures = vec![
            texture("first", 1),
            texture("duplicate", 1),
            texture("linear", 0),
            texture("different", 1),
        ];
        let blocks = vec![block(30), block(30), block(30), block(31)];
        assert_eq!(canonical_texture_ids(&textures, &blocks), [0, 1, 1, 3, 4]);
    }

    #[test]
    fn weathering_tuning_preserves_artwork() {
        for name in [
            "decal_WEAR_WaterStain_01",
            "subway_grunge03",
            "decal_GrimePuddle",
            "OT_Ground_decals",
        ] {
            assert_eq!(stain_opacity(name), 0.35);
        }
        for name in [
            "decal_Graphic_SP_UN_Shark_01",
            "decal_other_sp_arrowramps_01",
            "decal_Wear_GL_UN_MPedge_01",
            "",
        ] {
            assert_eq!(stain_opacity(name), 1.);
        }
    }

    #[test]
    fn resample_averages_rather_than_dropping_texels() {
        // 2x2 white/black checker to 1x1 must average, not pick a corner.
        let rgba = vec![
            255, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        assert_eq!(resample(&rgba, 2, 2, 1, 1), vec![127, 127, 127, 255]);
    }

    #[test]
    fn world_features_follow_the_fragment_gates() {
        use super::world_features::*;
        // University's cutout materials take only the lightmap (flag 32), and of their
        // families only 7 takes the skater's shadow.
        assert_eq!(of(9, 32), 0);
        assert_eq!(of(10, 32), 0);
        assert_eq!(of(7, 32), SHADOW);
        // Only families 3 and 4 have the decal, and only the reflective ones the constant
        // detail texel and the environment cube.
        assert_eq!(of(5, 0xff), ALL & !DECAL);
        assert_eq!(of(13, 0xff), ALL & !DECAL);
        assert_eq!(of(3, 0xff), ALL & !(LOAD_DETAIL | ENVIRONMENT));
        // Family 2 has no detail map, and families 7 and 8 no normal map.
        assert_eq!(of(2, 1 | 2 | 16 | 32), SHADOW | NORMAL | SPECULAR);
        assert_eq!(of(8, 1 | 2), SHADOW | DETAIL);
        // Family 16 (see-through fences) takes the shadow like 13, but none of 13's normal
        // map, constant detail texel or environment cube.
        assert_eq!(of(16, 16 | 32), SHADOW | SPECULAR);
        assert_eq!(of(16, 0xff), ALL & !(NORMAL | LOAD_DETAIL | DECAL | ENVIRONMENT));
        // The water families' own branches test their channels themselves, so only the
        // lookups every family shares count for them.
        assert_eq!(of(31, 1 | 4 | 32 | 64 | 128), MACRO);
        assert_eq!(of(31, 4 | 16 | 32 | 64), MACRO | SPECULAR);
        assert_eq!(of(14, 0), 0);
    }

    #[test]
    fn classes_split_on_culling_blending_and_alpha_testing() {
        assert_eq!(
            RenderClass::new(false, false, false),
            RenderClass::Opaque,
            "no blending, no cutoff and back-face culling is the plain case"
        );
        assert_eq!(RenderClass::new(false, true, false), RenderClass::Cutout);
        assert_eq!(
            RenderClass::new(true, true, true),
            RenderClass::BlendedTwoSided,
            "blending subsumes the cutoff rather than splitting again"
        );
        assert!(RenderClass::OpaqueTwoSided.two_sided());
        assert!(RenderClass::CutoutTwoSided.two_sided());
        assert!(!RenderClass::Cutout.two_sided());
        assert_eq!(RenderClass::ALL.len(), 6);
    }

    /// The whole point of the split: only the opaque classes may drop the
    /// `discard`, and they must not claim an alpha mode that makes Bevy set
    /// MAY_DISCARD for them anyway.
    #[test]
    fn only_opaque_classes_give_up_the_discard() {
        for class in RenderClass::ALL {
            let opaque = matches!(class, RenderClass::Opaque | RenderClass::OpaqueTwoSided);
            assert_eq!(class.discards(), !opaque, "{class:?}");
            assert_eq!(
                class.alpha_mode() == AlphaMode::Opaque,
                opaque,
                "{class:?} must report Opaque exactly when it drops the discard"
            );
        }
    }

    /// A retail definition record with the shader name, stored family and
    /// diffuse / lightmap / specular bindings, no parameters.
    fn transparent_definition(shader: &str, family: u32) -> Vec<u8> {
        fn text(out: &mut Vec<u8>, value: &str) {
            out.extend_from_slice(&(value.len() as u32).to_le_bytes());
            out.extend_from_slice(value.as_bytes());
        }
        let mut out = vec![0u8; 16];
        text(&mut out, shader);
        for value in [family, 2, 3] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        for (i, role) in ["diffuse", "lightmap", "specular"].into_iter().enumerate() {
            text(&mut out, role);
            for value in [i as u32 + 1, 0, 0, 0] {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        out.extend_from_slice(&0u32.to_le_bytes());
        text(&mut out, "");
        out
    }

    #[test]
    fn transparent_environment_takes_its_own_family() {
        // Packages exported before the converter knew the family stored 0 and
        // drew chain-link fences as opaque family 1.
        for stored in [0, TRANSPARENT_ENVIRONMENT_FAMILY] {
            let bytes = transparent_definition("environment.transparent", stored);
            let definition = Definition::parse(&bytes).expect("parses");
            assert_eq!(definition.family, TRANSPARENT_ENVIRONMENT_FAMILY, "stored {stored}");
            assert!(definition.supported(&MaterialTuning::default()));
        }
        // Other shaders keep their stored family.
        let reflective = Definition::parse(&transparent_definition("environment.reflective_trans", 13)).unwrap();
        assert_eq!(reflective.family, 13);
        let unknown = Definition::parse(&transparent_definition("incandescent.transparent", 0)).unwrap();
        assert_eq!(unknown.family, 0);
    }

    #[test]
    fn transparent_environment_shader_matches_retail() {
        let src = include_str!("retail_world.wgsl");
        // transparentenvironment_defaultPS 39, 43, 53: alpha-scaled light, alpha^2 out.
        assert!(src.contains("if fam == 13u || fam == 16u { lin = lml*d*a.a; }"));
        assert!(src.contains("if fam == 13u || fam == 16u { alpha *= alpha; }"));
        assert!(src.contains("if fam >= 7u { alpha = a.a; }"));
        // Shadowed lightmap and the m_params multiplier like the other world families.
        assert!(src.contains("(fam<=8u || fam==13u || fam==16u)"));
        assert!(src.contains("if fam <= 8u || fam == 13u || fam == 16u { fog_a *= p.surface.w; }"));
        // No normal map read: the retail program fetches none. Under `WORLD_SHORT_LIVES` the
        // read has a block of its own, its test wrapped onto the line before.
        let lines: Vec<&str> = src.lines().collect();
        let reads: Vec<&str> = (0..lines.len())
            .filter(|&at| lines[at].contains("sample_normal_map(*channels,i.uv,g)"))
            .map(|at| if lines[at].contains("if ") { lines[at] } else { lines[at - 1] })
            .collect();
        assert_eq!(reads.len(), 2, "{reads:?}");
        for read in reads {
            assert!(read.contains("(fam <= 6u || fam == 13u)") && !read.contains("16u"), "{read}");
        }
    }
}

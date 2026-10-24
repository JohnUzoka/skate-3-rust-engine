# Local Bevy patch

## Bindless material resource reuse

`bevy_pbr/src/material_bind_groups.rs` chooses a fitting slab requiring the fewest
new resource slots instead of first-fit allocation. It prefers already resident
textures, then fuller slabs, while keeping existing allocation/refcount/free logic
and capacity limits. This adds work during material allocation, not each frame.
The game's explicit Vulkan shader probe tests reuse and retirement using real GPU
texture identities. See `docs/cpu-followup-optimizations.md` for scope and limits.

## Conservative occlusion depth pyramid

`bevy_core_pipeline` is vendored from crates.io 0.18.1 with its original licenses.
Its `src/experimental/mip_generation/{mod.rs,downsample_depth.wgsl}` backports
[Bevy #22603](https://github.com/bevyengine/bevy/pull/22603), commit
`43f4552308a29f4130c3ad71ea65d48f77daea6e`. The upstream file paths changed
between releases; the algorithm is the upstream fix. Power-of-two pyramid
levels and conservative source sampling prevent false occlusion at arbitrary
viewport sizes, including scaled targets and MSAA. The associated meshlet
texture-dimension fix is included in `bevy_pbr/src/meshlet/meshlet_cull_shared.wgsl`.
The game uses ordinary meshes, not meshlets.

The late mesh preprocessing pass in 0.18.1 also binds its indirect dispatch
buffer as writable storage, which fails wgpu validation when occlusion is on.
`bevy_pbr/src/render/{gpu_preprocess.rs,mesh_preprocess.wgsl}` now declares that
binding read-only in the late pass. Its counters are plain `u32` in that shader
variant, preserving the same buffer layout; the early producer retains atomic
counters and writable storage. No dispatch count or culling decision changes.

Unused depth-pyramid mip slots previously shared one dummy storage texture.
When the pyramid was short enough that the ReadWrite SPD intermediate (`mips[5]`)
was also a dummy, wgpu rejected the dispatch: `STORAGE_WRITE_ONLY` and
`STORAGE_READ_WRITE` on the same texture in one usage scope. Padding now uses one
dummy texture per binding slot, dummies are 1×1 (not 0×0), zero-sized viewports
clamp safely, and pyramid side lengths are at least 32 so that intermediate mip
exists as a real subresource.

Occlusion culling also leaves alone every mesh with `NoFrustumCulling`
(`bevy_pbr/src/render/mesh_preprocess.wgsl`, "Are we culled out?"). In 0.18.1, and
on Bevy's main branch at `f0391870df745bd4904218c3270e09808eabd9b9`
(`mesh_preprocess.wesl`), the frustum test honours that flag but the occlusion
test doesn't. It tests such a mesh's `Aabb` anyway. The game sets the flag on the
skater's skinned meshes, whose bounds are the bind pose's (`animation.rs`
`disable_skater_culling`), and on the sky dome, which the vertex shader moves with
the camera (`retail_sky.rs`). A mesh with the flag from the start gets no `Aabb`
(bevy_camera `calculate_bounds`), and Bevy stands in infinite extents
(`MeshCullingData::new`), whose corners project to infinities and NaNs. Without
this, a skater or sky whose stale bounds are hidden could vanish. The game only
turns occlusion culling on under `SKATE_OCCLUSION=1`.

`bevy_pbr` is the unmodified crates.io 0.18.1 source except for the changes
described below. Its original MIT and Apache licenses are included. Cargo selects
it through the workspace `[patch.crates-io]` entry; the Cargo registry is untouched.

## Cached lightmap bindings

Bevy 0.18.1 rebuilds a bind group for every allocated lightmap slab and every
render phase on each frame, even when all bound GPU resources are unchanged.
On San Vanelona, mesh binding preparation measured roughly 10 ms per frame.

Local changes are restricted to:

- `src/lightmap/mod.rs`: each slab retains at most eight bindings. Lookup compares
  the actual wgpu buffer identity, binding offset and size, and layout identity.
  Texture insertion/removal clears the cache. FIFO eviction bounds retained old
  buffers when phases disappear or instance buffers resize.
- `src/render/mesh_bindings.rs`: reuse or create the binding through that cache.
  The lightmap slab parameter is now mutable.
- `src/lightmap/cache_tests.rs`: explicit GPU regression test.
- `src/render/mesh.rs`: retain complete lookup tables for both alternating
  instance buffers per phase. Keys include buffer identity, offset, size, layout,
  and the lightmap binding revision. A slab creation, texture upload, or removal
  changes that revision. Tables are shared by `Arc`, so a cache hit avoids
  rebuilding and cloning thousands of individual entries. Keep only two tables
  per phase, and discard caches for phases that disappear.
- `src/render/mesh.rs`: cache the IDs of meshes with morph targets when
  `RenderAssets<RenderMesh>` changes. Each phase still rebuilds its dynamic morph
  bindings from the current GPU resources, but skips scanning static city meshes.

Writing new instance data into an existing buffer does not require rebuilding a
bind group. Changing the buffer, bound range, layout, or texture does. The patch
does not skip extraction, uploads, animation, or visibility updates. It retains
the existing single-texture and binding-array rendering paths and shaders.

Run the Vulkan test explicitly:

```powershell
cargo test -p bevy_pbr --lib cache_tracks_binding_identity_and_texture_replacement --locked -- --ignored
```

It covers cloned versus replaced buffers, changed binding ranges/layouts, writes
to an existing buffer, texture insertion/removal, bounded eviction, and actual
bind-group construction/reuse, alternating-buffer table reuse, table revisions,
and morph-target additions/replacements/removals. Regular game tests and a city screenshot/benchmark
also exercise the patched renderer.

Upstream references reviewed September 6, 2026:

- [Bevy issue 23595](https://github.com/bevyengine/bevy/issues/23595) reports
  increasing `prepare_mesh_bind_groups` cost. That report points at mesh scanning;
  local measurements and source inspection identified lightmap binding recreation
  as the opportunity addressed here.
- [Bevy 0.19 rendering improvements](https://bevy.org/news/bevy-0-19/#render-big-scenes-faster)
  explain further GPU batching and CPU overhead reductions. An engine migration
  is separate from this small patch; 0.19.1 source still rebuilds lightmap groups.

When upgrading Bevy, reassess this patch and rerun the GPU test, stationary and
moving-camera benchmarks, and visual comparison. Do not assume an unchanged
private renderer implementation across engine versions.

## Skippable passes and re-extraction (Switch port)

Three opt-in changes, each inert unless the game asks for it, made while
measuring the Switch's GPU frame:

- `bevy_core_pipeline/src/prepass/{mod.rs,node.rs}`: with the
  `SkipPrepassDepthCopy` resource in the render world, the prepass no longer
  copies the view's depth into the prepass depth texture after each prepass.
  That copy exists for shaders that sample `depth_prepass_texture`
  (`prepass_utils::prepass_depth`: SSR, forward decals and the like). The
  game samples it nowhere, and the occlusion depth pyramid reads the view's
  own depth texture. The texture stays bound and keeps its last contents.
- `bevy_pbr/src/render/light.rs` and `src/lib.rs`: with the
  `SkipClearShadowLayers` resource in the render world, a directional
  cascade whose shadow phase is empty is not run again once its layer holds
  only the clear value. Each shadow pass clears its layer, so an empty
  cascade otherwise clears the same layer again every frame and draws
  nothing. `prepare_lights` tags each cascade view with its `ShadowLayer`
  (texture ID and array layer). `skip_clear_shadow_layers` runs after the
  phases are batched. It treats a layer as clear from the frame after a pass
  cleared it without drawing, until a pass draws into it or a frame goes by
  without the layer in use. Cascades with occlusion culling always run.
  Texture IDs are never reused, so a recreated shadow map starts unclaimed.
- `bevy_pbr/src/render/mesh.rs`: with `SKATE_REEXTRACT_UNALLOCATED=1`, a mesh
  instance whose mesh has no allocator slice yet is extracted again next
  frame rather than keeping the zero vertex and index ranges until something
  else changes its `Mesh3d`.

## Finer occlusion tests, depth-pyramid timers, one pyramid build and a tiled first pass (Switch port)

- `bevy_pbr/src/render/{occlusion_culling.wgsl,mesh_preprocess.wgsl,gpu_preprocess.rs}`:
  Bevy's occlusion test reads a 2×2 block of depth-pyramid texels at the level
  where a mesh's screen rectangle is at most one texel wide, so every occluder
  must cover up to four times the rectangle's width each way. With
  `OCCLUSION_TEST_SPAN` (a `pub static AtomicU32`, re-exported as
  `bevy::pbr::OCCLUSION_TEST_SPAN`) set to N > 0, `occluded_finely` instead
  reads every texel the rectangle touches at the finest level where it spans at
  most N texels each way, up to (N + 1)², stopping at the first one the mesh
  isn't behind. Each pyramid texel holds the farthest depth below it, so this
  culls whatever the 2×2 test culls and more, by the same conservative rule:
  a mesh is culled only when its nearest point is behind the farthest depth
  everywhere its rectangle could draw. The rectangle is clamped to the pyramid,
  as what lies off it isn't on screen, and the loop to N + 2 texels each way,
  which only non-finite UVs could exceed. The span is a second push constant
  beside the late-phase offset (range `0..8`), so it can change at run time
  without re-specialising the cached preprocessing pipelines. Zero, the
  default, keeps Bevy's test. The game sets it from `SKATE_OCCLUSION_SPAN`.
- The same files: with `OCCLUSION_TEST_BATCH` (a `pub static AtomicBool`, re-exported as
  `bevy::pbr::OCCLUSION_TEST_BATCH`) set as well, `occluded_finely_batched` reads
  the same texels eight at a time along each row, repeating a row's last texel
  past its end, and compares the mesh's depth with the farthest of each eight, as
  `get_occluder_depth` takes the farthest of its four. For any pyramid without
  NaNs, which a pyramid of rasterised depths can't hold, it gives
  `occluded_finely`'s answer, and a NaN depth is still never culled. With no
  compare between them, the eight reads can all be in flight at once; NAK issues
  them so only with its `plat` flag (bevy-horizon's mesa patch 0015, on by
  default in Switch builds since patch 0019), and otherwise two at a time. The
  flag is bit 16 of the span's push constant, whose low 16 bits hold the span,
  so it also changes at run time without re-specialising. The game sets it from
  `SKATE_OCCLUSION_BATCH`, on unless that is 0.
- `mesh_preprocess.wgsl`: the late phase, and preprocessing without occlusion
  culling, look up the mesh's previous model matrix after the occlusion test
  rather than before it, since only the early phase's test uses it. The lookup
  and what is written are unchanged, but the late phase no longer holds the
  matrix's twelve floats in registers across its test, and culled meshes skip
  its loads. `NAK_DEBUG=stats` reports the late phase's shader at 48 registers
  rather than 56, room for 40 warps a multiprocessor rather than 36.
- `bevy_core_pipeline/src/experimental/mip_generation/mod.rs`: each
  depth-pyramid build records a GPU pass span named after its node
  (`EarlyDownsampleDepth`, `LateDownsampleDepth`), as Bevy's mesh
  preprocessing passes already do. No dispatch changes.
- The same file: while `SKIP_LATE_DEPTH_PYRAMID` (a `pub static AtomicBool`) is
  set, the `LateDownsampleDepth` node returns before building anything. Bevy
  builds the pyramid twice a frame: after the early prepass, for the late
  phase's test, and at the end of the main pass, from the finished depth, for
  the next frame's early test. The pyramid is the same `TextureCache` texture
  from frame to frame (Bevy's early test relies on that), so skipping the
  second build leaves the first for the next frame. That pyramid lacks only the
  meshes drawn in the late phase, so the next early test can only keep more
  meshes, which the late phase then draws as usual. The game sets it from
  `SKATE_PYRAMID_ONCE`.
- The same file and `downsample_depth.wgsl`: while `PYRAMID_TILE` (a `pub static AtomicBool`)
  is set, a single-sampled view builds the pyramid's levels 1 and 2 with the
  `SKATE_PYRAMID_TILE` variant of the first pass. Bevy's `reduce_load_mip_0` takes four
  gathers for each level 1 texel, sixteen a thread, and NAK waits for each before the next.
  In the variant, the workgroup's 256 threads first gather, between them, the block of depth
  its 64×64 virtual texels read into a workgroup array (`depth_tile`, 2,304 floats): 504
  gathers at 1280×720 instead of 4,096. Each thread then takes, for each of its four level 1
  texels, the minimum over the texels Bevy's gathers read, two or three a side and clamped to
  the depth buffer as the sampler clamps (`tile_footprint_min`); their minimum is its level 2
  texel. From there the first pass, and the second, go on as Bevy's do. The footprints come from exact
  arithmetic: at 1280×720 and 640×360 they are the texels the hardware reads, so the pyramid is
  Bevy's; at other sizes they can differ from the hardware's by a texel at an edge but still
  cover the texel's whole region, so the test stays conservative. A view
  whose largest block wouldn't fit the array (`depth_tile_fits`, by the shader's own
  arithmetic) or whose depth is under 32 texels a side keeps Bevy's pass, with one warning, as
  does every view until the variant has compiled. 1280×720, 960×540 and 1920×1080 fit; 1600×900
  doesn't. The game sets it from `SKATE_PYRAMID_TILE`.
- The same files: while `PYRAMID_BLOCK` (a `pub static AtomicBool`) is set, a single-sampled
  view whose pyramid the tile isn't building builds levels 1 and 2 with the
  `SKATE_PYRAMID_BLOCK` variant of the first pass. Each thread takes a 2×2 block of level 1
  texels, rather than four texels 16 apart, and gathers the depth beneath the block in 2×2
  blocks side by side: two a side, or three on an axis where three virtual texels can span two
  depth texels (three times the size above twice the virtual size). That is six gathers a thread
  at 1280×720 and 1920×1080 rather than sixteen. No gather's coordinates depend on another's
  result, and at 1280×720 NAK waits for them in three rounds rather than one by one. Each level 1
  texel is the minimum over the texels Bevy's gathers read, picked by the first texel each
  virtual texel's gather reads (`first_gathered_texel`, `block_min_first`, `block_min_second`
  and `block_min_third`). The four texels' minimum is the thread's level 2 texel, which goes to
  `intermediate_memory` for the rest of the pass, as the tile's does. The texels come from the
  same exact arithmetic as the tile's. At 1280×720, 1280×800 and 640×360 they are the texels the
  hardware reads, so the pyramid is Bevy's; at other sizes they still cover each texel's region.
  A model of the selection checks both on each axis for every size from 1 to 4,096. With the
  Switch build's NAK flags, `NAK_DEBUG=stats` reports the variant's first pass at 525
  instructions, 2,087 static cycles and 40 registers (48 warps a multiprocessor), against
  Bevy's 689, 3,341 and 32 (64). Every view keeps Bevy's pass until the variant has compiled.
  The game sets it from `SKATE_PYRAMID_BLOCK`.

## Direct late preprocessing and fewer indirect-parameter builds (Switch port)

- `bevy_pbr/src/render/gpu_preprocess.rs`: while `LATE_PREPROCESS_DIRECT` (a
  `pub static AtomicBool`, re-exported as `bevy::pbr::LATE_PREPROCESS_DIRECT`)
  is set, the late mesh preprocessing node dispatches each view and phase
  directly, with `late_indexed.len()` (or `late_non_indexed.len()`) threads
  rounded up to whole workgroups, rather than indirectly with the count the
  early phase wrote. `PreprocessWorkItemBuffers::push` reserves a late slot for
  every early work item, and the early phase adds at most one late work item
  for each of its own, so that length bounds the count. The late shader returns
  at once for a thread at or past the count (`mesh_preprocess.wgsl`,
  `LATE_PHASE`), so the same work items are processed. On the Switch, NVK
  stalls the command stream before every indirect dispatch so that it can fetch
  the dispatch's size from memory (`nvk_CmdDispatchIndirect2KHR` before
  Turing); a direct dispatch's size is in the command stream. The game sets it
  from `SKATE_LATE_PREPROCESS_DIRECT`, on unless that is 0.
- The same file: while `INDIRECT_PHASE_FILTER` (a `pub static AtomicBool`,
  re-exported as `bevy::pbr::INDIRECT_PHASE_FILTER`) is set, each of the three
  indirect-parameter building nodes skips the phases nothing draws before the
  next build rebuilds them (`skips_indirect_parameters`). The early and late
  prepass builds skip `Opaque3d`, `AlphaMask3d`, `Transmissive3d` and
  `Transparent3d`, which only passes after the main build draw. The main build
  skips `Opaque3dPrepass`, `AlphaMask3dPrepass`, `Opaque3dDeferred`,
  `AlphaMask3dDeferred` and `Shadow`, whose passes all come before it in the
  graph. Each build resets and recounts its own phases' batch sets and reads
  only what preprocessing wrote, so nothing reads what a skipped build would
  have written. Phases it doesn't name, such as 2D ones, are built every time.
  The main build skips only in a frame with a view the early build runs for
  (one with `DepthPrepass`, or a shadow view: its node's `early_view_query`),
  so a camera with a normal, motion-vector or deferred prepass but no
  `DepthPrepass`, in a scene without shadows, has its prepass phases built by
  the main build, as in Bevy. The main build runs whatever the views, and
  `DrawMesh` draws nothing until all three builds' pipelines have compiled
  (`PreprocessPipelines::pipelines_are_loaded`), so the main pass's phases are
  always built before it draws them. The game sets it from
  `SKATE_INDIRECT_PHASE_FILTER`, on unless that is 0.

## Per-triangle culling (Switch port)

- `bevy_pbr/src/render/{triangle_cull.rs,triangle_cull.wgsl,triangle_cull_v2.wgsl,
  triangle_cull_clusters.wgsl}` (new), with hooks in
  `gpu_preprocess.rs` (plugin `build` and `finish`), `mesh.rs` (`DrawMesh`, and
  `MeshInputUniform::pad`) and `mod.rs`:
  while `TRIANGLE_CULL` (a `pub static AtomicU32`, re-exported as `bevy::pbr::TRIANGLE_CULL`)
  is non-zero, a compute pass between `MainBuildIndirectParameters` and `StartMainPass` tests
  each triangle of every mesh that `Opaque3d` or `AlphaMask3d` draws whose `MeshTag` has
  `TRIANGLE_CULL_LEAF`. It writes the survivors' indices, in their original order, to a
  mirror of the mesh's index slab at the mesh's own offsets, and lowers the draw's
  `index_count` to match. `DrawMesh` binds the mirror for exactly those draws. A triangle is
  dropped when it lies wholly outside one clip plane, faces away (unless the tag also has
  `TRIANGLE_CULL_TWO_SIDED`), covers no pixel centre (single-sample views only), or is behind
  the depth pyramid, using `occluded_finely` on its own rectangle with the low 8 bits as the
  span. The pyramid test is skipped under `TRIANGLE_CULL_NO_DEPTH` or when the view has no
  pyramid. A triangle that crosses the near plane is kept. A draw of several instances is copied
  whole. The prepass, the shadow passes, `Transparent3d` and every untagged mesh draw as before.
- The tagged meshes may fill several slabs (the game's world needs two 64 MiB vertex slabs):
  there is one dispatch per pair of vertex and index slabs that holds any, and one mirror per
  index slab. `MeshInputUniform::pad`, unused upstream, carries each mesh's vertex and index
  slab ids (16 bits each), so a dispatch touches only the draws from its own slabs, and a mesh
  that arrives in a pair already covered is culled from its first frame. `DrawMesh` swaps only
  when the draw's vertex and index buffers are a covered pair's.
- The pass runs only while it can be right for every draw it touches: one view draws opaque or
  cutout meshes, and every tagged mesh that is ready has 32-bit indices and 32-bit float
  positions at a 4-byte-aligned offset, in slabs whose ids fit in 16 bits. Otherwise nothing is
  dispatched and nothing changes. It rescans when the instance count changes or a covered slab
  moves, every 15 frames while a tagged mesh isn't uploaded, and every 120 frames. It logs where
  the tagged meshes are when that changes, and a status line says why it's off. About every
  two seconds, one frame counts its triangles by outcome; the counts are read back and logged
  at `info` under the target `bevy_pbr::render::triangle_cull`.
- Six more bits in `TRIANGLE_CULL` take the pass apart, for narrowing down a fault:
  `TRIANGLE_CULL_DEBUG_MIRRORS_ONLY` makes the mirrors but dispatches nothing;
  `TRIANGLE_CULL_DEBUG_EMPTY` dispatches, but every workgroup returns at once (push-constant
  flag 4), so no draw changes; `TRIANGLE_CULL_DEBUG_NO_SWAP` leaves `DrawMesh` drawing from the
  index slabs; `TRIANGLE_CULL_DEBUG_NO_STATS` never counts or reads back;
  `TRIANGLE_CULL_DEBUG_CLEAR` clears the mirrors every frame; and
  `TRIANGLE_CULL_DEBUG_STATS_ONLY`, with mirrors only, still clears, copies and reads back the
  counts. A line logs the bits when they change. The game sets them from
  `SKATE_TRIANGLE_CULL_DEBUG`.
- The pass has two versions with the same tests and outcomes. The second, the default
  (`triangle_cull_v2.wgsl`), runs `list`, one thread per draw slot, which appends each tagged
  draw it finds to an items buffer (marked to be copied whole if it has several instances) and
  counts them in an indirect-dispatch argument; then `cull`, through
  `dispatch_workgroups_indirect`, tests or copies one item's triangles per 64-thread workgroup,
  with the item's transform in workgroup memory. Each dispatch's count starts at one workgroup,
  for an empty item that `cull` returns from at once, so no indirect dispatch is ever empty.
  `list` and `cull` bind different sets,
  as wgpu takes no buffer as both written storage and indirect arguments (or read-only storage)
  in one dispatch. The second version turns itself off when a phase has 65535 draw slots or
  more. The first (`triangle_cull.wgsl`, `TRIANGLE_CULL_V1`, the game's
  `SKATE_TRIANGLE_CULL_V1`) starts a 256-thread workgroup for every draw slot of every dispatch,
  most of which find nothing to do, and costs several times as much on the Switch.
- `finish` adds `STORAGE` to `MeshAllocator::extra_buffer_usages`, so every mesh slab can be
  bound as a storage buffer, whether or not the pass ever runs.
- `TRIANGLE_CULL_MARKS` replaces the second version's tests with marks the view's depth
  prepass leaves: the game's prepass writes 1 to a word for each triangle that gets a fragment
  into its depth test, at its tag's `TRIANGLE_CULL_MARK_BASE` bits (the low 29) plus its
  `primitive_index`. `cull` keeps a triangle whose word is set, clears the word for the next
  frame, and leaves out the rest; a word out of the buffer's range keeps its triangle, and an
  item with several instances is still copied whole. `TRIANGLE_CULL_MARKS_OPAQUE_ONLY` leaves
  the `AlphaMask3d` draws out of the pass, for a prepass that marks only `Opaque3d`.
  `TRIANGLE_CULL_MARKS_INVERT`, for debugging, keeps the unmarked triangles instead (push flag
  16), still clearing the marked words, so the main pass draws only what the marks would lose.
  The marks are a `ShaderStorageBuffer` the game names in the `TriangleCullMarks` resource
  (`ExtractResource`), bound to `cull` at binding 12, with a placeholder when the mode is off.
  The pass waits until the buffer is uploaded and until every pipeline of the view's depth
  prepass is ready (a draw whose pipeline is still compiling is skipped, and marks nothing),
  turns itself off under the first version or without the view's `DepthPrepass`, and its
  statistics line counts the marked and unmarked triangles. The prepass and the main pass draw
  the same depths, and the depth buffer only nears the camera, so every fragment the main pass
  draws passed the prepass's test, and marked.
- Under `TRIANGLE_CULL_MARKS` a third entry point, `keep_marked`, keeps the marked triangles in
  `cull`'s place, with the same bindings, outcomes and order. Each thread takes four
  neighbouring triangles of a 256-triangle round, sets their bits with one `atomicOr` on a word
  eight threads share (in `cull`, each survivor's bit takes an `atomicOr` on a word its whole
  warp shares, which the GPU serialises), and loads its four marks, then its survivors'
  indices, before its first store: NAK issues `cull`'s loads one after another, each after the
  store before it. `TRIANGLE_CULL_MARKS_IN_CULL` (the game's `SKATE_VIS_CULL_KERNEL=0`) keeps
  them with `cull`, as before, to compare; a log line names the kernel.
- `TRIANGLE_CULL_MARKS_COALESCED` (the game's `SKATE_VIS_CULL_KERNEL=2`) keeps them with a
  fourth entry point, `keep_marked_coalesced`, instead: the same bit sets and order, but
  neighbouring threads read neighbouring marks into workgroup memory, and then copy the
  survivors' index words to the mirror a word a thread, each to the place its triangle's rank
  among the survivors gives. `keep_marked`'s warps touch words 16 or 48 bytes apart, a sector
  each, and perf27 found its cost follows the triangles it tests. Three barriers a round
  rather than one.
- `TRIANGLE_CULL_CLUSTERS` (a second static; the game's `SKATE_CLUSTER_CULL`, whose value is
  the span) culls the depth prepass's leaf draws too, by clusters of 32 triangles, under
  `TRIANGLE_CULL_MARKS`. `triangle_cull_clusters.wgsl` runs `early` between
  `EarlyPrepassBuildIndirectParameters` and `EarlyPrepass`, and `late` between
  `LatePrepassBuildIndirectParameters` and `LatePrepass`, over the `Opaque3dPrepass` and
  `AlphaMask3dPrepass` draws of the phases the main pass covers, a dispatch per pair of slabs.
  `early` tests each cluster's box against the previous frame's view and the depth pyramid, as
  Bevy's early pass tests meshes, writes the survivors' indices in order into a prepass mirror
  of the index slab, at the leaf's own offsets, and lowers the draw's `index_count`; it records
  each cluster's outcome. `late` tests every cluster of a leaf first drawn in the late pass,
  and those `early` left out of a leaf it drew, against this frame's view and the pyramid the
  early prepass left; it draws the late survivors through the leaf's slot after the early ones,
  moving the slot's `first_index` and the leaf's tag in the phase's `Mesh` buffer, so their
  marks follow the early ones'. The boxes are a `ShaderStorageBuffer` the game names in the
  `TriangleCullClusters` resource (`ExtractResource`): two `vec4<f32>` a cluster, in the leaf's
  local space, from its tag's mark base over 32, which the game keeps a multiple of 32.
- A leaf's count word holds the triangles its prepass drew and the frame's stamp (1 to 255),
  and is flagged when they're its kept clusters'. The main pass's `list`, with push flag 32
  and the stamp in the flags' top 8 bits, keeps the marked triangles among those, read from
  the prepass mirror (`cull`'s binding 5), and draws nothing for a leaf whose word is stale
  (counted as out of range); its binding 13 holds the outcomes and count words, or a
  placeholder. A draw of several instances, or one that isn't drawn as its mesh's input says,
  and a leaf without boxes are copied whole. `DrawMesh` binds the prepass mirror for the
  prepass phases only once both nodes ran, and the main pass waits for them too. Each frame
  that counts adds the prepass's triangles and kept clusters to the statistics.
- `TRIANGLE_CULL_CLUSTERS_CUTOUTS` (bit 8 of the static; the game's `SKATE_CLUSTER_CUTOUTS`)
  covers the cutouts under `TRIANGLE_CULL_MARKS_OPAQUE_ONLY`, which leaves them unmarked: the
  `AlphaMask3dPrepass` draws join the cluster cull, and the main pass's `AlphaMask3d` draws
  read the prepass mirror as it is. Their `list` dispatch, with push flags 32 and 64, only sets
  each draw's `index_count` from the leaf's count word, or 0 if it's stale, and there's no
  `cull` dispatch. A cluster left out lies behind the early prepass's depth, so none of its
  fragments could pass the main pass's depth test. The cutouts' cluster dispatches set their
  flag 4, which puts their counts in words 16 to 23 of the statistics, logged on a line of
  their own.
- `TRIANGLE_CULL_CLUSTERS_KERNEL_SHIFT` (bits 16 to 23 of the static; the game's
  `SKATE_CLUSTER_KERNEL`) picks the kernel: 0 runs `early` and `late`, anything else
  `early_fast` and `late_fast`, which keep the same clusters with fewer waits on memory. A
  workgroup gets about two leaves among its eight slots, so its time goes on reads waiting for
  the reads before them. The second asks for the pyramid's size once rather than twice a box,
  reads a slot's draw in two waits rather than five, reads a box with its early outcome, reads
  the pyramid 24 texels a wait rather than 8, and copies four surviving clusters a wait rather
  than one. Each kernel's pipelines are queued on its first use.
- The cluster cull needs the second version's marks, not inverted, no debug bits, the view's
  depth prepass, occlusion culling and depth pyramid, no other view drawing a depth prepass,
  the previous view's uniforms and the boxes; and a device without
  `MULTI_DRAW_INDIRECT_COUNT`, whose build packs the draws. Otherwise a status line says why,
  and the prepass draws its leaves whole. It costs a mirror per index slab, as large as it
  (25.6 MiB for the game's world), and 8 bytes a cluster.
- Its workgroups are 32 threads, one warp, so NAK drops their barriers: on the GPU a warp's
  threads run together. Ryujinx on lavapipe runs those 32 as four subgroups of 8 that only
  meet at a barrier, which leaves the cull wrong there; run it with `NAK_DEBUG=wgbar`
  (bevy-horizon's Mesa patch 0022), which keeps them.
- The game tags its world leaves and sets the static from `SKATE_TRIANGLE_CULL`, or from
  `SKATE_VIS_CULL` for the marks (`3` for the inverted debug mode; Wihlidal, "Optimizing the
  Graphics Pipeline with Compute", GDC 2016, describes the technique).

## Scissored shadow cascades (Switch port)

One opt-in change, inert unless the game asks for it:

- `bevy_pbr/src/render/light.rs`: with the `ShadowScissors` resource in the
  render world, a shadow view's pass draws only inside the view's rectangle.
  `ShadowPassNode` calls `set_scissor_rect` before it renders the phase, for
  each view whose `retained_view_entity` has a rectangle in `rects` and whose
  viewport is `size` texels square. The rectangle is in texels from the top
  left of the map, and is clamped to the map. A view without one, or whose map
  is another size, draws all of its map, as Bevy does. A cascade's key is the
  one `prepare_lights` gives its view:
  `RetainedViewEntity::new(light, Some(camera), cascade index)`, with
  main-world entities.
- The scissor limits only what is drawn. Each pass still clears its whole
  layer, because a load-op clear covers the render area whatever the scissor.
  NVK's `nvk_CmdBeginRendering` clears through `nvk_CmdClearAttachments` over
  the render area, with `use_scissor0` false. So `SkipClearShadowLayers` still
  holds. Each cascade is its own render pass, so a rectangle never carries over
  into the next.
- A view whose rectangle is empty, no texel wide or high, doesn't render its
  phase: the scissor would keep out every fragment, so its draws would cost
  only their vertices. With no skater by the billboard, the game's cascade 3
  drew 229 leaves, 163,332 triangles, into an empty rectangle, for 1.40–1.42 ms
  of the Switch's GPU time a frame. The pass still begins and ends, so the
  layer is cleared as before, and the same texels result.
- What the app must guarantee: the rectangle holds every texel that anything
  drawn that frame reads from the view's map, through whichever filter it
  uses. Outside the rectangle the map holds the clear value rather than the
  casters' depth, so a read there would lose its shadow. With
  `experimental_pbr_pcss`, a soft-shadowed light's blocker search and penumbra
  reach further than the PCF filters do. The game doesn't compile that feature.
- The game fills the rectangles in the main world, from the boxes of the
  meshes that can receive the sun's shadow
  (`crates/skate-game/src/retail_shadow_bounds.rs`, `sun_receivers`). It copies
  the resource into the render world in `render_options.rs`
  (`extract_render_skips`). `SKATE_SUN_SCISSOR=0` turns it off.

## UI drawn into the window (Switch port)

`bevy_ui_render` is vendored from crates.io 0.18.1 with its original licenses.
One opt-in change, inert unless the game asks for it:

- `src/lib.rs` and `src/render_pass.rs`: a camera with the new `UiToOutput`
  component has its UI drawn into its output texture, the window, through
  `ViewTarget::out_texture_color_attachment`, rather than into its main
  texture. `extract_ui_camera_view` copies the marker to the camera's render
  entity and, while it's there, gives the UI view `hdr: false`, so the UI's
  pipelines are specialised for the window's format
  (`TextureFormat::bevy_default()`) rather than `Hdr`'s. Upstream, a camera's
  UI always goes into its main texture, which upscaling then copies into the
  window. The game adds the marker to the gameplay camera, with
  `CameraOutputMode::Skip`, while its tone pass writes the window itself
  (`crates/skate-game/src/retail_exposure.rs`, `choose_tone_output`;
  `SKATE_TONE_DIRECT=0` turns that off).

## Closed menus skip the UI walks (Switch port)

`bevy_ui` is vendored from crates.io 0.18.1 with its original licenses. In
0.18.1, three systems walk every UI node of every root on every frame, changed
or not: `ui_layout_system` (the children sync, taffy's layout and rounding, and
`update_uinode_geometry_recursive`), `ui_stack_system` and
`update_clipping_system`. The game keeps its closed menus spawned under roots
set to `Display::None`, so most of those nodes are hidden. On the Switch at
0.1.3-rc2 the walks cost about 2 ms of the main thread's 12.5 ms a frame
(`ui_layout_system` alone 11.4 % of its samples), with every menu closed. The
changes, each marked `[Skate 3 patch]`:

- `src/layout/mod.rs`, `ui_layout_system`: a root that is `Display::None`,
  unchanged (`Node` and its render-target info) and already zero-sized is
  parked: skipped entirely. Its subtree was zeroed the frame it was hidden, so
  nothing in it is drawn or picked. Nodes are still synced to taffy every frame
  as before; when the root is shown again, its subtree's children lists are
  synced in full (`update_children_recursively`'s `resync`), so children added
  or removed while it was parked are laid out.
- `src/stack.rs`, `ui_stack_system`: roots and `GlobalZIndex` nodes that are
  `Display::None` are left out of the stack, so they aren't walked, drawn or
  picked. A button in a closed menu keeps its last `Interaction` until the
  menu opens again (a mouse release still resets `Pressed` everywhere).
- `src/update.rs`, `update_clipping_system`: hidden roots aren't walked.

Limits: only roots are parked, not hidden nodes inside a shown tree; a node
moved from a shown tree into a parked one would keep its old size until the
root is shown. The game does neither. Tests:
`layout::tests::hidden_root_is_parked_then_synced_when_shown` and
`stack::tests::hidden_root_is_left_out_of_the_stack`.

## SDL3 static C runtime

`sdl3-sys` is vendored from crates.io 0.7.1+SDL-3.4.16 with its original license.
The only change is in `build.rs`: when building SDL from source for an MSVC target,
it defines `CMAKE_MSVC_RUNTIME_LIBRARY` to match the C runtime Rust links
(`MultiThreaded` with `+crt-static`, as release packages and CI use; otherwise
`MultiThreadedDLL`). SDL's CMake project enables policy CMP0091, which ignores
the `-MT` flag cmake-rs adds, so without this the static-CRT link fails with
unresolved `__imp_*` CRT symbols. Development builds (`scripts/Build.ps1`) are
unaffected. When upgrading `sdl3`/`sdl3-sys`, re-vendor and reapply this change,
or drop it if upstream cmake-rs/sdl3-sys handle the runtime themselves.

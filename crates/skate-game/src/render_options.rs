//! Rendering trade-offs chosen at launch from environment variables. On the Switch they are
//! `env` lines in `options.txt` (horizon.rs). Unset, each one renders as the game did before it
//! existed, except the depth prepass and `SKATE_PROCEDURAL_HDR`, which the Switch turns on unless
//! told not to, and the world variants `SKATE_WORLD_TRIM`,
//! `SKATE_WORLD_PACKED` and `SKATE_CUTOUT_FLAT`, the shadow skips `SKATE_SHADOW_BOUNDS`,
//! `SKATE_SUN_RECEIVERS`, `SKATE_SUN_SCISSOR` and `SKATE_CASCADE_SKIP`, `SKATE_PREPASS_DEPTH_COPY`,
//! `SKATE_HUD_RETAIN`, `SKATE_PRESENT_DIRECT` and `SKATE_TONE_DIRECT`, which draw the same pixels.
//!
//! - `SKATE_DEPTH_PREPASS=1`: draw the depth of every opaque mesh first (Bevy's `DepthPrepass` on
//!   the gameplay camera), so the main pass shades only the front surface of each pixel instead
//!   of every surface the draw order lets through. It costs a second, depth-only pass over the
//!   geometry. The world's two vertex stages compute the clip position the same way for this,
//!   and declare it `@invariant` (retail_world.wgsl). The world's alpha-tested classes run their
//!   cutoff in the prepass only, and the main pass draws them where the depths are equal
//!   (retail_render.rs `WorldMaterial::specialize`). On by default on the Switch, where it halved
//!   the world's shaded fragments and took about 24 ms off a frame (perf4, findings 21);
//!   `SKATE_DEPTH_PREPASS=0` turns it off.
//! - `SKATE_SUN_CASCADES=<1-4>`: the scene sun's shadow cascades, 4 unless set. Each cascade
//!   draws the shadow casters in its range again.
//! - `SKATE_SUN_SHADOW_DISTANCE=<metres>`: how far from the camera the scene sun's shadows reach,
//!   100 unless set.
//! - `SKATE_SHADOW_FILTER=hardware|gaussian|temporal`: how shadow edges are filtered (Bevy's
//!   `ShadowFilteringMethod`). Bevy's default, `gaussian`, takes 9 shadow-map samples per light;
//!   `hardware` takes one 2x2 comparison sample and gives harder edges.
//! - `SKATE_DRAW_DISTANCE=<metres>`: hide the world leaves whose nearest point is farther than
//!   this from the camera, unlimited unless set. Far geometry vanishes at the cut instead of
//!   fading into the fog, so this is for measuring how much of a frame the world's triangles
//!   cost, and for whoever prefers the frames to the horizon.
//! - `SKATE_WORLD_SHADOWS=0`: the world's leaves stop casting into the scene sun's cascades. The
//!   world never receives those: its shadows come from its lightmaps and from the light that
//!   casts only the skater and board (retail_world.wgsl). So this changes only the skater and
//!   the props, which stay sunlit under bridges and roofs, and leaves the cascades only them to
//!   draw.
//! - `SKATE_SHADOW_BOUNDS=0`: the world looks up the skater and board's shadow at every pixel
//!   within its 24 m, as it did before. Unset, it looks only where they can cast it, inside a
//!   sphere round them all extended away from the light (retail_shadow_bounds.rs
//!   `caster_bounds`); everywhere else the lookup could only have said "lit".
//! - `SKATE_SHADOW_BOUNDS_TIGHT=1`: that sphere is fitted round what each of the skater's joints
//!   carries, its own ball and box (retail_shadow_bounds.rs `tight_sphere`), rather than round
//!   the box of all their balls, so it is smaller and the world looks up the shadow at fewer
//!   pixels. Every vertex stays inside it, so the picture should not change.
//! - `SKATE_SHADOW_BOUNDS_CHECK=1`: for checking only. The casters are skinned on the CPU each
//!   frame, as the GPU skins them, and the log says how far inside the sphere their farthest
//!   vertex lies (retail_shadow_bounds.rs `farthest_vertex`). It costs milliseconds a frame.
//! - `SKATE_SHADOW_LOOKUPS=0`: for measuring only. The world looks up the skater and board's
//!   shadow nowhere (retail_shadow_bounds.rs `caster_bounds`), so it vanishes from the ground,
//!   and against the default the frame gives what the lookups cost. Their light still draws them
//!   into its map.
//! - `SKATE_HIDE_SKATER=1`: for measuring only. The skater and everything that hangs from it are
//!   hidden (`hide_skater`), so they leave every pass, and their shadows and the world's lookups
//!   of them go too. Against `SKATE_SHADOW_LOOKUPS=0` the frame gives what drawing the skater
//!   costs. Its animation stops while it is hidden; the physics and the camera carry on.
//! - `SKATE_SUN_RECEIVERS=0`: the scene sun draws all of its cascades, as it did before. Unset, it
//!   draws only those that something on screen looks up: each cascade covers a range of depths
//!   from the camera, and the world takes its shadows from the other light, so with only the
//!   skater and board looking the sun up, the cascades past them draw their casters for nothing
//!   (retail_shadow_bounds.rs `sun_receivers`).
//! - `SKATE_SUN_SCISSOR=0`: each cascade the scene sun draws covers all of its layer, as before.
//!   Unset, it draws only within the texels that what looks it up can read: round the skater and
//!   board, their shadow biases and the filter's reach, a small part of a layer that spans what
//!   the camera sees. Elsewhere nothing reads the layer, which the pass still clears whole
//!   (vendor/README.md). It needs `SKATE_SUN_RECEIVERS`.
//! - `SKATE_PREPASS_DEPTH_COPY=1`: after the prepass, copy the depth it drew into the texture
//!   shaders sample it from (Bevy's `depth_prepass_texture`), as Bevy does. None of the game's
//!   shaders samples it, so unset the copy is left out (vendor/README.md): a 3.7 MB read and
//!   write at 1280x720, twice with occlusion culling, outside every timed pass.
//! - `SKATE_CASCADE_SKIP=0`: clear every shadow cascade every frame, as Bevy does. Unset, a
//!   directional cascade with nothing to draw skips its pass when its layer of the shadow map is
//!   already clear (vendor/README.md). In perf6 the sun's cascades 1 to 3, which
//!   `SKATE_SUN_RECEIVERS` empties, each still cleared a 4 MiB layer every frame.
//! - `SKATE_HUD_RETAIN=0`: draw the scoring HUD every frame, as before. The HUD draws into its
//!   own full-screen image (scoring_hud.rs): its camera clears a texture and a depth texture,
//!   draws, and copies the texture into the image, and a full-screen node blends the image over
//!   the frame, about 26 MB of reads and writes a frame at 1280x720. Unset, while the HUD draws
//!   nothing (no trick on screen), its node is hidden and, once its image has been cleared, its
//!   camera stops, since blending a clear image over the frame changes nothing. The
//!   session-marker HUD's camera, which blends the whole frame into the window again even with
//!   the marker hidden (session_marker/hud.rs), likewise stops while it draws nothing; while it
//!   runs, `SKATE_PRESENT_DIRECT` can't.
//! - `SKATE_HUD_RECT=1`: while the scoring HUD shows, its camera draws and copies, and its node
//!   blends, only round what it shows: what its shapes cover, 4 pixels more on each side, on a
//!   16-pixel grid (scoring_hud.rs `hud_rect`). The copy no longer clears the HUD's image first,
//!   and the node loads the image's pixel under each of its own. Unset, both cover the screen,
//!   as before: on the Switch about 0.8 ms of timed work a frame while the HUD shows (findings
//!   43 §1.5).
//! - `SKATE_PRESENT_DIRECT=0`: always present the world through the scene image, as before. At
//!   100% render scale the image is the window's size, and the camera that presents it only
//!   clears a texture and a depth texture, copies the image into the texture under the UI and
//!   copies that into the window, about 18 MB of reads and writes a frame at 1280x720. Unset,
//!   at 100% the gameplay camera draws into the window itself and the UI over the world, and
//!   that camera stops (graphics_menu.rs `choose_presentation`). The UI then blends in the
//!   world's 16-bit colour rather than 8-bit sRGB, which can differ in the last bit at its edges.
//! - `SKATE_TONE_DIRECT=0`: while the gameplay camera draws into the window, tone its 16-bit
//!   colour into another 16-bit texture, draw the UI over that, and have Bevy's upscaling copy it
//!   into the window, as before. Unset, the tone pass writes the window itself and the UI is drawn
//!   over it there. The copy goes, with its read of the 16-bit colour, and so do the tone pass's
//!   clear of its 16-bit target and half of its write: about 22 MB of reads and writes a frame
//!   at 1280x720 (retail_exposure.rs `choose_tone_output`, the vendored bevy_ui_render
//!   `UiToOutput`). The tone's result then reaches 8-bit sRGB without a 16-bit step between, and
//!   the UI blends in 8-bit sRGB again, as before `SKATE_PRESENT_DIRECT`; either can differ in the
//!   last bit.
//! - `SKATE_PRESENT_TO_WINDOW=0`: while the camera that presents the scene image runs (below 100%
//!   render scale, or with `SKATE_PRESENT_DIRECT=0`), have it clear a texture, draw the image and
//!   the UI into it, and have Bevy's upscaling copy that into the window, as before. Unset, it
//!   draws them into the window itself (`UiToOutput`, its output step skipped), clears nothing,
//!   and the image's node writes its pixels without blending: the same pixels, without the copy,
//!   the clear or the blend's reads (graphics_menu.rs `choose_presentation`). On the Switch the
//!   UI pass took 1.6-1.9 ms and the copy 0.3-0.5 ms a frame at 67-85% (findings 62). Not while
//!   another camera draws into the window over it.
//! - `SKATE_UPSCALE=easu|bilinear`: below 100% render scale, how the camera that presents the
//!   scene image stretches it to the window (graphics_menu.rs `choose_upscaler`). Unset, FSR 1's
//!   sharpening (RCAS, fsr_rcas.wgsl) runs on the image's bilinear stretch: five filtered reads
//!   a pixel. `easu` is FSR 1's edge-adaptive upscale (fsr_easu.wgsl), which cost 6-7.5 ms a
//!   frame on the Switch at 67-75% (findings 62), more than the lower scale saved; `bilinear` is
//!   the image's bilinear filter alone.
//! - `SKATE_TONE_GAMMA=0`: with `SKATE_TONE_DIRECT`, the tone pass converts the retail curve's
//!   result to linear, three powers a pixel, and the window's sRGB view converts it back
//!   (retail_tone.wgsl), as before. Unset, it writes that result into the window as it is,
//!   through a view of the window's 8-bit texture that doesn't encode sRGB, which saved 0.34 ms
//!   a frame on the Switch (findings 36). The same 8-bit values come out but for rounding, so
//!   either can differ in the last bit. It needs a window whose texture is `Rgba8Unorm` viewed
//!   as sRGB, as the Switch's is; elsewhere the tone pass converts as it does with 0
//!   (retail_exposure.rs `ExposureNode`).
//! - `SKATE_PROCEDURAL_HDR=0`: a scene without a retail map (a custom map, or the test world) is
//!   drawn through the 8-bit camera Bevy gives it, which tonemaps in each material's shader, as
//!   before. Unset, the Switch draws it through an HDR camera, as it draws a retail map, and
//!   Bevy tonemaps it in a pass of its own (camera.rs `set_world_environment`). With the skater
//!   hidden, Kenney Park and a one-draw custom map ran on the Switch that way. Elsewhere it's off
//!   unless set to 1.
//! - `SKATE_PROCEDURAL_TONEMAPPING=none|aces|tony`: Bevy's tonemapping for such a scene: none,
//!   ACES fitted, or TonyMcMapface, which reads a 3D lookup table and is Bevy's default and the
//!   default here.
//! - `SKATE_PROCEDURAL_CHARACTER=1`: on such a scene the skater is shaded as on a retail map, with
//!   the character shader (retail_character.rs `load`), lit by `character-lighting.json`'s
//!   default probe and a sun overhead. Unset, it keeps the `StandardMaterial`s it was made with,
//!   lit by Bevy. The Switch turned it on while Bevy's lit `StandardMaterial` lost the GPU within
//!   a few frames wherever it drew (findings 55 and 59); bevy-horizon's Mesa patch 0032
//!   (`NAK_DEBUG=hzero`) fixed that.
//! - `SKATE_OCCLUSION=1`: Bevy's GPU occlusion culling on the gameplay camera (`OcclusionCulling`,
//!   which Bevy ignores without the depth prepass). The prepass first draws the meshes that were
//!   visible the frame before and reduces their depth to a pyramid of smaller and smaller images,
//!   each texel the farthest depth beneath it. Every mesh whose bounding box lies wholly behind
//!   that is left out of the prepass and the main pass; the rest are tested again against the
//!   frame's own depth, and those it can't rule out are drawn (two-phase occlusion culling,
//!   bevy_render `experimental::occlusion_culling`). A world leaf hidden behind a building then
//!   costs neither pass its vertices or its triangles. It costs two pyramid builds and a second
//!   mesh preprocessing pass a frame. The vendored pyramid is conservative at any size
//!   (vendor/README.md), so the pixels shouldn't change, but Bevy calls the feature experimental.
//! - `SKATE_OCCLUSION_SPAN=<1-32>`: under `SKATE_OCCLUSION`, test each bounding box against the
//!   pyramid more finely than Bevy does. Bevy reads 2x2 texels at the level where the box's
//!   rectangle on screen spans about one, so the patch it reads can be up to four times the
//!   rectangle each way, and one texel of sky in that patch keeps the box. This reads every texel
//!   the rectangle touches at the finest level where it spans at most this many texels each way,
//!   up to (span + 1)^2 reads a box (vendor/README.md). Each texel of a level is the farthest of
//!   those beneath it, so this rules out every box Bevy's test does and more, and still keeps
//!   any box with a pixel in front of the pyramid. Unset or 0, Bevy's test.
//! - `SKATE_OCCLUSION_BATCH=0`: with `SKATE_OCCLUSION_SPAN`, the finer test compares the box with
//!   each texel in turn, as before. Unset, it reads the same texels eight at a time along each row
//!   and compares the box with the farthest of each eight, as Bevy's test does with its four, so
//!   the GPU can wait for eight reads at once rather than for each in turn (vendor/README.md).
//!   The answer is the same unless the pyramid held a NaN, which a pyramid of rasterised depths
//!   cannot. NAK issues the eight together only with its `plat` flag (bevy-horizon's mesa patch
//!   0015, findings 36), on by default in Switch builds since patch 0019, and otherwise two at a
//!   time. On the Switch it saved 0.43 ms a frame with `plat` and 0.31 ms without (findings 38).
//! - `SKATE_PYRAMID_ONCE=1`: under `SKATE_OCCLUSION`, build the depth pyramid once a frame rather
//!   than twice. Bevy builds it after the early prepass, for the frame's own second test, and again
//!   at the end of the main pass, from the finished depth, for the next frame's first. This skips
//!   the second build, so the next frame tests against the first (vendor/README.md). That lacks
//!   only the meshes the late prepass drew, so the next frame's first test can keep more meshes,
//!   never fewer, and the pixels don't change.
//! - `SKATE_PYRAMID_TILE=1`: under `SKATE_OCCLUSION`, each workgroup of the pyramid build reads
//!   the depth beneath it into shared memory once and makes the first two levels from there,
//!   rather than with four gathers a texel that the GPU waits on one by one (vendor/README.md).
//!   Each texel is the minimum over the same depth texels as before, so the pyramid and the
//!   pixels don't change. A depth buffer whose blocks don't fit, such as 1600x900, keeps Bevy's
//!   build and warns once.
//! - `SKATE_PYRAMID_BLOCK=1`: under `SKATE_OCCLUSION`, each thread of the pyramid build gathers
//!   the depth beneath its 2x2 block of first-level texels side by side, six gathers at
//!   1280x720, rather than sixteen the GPU waits on one by one (vendor/README.md). Each texel is
//!   the minimum over the same depth texels as before, so the pyramid and the pixels don't
//!   change. `SKATE_PYRAMID_TILE` takes precedence where both are set.
//! - `SKATE_LATE_PREPROCESS_DIRECT=0`: under `SKATE_OCCLUSION`, the second mesh preprocessing
//!   pass is dispatched indirectly, as in Bevy, with the number of meshes the first pass left out
//!   for it to test again. Unset, it is dispatched directly, with a thread for each mesh the first
//!   pass tested, and the shader still stops at that number, so the same meshes are tested and
//!   nothing drawn changes. On the Switch, NVK makes each indirect dispatch wait for the work
//!   before it, and the pass took 0.27 ms less dispatched directly (findings 40,
//!   vendor/README.md).
//! - `SKATE_INDIRECT_PHASE_FILTER=0`: the three passes that build the draws' indirect
//!   parameters each build every phase, as in Bevy. Unset, each skips the phases nothing draws
//!   before the next build rebuilds them: the prepass builds skip the main pass's phases, and
//!   the main pass's build skips the prepass, deferred and shadow phases once the first build has
//!   built them. Nothing reads what they skip, so nothing drawn changes. On the Switch the three
//!   took 0.17 ms less (findings 40, vendor/README.md).
//! - `SKATE_TRIANGLE_CULL=<1-32>`: before the main pass, a compute pass tests each triangle of
//!   every opaque and cutout world leaf it draws, and leaves out those that can't give it a
//!   pixel: wholly outside the view, facing away (one-sided materials), covering no pixel
//!   centre, or, under `SKATE_OCCLUSION`, behind every texel of the depth pyramid its rectangle
//!   touches, at the finest level where it spans at most this many texels each way, as
//!   `SKATE_OCCLUSION_SPAN` reads them for boxes. The survivors go, in their order, to a copy of
//!   the index buffer that the main pass draws those leaves from (bevy_pbr `triangle_cull`,
//!   vendor/README.md). Every test is conservative, so the pixels shouldn't change. It costs a
//!   read of every drawn leaf's triangles and corners a frame; the prepass and the shadows still
//!   draw them all. `nodepth` leaves out the pyramid test. Unset or 0, off; the fetch variant
//!   (`SKATE_WORLD_FETCH`) turns it off.
//! - `SKATE_TRIANGLE_CULL_DEBUG=<1-63>`: for narrowing down a fault in `SKATE_TRIANGLE_CULL`,
//!   which must be on. Bits: 1 makes the copies of the index buffers but dispatches nothing; 2
//!   dispatches, but every workgroup returns at once, so the draws keep their counts; 4 leaves
//!   the main pass drawing from the index buffers themselves; 8 never counts the triangles or
//!   reads the counts back; 16 clears the copies every frame; 32, with 1, still clears, copies
//!   and reads back the counts. Unset or 0, the full pass.
//! - `SKATE_TRIANGLE_CULL_V1=1`: `SKATE_TRIANGLE_CULL` runs its first version, which starts a
//!   workgroup for every draw, to compare with the second, the default, which lists the world
//!   leaves' draws first and starts workgroups only for them. Same tests, same pixels.
//! - `SKATE_VIS_CULL=1|2|3`: the triangle cull keeps, of each world leaf it takes, only the
//!   triangles the view's depth prepass marked, and none of its own tests run (bevy_pbr
//!   `TRIANGLE_CULL_MARKS`, vendor/README.md). The prepass marks each triangle that gets a
//!   fragment into its depth test, in a word a world triangle that the cull clears once it has
//!   read it (retail_depth.wgsl `WORLD_MARK`). The opaque classes' depth pipelines get a fragment
//!   stage for it, which runs after the depth test, so only fragments that pass it mark. The
//!   depth only nears the camera as the prepass goes on, and the main pass tests the same depths
//!   against where it ends, so every fragment the main pass draws marked its triangle, and the
//!   pixels shouldn't change. The cutout classes mark before their alpha test, and so keep every
//!   triangle the prepass rasterises. `1` marks in every class; `2` only in the opaque ones, and
//!   the cutout leaves are drawn whole. `3`, for debugging, marks as `1` does but keeps only the
//!   unmarked triangles (`TRIANGLE_CULL_MARKS_INVERT`), so the leaves' pixels the main pass
//!   draws are just those that `1` would lose. It needs the depth prepass and takes the place of
//!   `SKATE_TRIANGLE_CULL`, set or not. A map builds the marks when it is set at launch or in a
//!   `SKATE_PERF_MATRIX` step, and then only if the session builds no fetch copy
//!   (`SKATE_WORLD_FETCH`); they need fragment stages with `primitive_index`, early depth tests
//!   and storage writes (retail_render.rs `vis_cull_supported`). Unset or 0, off.
//! - `SKATE_VIS_CULL_KERNEL=0|1|2|3|4`: under `SKATE_VIS_CULL`, the kernel that keeps the
//!   marked triangles. Unset or `3`, `keep_marked_ballot` (`TRIANGLE_CULL_MARKS_BALLOT`): perf28
//!   found that the sectors weren't `keep_marked`'s time (findings 48), so a 32-thread workgroup
//!   places each kept triangle with a subgroup ballot, in fewer instructions and with no
//!   workgroup memory, atomic or barrier. perf29 found it kept the same triangles as
//!   `keep_marked`, 0.25 ms sooner by the billboard (findings 49). It needs subgroups of at
//!   least 32 invocations; without them `keep_marked` runs, and the log says so. Same
//!   triangles, same order, same pixels. `0`, the kernel that runs the cull's tests (bevy_pbr
//!   `TRIANGLE_CULL_MARKS_IN_CULL`), as before perf27. `1`, `keep_marked`, the default from
//!   perf27 to perf29, which hands each thread four neighbouring triangles and loads before it
//!   stores (findings 46). `2`, `keep_marked_coalesced` (`TRIANGLE_CULL_MARKS_COALESCED`): the
//!   same work, but neighbouring threads read and write neighbouring words, as perf27 found
//!   `keep_marked`'s cost follows the triangles it tests, about 5 ns each (findings 47). `4`,
//!   the ballot kernel in an 8-thread workgroup (`TRIANGLE_CULL_MARKS_BALLOT_8`), only to test it
//!   in Ryujinx on llvmpipe, whose subgroups have 8 invocations, which split `3`'s workgroup in
//!   four (findings 48).
//! - `SKATE_VIS_CULL_AFTER_TEST=1`: under `SKATE_VIS_CULL=1` or `3`, the cutout classes mark
//!   after their alpha test rather than before it (retail_depth.wgsl `WORLD_MARK_AFTER_TEST`),
//!   so only the fragments the test keeps store a mark, and the cull leaves out a triangle it
//!   rejected everywhere. Such a triangle left no depth of its own, and the main pass draws a
//!   cutout only where its depth equals the prepass's, so the pixels change only where it lies
//!   at exactly another surface's depth, where the main pass drew texels the test rejected.
//!   The water families still mark before it: the main pass tests their texel scrolled. Unset
//!   or 0, every cutout marks before it.
//! - `SKATE_PREPASS_ORDER=1`: the view's opaque depth prepass draws the world's leaves nearest
//!   first, by the distance from the camera to each leaf's box (prepass_order.rs). Bevy draws
//!   them in the order its bins were filled. Near surfaces then fill the depth buffer before
//!   the far ones behind them come, so fewer far fragments pass the depth test, and under
//!   `SKATE_VIS_CULL` fewer far triangles are marked and kept for the main pass. Each
//!   multi-draw (a batch set: a pipeline, material and vertex slab) is ordered on its own, as
//!   Bevy draws them one after another. The depth the prepass ends with doesn't depend on the
//!   order, and every triangle with a fragment at that depth still marks, as the test passes
//!   equal depths, so the same pixels. The cutout classes and the main pass keep Bevy's order.
//!   Unset or 0, Bevy's order.
//! - `SKATE_CLUSTER_CULL=<1-32>`: the depth prepass draws a world leaf only by the runs of 32 of
//!   its triangles whose boxes pass Bevy's occlusion test, which it otherwise passes or fails
//!   for the whole leaf, testing each run's box at this span (as `SKATE_OCCLUSION_SPAN`; bevy_pbr
//!   `TRIANGLE_CULL_CLUSTERS`). A leaf passes if any of it may show, so by the billboard perf29's
//!   prepass drew about 586,000 triangles, about thirteen for each the main pass kept, and a
//!   model of the test put runs in the order they're drawn at about 305,000 (findings 49). A pass
//!   before each prepass phase writes the triangles of the runs it keeps to a copy of the index
//!   slab, which the prepass draws from. The late phase tests again, against the frame's own
//!   pyramid, the runs the early one left out, as Bevy does leaves. A run left out lies behind
//!   the depth the prepass has drawn by then, so its triangles would have drawn nothing that
//!   lasts: the same depth and the same pixels. The main pass's cull then takes only the
//!   triangles the prepass drew. It needs `SKATE_OCCLUSION`, the depth prepass, `SKATE_VIS_CULL`
//!   or `SKATE_TRIANGLE_CULL`, and where each leaf's marks start, so a map builds the boxes only
//!   if it builds the marks and this is set at launch or in a `SKATE_PERF_MATRIX` step
//!   (`ccull`). Unset or 0, off.
//! - `SKATE_CLUSTER_CUTOUTS=1`: with `SKATE_CLUSTER_CULL` under `SKATE_VIS_CULL=2`, the cutouts'
//!   leaves too (bevy_pbr `TRIANGLE_CULL_CLUSTERS_CUTOUTS`). `SKATE_VIS_CULL=2` leaves them
//!   unmarked, so their prepass draws each leaf that passes whole, and the main pass draws it
//!   whole again. With this, their prepass draws only the runs whose boxes pass, as the opaque
//!   prepass does, and the main pass draws those runs from the prepass's copy rather than the
//!   whole leaf. A run left out lies behind the depth the early prepass drew, and the main pass
//!   passes only fragments at or nearer than the depth the prepass ends with, so it would have
//!   drawn no pixel there either, whatever the cutout's texture. `SKATE_VIS_CULL=1` marks the
//!   cutouts, so `SKATE_CLUSTER_CULL` covers them already. Set at launch or in a
//!   `SKATE_PERF_MATRIX` step (`ccut`). Unset or 0, off.
//! - `SKATE_CLUSTER_KERNEL=1`: with `SKATE_CLUSTER_CULL`, the runs' boxes are tested by a second
//!   kernel (bevy_pbr `TRIANGLE_CULL_CLUSTERS_KERNEL_SHIFT`), which keeps the same runs but waits
//!   on memory less often. By the billboard perf32's test took 4.6-4.8 ms, mostly reads waiting
//!   for the reads before them (findings 51). Set at launch or in a `SKATE_PERF_MATRIX` step
//!   (`ckern`). Unset or 0, the first.
//! - `SKATE_WORLD_HOIST=1`: the world fragment loads all of its material's parameters and
//!   channel words at the top, as it did before it loaded each where it is used
//!   (retail_world.wgsl). That held them all in registers through the whole shader, leaving the
//!   GPU room for fewer fragments at once. A shader variant, read once at launch: unlike the
//!   others, `SKATE_PERF_MATRIX` cannot switch it.
//! - `SKATE_WORLD_PAGE_ARRAY=1`: the world's eight page textures are bound as one array of
//!   textures rather than eight bindings, and each lookup indexes it with the page its material
//!   names (retail_material_bindings.wgsl `sample_page`). Bound one by one, every lookup is a
//!   switch over the eight with a texture instruction in each case, about twelve of them in the
//!   world fragment. Indexed, each is one instruction. The array is a layout of the world
//!   material's bind group, which every world pipeline shares, so this is read once at launch and
//!   `SKATE_PERF_MATRIX` cannot switch it either. It needs the adapter to index texture arrays
//!   by a value that differs between fragments; NVK offers that on every NVIDIA GPU, and before
//!   Turing compiles each such lookup to a loop over the distinct indices in a warp.
//! - `SKATE_WORLD_SHADOW_SPLIT=1`: the world's opaque and cutout leaves are built in two
//!   groups, the materials the skater's shadow can fall on (families 8 and below, and 13:
//!   retail_world.wgsl) and the rest, so that no draw holds both (retail_render.rs
//!   `MaterialEntry::shadow_group`). The blended leaves stay as they are: Bevy draws them back to
//!   front, a draw at a time, so splitting them would change which of their surfaces covers
//!   which. Alone it changes only the draws. It is there for `SKATE_WORLD_FEATURES` below,
//!   which can then leave the shadow lookup out of the second group's pipelines: in University,
//!   the cutout foliage's (families 9 and 10). Where a slab's class holds both groups it adds a
//!   draw, and the leaves are built when a map loads, so this is read once at launch and
//!   `SKATE_PERF_MATRIX` cannot switch it.
//!
//! The world shader variants below (`WorldVariant`) exist to be measured against each other.
//! The first three give the same pixels, or near enough; the probes don't. Each one compiles the
//! world's pipelines again, which on the Switch takes seconds.
//!
//! - `SKATE_WORLD_TRIM=0`: the world's depth-only pipelines (the prepass and the shadow
//!   cascades) fetch all seven vertex attributes and write the UV and material index, as they
//!   did before. Unset, they fetch and write only the position, plus the UV and material index
//!   for the classes that test alpha. NVK fetches every attribute a pipeline declares and
//!   compiles each stage alone, so a vertex stage writes its outputs whether or not a fragment
//!   stage reads them (retail_depth.wgsl).
//! - `SKATE_WORLD_PACKED=0`: the main pass's vertex stage writes the seventeen components its
//!   fragment stage reads in seven outputs, as it did before. Unset, it packs them four to an
//!   output, into five (retail_world.wgsl): the same values in fewer attribute slots.
//! - `SKATE_WORLD_LOD=1`: the world's page textures are sampled at a level of detail the shader
//!   works out from its gradients (`textureSampleLevel`) rather than with the gradients
//!   (`textureSampleGrad`), which hand the texture unit four more values for every lookup
//!   (retail_material_bindings.wgsl `sample_page`). The level is the one the gradients give
//!   without anisotropic filtering, which the pages' sampler doesn't use.
//! - `SKATE_WORLD_PROBE=1|2|3|4|5|6`: for measuring only. Under `1` and `2` the world is drawn
//!   unshaded. `1` reads every interpolated value, folds them into a false colour and samples
//!   nothing; `2` interpolates nothing and draws grey. Against each other and the full shader they
//!   split a frame's world cost between shading, interpolation and the geometry itself. Under
//!   `SKATE_WORLD_FETCH`, `1` fetches every value as that variant does and folds them the same
//!   way, and `2` reads only the triangle's number. `3` shades in full but reads every page at its
//!   smallest level (retail_material_bindings.wgsl `sample_page`), so the texels stay in cache:
//!   against the full shader it gives what fetching texels costs the main pass. `4` shades in
//!   full but reads no page or cube at all: each lookup returns a value made from its
//!   coordinates, so against the full shader it gives what the lookups cost altogether (their
//!   gradients, switches, texture instructions and texels), and against `1` what the shading
//!   arithmetic does. `5` shades in full but reads every lookup from the first page, whatever
//!   page its material names, so each is one texture instruction rather than a switch over the
//!   eight (retail_material_bindings.wgsl `page_select`): against the full shader it gives what
//!   the switches cost, which `SKATE_WORLD_PAGE_ARRAY` would save, though the texels it reads
//!   differ. `6`, under `SKATE_WORLD_SLOT_WATERFALL`, shades each fragment once with the first
//!   material its warp holds, so a warp holding several materials shades once rather than once
//!   for each: against the full shader it gives what the waterfall's repeats cost, though a
//!   fragment whose own material differs is drawn wrong. The prepass's alpha test is unchanged
//!   under all of them.
//! - `SKATE_PREPASS_PROBE=1|2|3|4|5|6`: for measuring only, the same kind of split for the view's
//!   depth prepass, where the cutout classes' alpha test runs; the sun's shadow cascades keep
//!   theirs (Bevy keys a directional light's shadow pipelines `UNCLIPPED_DEPTH_ORTHO`). `1` reads
//!   the diffuse page at its smallest level, so its texels stay in cache; `2` reads no page and
//!   tests an alpha of 1, so the test and its `discard` stay but discard nothing; `3` reads every
//!   lookup from the first page, so the switch over the eight goes; `4` drops the test and the
//!   cutout classes' fragment stage, so they draw as the opaque ones do; `5` drops the test but
//!   keeps an empty fragment stage, which can't discard, so the GPU tests depth before running it,
//!   and the vertex stage still writes the UV and material index; `6` keeps a test that reads only
//!   the UV and never discards, so the stage can discard but waits on no storage read or page.
//!   Against the full prepass they give what its texels, its lookup and its page switch cost, `2`
//!   against `4` what a fragment stage that can discard costs at all, `2` against `5` how much of
//!   that is the `discard`, `6` against `5` what a stage that can discard costs with nothing to
//!   wait for, and `2` against `6` what its read of the cutoff costs. `5`'s empty stage goes
//!   uncounted, as no stage would, so it can't count the cutout fragments a depth test before the
//!   shader leaves (findings 30). Each changes the pixels the prepass keeps, and so the picture
//!   and the main pass after it: read only the prepass's time.
//! - `SKATE_WORLD_FETCH=1`: the main pass's vertex stage writes the clip position and one flat
//!   number, the leaf's first triangle (Bevy's `MeshTag`), instead of the seventeen interpolated
//!   components. The fragment stage finds its triangle from that and `primitive_index`, loads the
//!   triangle's three corners from a copy of the world's vertices in storage buffers
//!   (skate_world.rs `FetchGeometry`), and interpolates them itself with the perspective-correct
//!   weights the rasterizer would have used (retail_world.wgsl `barycentrics`). The shading is
//!   unchanged, and its derivatives still come from the 2x2 quad, which belongs to one triangle.
//!   On the Switch, writing and interpolating those components took about 16 ms of the main
//!   pass (perf6's `SKATE_WORLD_PROBE` runs, findings 22). It costs a second copy of the world's
//!   vertices: 64 bytes a vertex and 16 a triangle, about 160 MiB for University. The copy is
//!   built when the variant is on at launch or `SKATE_PERF_MATRIX` turns it on in a step.
//! - `SKATE_WORLD_PAGE_UNIFORM=1`: with `SKATE_WORLD_PAGE_ARRAY`, each page lookup first asks
//!   whether every fragment in its subgroup (a warp of 32 on NVIDIA) names the same page, and if
//!   so reads that page with an index the compiler knows to be the same across the warp;
//!   otherwise it indexes the array as before (retail_material_bindings.wgsl `sample_page`).
//!   Before Turing, NVK compiles an index that may differ across a warp to a loop over the
//!   distinct pages, which waits for each texture before it moves on, where the same index for
//!   the whole warp needs no loop. The same pixels. It needs `Features::SUBGROUP`; without it,
//!   or without the page array, it changes nothing, and the launch log says which
//!   (retail_render.rs `log_page_uniform_support`).
//! - `SKATE_WORLD_FEATURES=1`: each world material's main pass compiles only the parts of the
//!   fragment that some material drawn with it reaches: the skater's shadow, the normal map and
//!   the tangent frame it needs, the detail, macro, decal and specular maps, and the reflective
//!   families' constant detail texel and environment cube (retail_render.rs `world_features`).
//!   The fragment still tests each material's family and channels as it did, so the same pixels;
//!   what goes is code none of its fragments run, and the registers that code held. The parts
//!   are gathered across the map for each class, so a class's slabs still share their pipelines,
//!   and a class whose materials reach every part, as University's opaque ones do, keeps the
//!   shader it had. The launch log lists them (`[Skate] world features`). In University the
//!   cutout class reaches only the shadow, which NAK compiles to 1211 instructions in 48
//!   registers rather than 2295 in 56, and under `SKATE_WORLD_SHADOW_SPLIT` its foliage reaches
//!   nothing, 387 instructions in 24 registers (findings 28).
//! - `SKATE_WORLD_SLOT_WATERFALL=1`: the main pass shades a subgroup's fragments one material at
//!   a time, each time with a material index the compiler knows is the same across the subgroup
//!   (retail_world.wgsl `fragment`). The material's parameters are read once for the warp, and
//!   its page lookups need no test or loop for an index that may differ; a warp holding k
//!   materials shades k times. The same pixels. With perf14's `NAK_DEBUG` and `lsink`, NAK
//!   compiles University's opaque world fragment to 2134 instructions rather than 2510, in the
//!   same 56 registers; without `lsink` it takes 64, so 32 warps at once rather than 36
//!   (findings 31). It needs `Features::SUBGROUP`; without it, it changes nothing, and the launch
//!   log says which (retail_render.rs `log_page_uniform_support`).
//! - `SKATE_WORLD_GPR48=1`: the opaque classes' main-pass world fragment is compiled in at most
//!   48 registers, by its entry point's name (retail_world.wgsl `fragment_gpr48`), which NVK's
//!   compiler reads as a cap with bevy-horizon's `patches/mesa-switch/0014`. Register allocation
//!   spills what doesn't fit to memory, so the fragment runs more instructions, but more warps fit
//!   on the GPU at once. The same pixels. With perf17's `NAK_DEBUG`, NAK compiles University's
//!   opaque world fragment to 2302 instructions with 44 spills in 48 registers, 40 warps at once,
//!   rather than 2142 in 56, 36 warps (findings 35). The other classes keep their fragments: the
//!   cutout and blended ones already fit in 48, and the water families' needs 112.
//!   Without the patch the name changes nothing.
//! - `SKATE_CUTOUT_FLAT=0`: the world's depth-only pipelines that test alpha (the view prepass
//!   and the shadow cascades) read the two words of the material the test needs, its diffuse
//!   slot and its cutoff, once a fragment from storage, as they did before. Unset, they read them
//!   once a vertex in the vertex stage, which hands them to the fragment stage flat
//!   (retail_depth.wgsl). Flat values come from the triangle's provoking vertex, as the material
//!   index does, so the same pixels. Each cutout fragment's texture lookup then starts without
//!   first waiting on a read from storage: on the Switch that took 9.3 ms off a view full of
//!   foliage, of the prepass's 14.9 (perf24, findings 43).
//! - `SKATE_WORLD_SHORT_LIVES=1`: the main pass's world fragment reads each of its material's
//!   textures where it folds the texel in, rather than all of them first, and builds its tangent
//!   frame only in the branches that use it (retail_world.wgsl `WORLD_SHORT_LIVES`). It does the
//!   same arithmetic on the same values, so the same pixels, but holds fewer values at once. With
//!   the Switch's `NAK_DEBUG` and naga's index checks, as the game runs, NAK compiles
//!   University's opaque world fragment to 2141 instructions in 48 registers, 40 warps at once,
//!   rather than 2153 in 56, 36 warps, with no spills (findings 48).
//! - `SKATE_WORLD_LEAN=1`: the main pass's world fragment leaves out its test of `foliage_debug`,
//!   which nothing sets, and, with `SKATE_WORLD_SHORT_LIVES`, builds a tangent frame from
//!   derivatives only where the map supplied none, rather than building one for every
//!   normal-mapped fragment and then replacing it (retail_world.wgsl `WORLD_LEAN`). The same
//!   pixels. Every vertex of a version 12 or later map carries a frame (skate_map.rs), so
//!   University's normal-mapped fragments skip the stand-in's arithmetic as they run.
//! - `SKATE_WORLD_IMPLICIT=1`: the main pass's page lookups let the texture unit work out their
//!   level of detail from the 2x2 quad, rather than handing it the gradients the fragment worked
//!   out (retail_material_bindings.wgsl `WORLD_IMPLICIT_LOD`). Each lookup then takes two
//!   coordinates and a layer rather than four more gradient components. The level can differ in
//!   its last bits, so a pixel between two levels can differ by a shade. Not the water families,
//!   whose refraction moves the coordinates after their gradients are taken, nor with
//!   `SKATE_WORLD_LOD` or `SKATE_WORLD_PROBE=3`, which set the level themselves.
//!
//! Each change logs a `[Skate] render options:` line.
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::VisibilitySystems;
use bevy::core_pipeline::prepass::{DepthPrepass, SkipPrepassDepthCopy};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::light::{
    CascadeShadowConfig, CascadeShadowConfigBuilder, NotShadowCaster, ShadowFilteringMethod,
};
use bevy::pbr::{ShadowScissors, SkipClearShadowLayers};
use bevy::prelude::*;
use bevy::render::experimental::occlusion_culling::OcclusionCulling;
use bevy::transform::TransformSystems;

/// The scene sun, whose cascades `RenderOptions` sets (retail_character.rs `spawn_shadow_sources`).
#[derive(Component)]
pub(crate) struct SceneSun;

#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub(crate) struct RenderOptions {
    pub depth_prepass: bool,
    pub sun_cascades: usize,
    pub sun_shadow_distance: f32,
    pub shadow_filter: ShadowFilteringMethod,
    pub draw_distance: Option<f32>,
    pub world_shadows: bool,
    pub shadow_bounds: bool,
    /// `SKATE_SHADOW_BOUNDS_TIGHT`: that sphere is fitted round each joint's vertices.
    pub shadow_bounds_tight: bool,
    /// `SKATE_SHADOW_BOUNDS_CHECK`: the casters are skinned on the CPU to check the sphere.
    pub shadow_bounds_check: bool,
    /// `SKATE_SHADOW_LOOKUPS`: the world looks up the skater's shadow at all.
    pub shadow_lookups: bool,
    /// `SKATE_HIDE_SKATER`: the skater is hidden, to measure what it costs.
    pub hide_skater: bool,
    pub sun_receivers: bool,
    /// `SKATE_SUN_SCISSOR`: the sun's cascades draw only within the texels read.
    pub sun_scissor: bool,
    /// `SKATE_PREPASS_DEPTH_COPY`: the prepass copies the depth for shaders to sample.
    pub prepass_depth_copy: bool,
    /// `SKATE_CASCADE_SKIP`: a cascade with nothing to draw into a clear layer is skipped.
    pub cascade_skip: bool,
    /// `SKATE_HUD_RETAIN`: the scoring HUD's camera and composite stop while it draws nothing.
    pub hud_retain: bool,
    /// `SKATE_HUD_RECT`: the scoring HUD's camera and composite cover only what it shows.
    pub hud_rect: bool,
    /// `SKATE_PRESENT_DIRECT`: at 100% scale the gameplay camera draws into the window.
    pub present_direct: bool,
    /// `SKATE_TONE_DIRECT`: while it does, its tone pass and UI write the window themselves.
    pub tone_direct: bool,
    /// `SKATE_TONE_GAMMA`: while the tone pass writes the window, it writes the curve's 8-bit
    /// values through a view that doesn't encode sRGB.
    pub tone_gamma: bool,
    /// `SKATE_PROCEDURAL_HDR`: a scene without a retail map is drawn through an HDR camera, and
    /// Bevy tonemaps it in a pass of its own rather than in each material's shader.
    pub procedural_hdr: bool,
    /// `SKATE_PROCEDURAL_TONEMAPPING`: Bevy's tonemapping for a scene without a retail map.
    pub procedural_tonemapping: Tonemapping,
    /// `SKATE_PROCEDURAL_CHARACTER`: on a scene without a retail map, the skater is shaded by the
    /// character shader retail maps use, not Bevy's lit `StandardMaterial`.
    pub procedural_character: bool,
    /// `SKATE_UPSCALE`: how the scene image is stretched to the window below 100% scale.
    pub upscale: Upscale,
    /// `SKATE_PRESENT_TO_WINDOW`: the presentation camera draws the image and the UI into the
    /// window itself.
    pub present_to_window: bool,
    /// `SKATE_OCCLUSION`: Bevy's GPU occlusion culling, with the depth prepass.
    pub occlusion: bool,
    /// `SKATE_OCCLUSION_SPAN`: 0 for Bevy's box test, else the finer test's span in texels.
    pub occlusion_span: u32,
    /// `SKATE_OCCLUSION_BATCH`: the finer test reads its texels eight at a time.
    pub occlusion_batch: bool,
    /// `SKATE_PYRAMID_ONCE`: the depth pyramid is built once a frame, after the early prepass.
    pub pyramid_once: bool,
    /// `SKATE_PYRAMID_TILE`: the depth pyramid's first levels come from shared-memory tiles.
    pub pyramid_tile: bool,
    /// `SKATE_PYRAMID_BLOCK`: the depth pyramid's first levels come from 2x2 blocks of gathers.
    pub pyramid_block: bool,
    /// `SKATE_LATE_PREPROCESS_DIRECT`: the late mesh preprocessing pass is dispatched directly.
    pub late_preprocess_direct: bool,
    /// `SKATE_INDIRECT_PHASE_FILTER`: each indirect-parameter build skips the phases it needn't.
    pub indirect_phase_filter: bool,
    /// `SKATE_TRIANGLE_CULL`: 0 for off, else bevy_pbr `TRIANGLE_CULL`'s setting.
    pub triangle_cull: u32,
    /// `SKATE_PREPASS_ORDER`: the view's opaque prepass draws the world's leaves nearest first.
    pub prepass_order: bool,
    /// `SKATE_CLUSTER_CULL`: 0 for off, else the span bevy_pbr `TRIANGLE_CULL_CLUSTERS` tests at.
    pub cluster_cull: u32,
    /// `SKATE_CLUSTER_CUTOUTS`: with `cluster_cull`, bevy_pbr `TRIANGLE_CULL_CLUSTERS_CUTOUTS`.
    pub cluster_cutouts: bool,
    /// `SKATE_CLUSTER_KERNEL`: with `cluster_cull`, bevy_pbr `TRIANGLE_CULL_CLUSTERS`'s second
    /// kernel.
    pub cluster_kernel: bool,
    pub world: WorldVariant,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            depth_prepass: cfg!(target_os = "horizon"),
            sun_cascades: 4,
            sun_shadow_distance: 100.,
            shadow_filter: ShadowFilteringMethod::Gaussian,
            draw_distance: None,
            world_shadows: true,
            shadow_bounds: true,
            shadow_bounds_tight: false,
            shadow_bounds_check: false,
            shadow_lookups: true,
            hide_skater: false,
            sun_receivers: true,
            sun_scissor: true,
            prepass_depth_copy: false,
            cascade_skip: true,
            hud_retain: true,
            hud_rect: false,
            present_direct: true,
            tone_direct: true,
            tone_gamma: true,
            procedural_hdr: cfg!(target_os = "horizon"),
            procedural_tonemapping: Tonemapping::TonyMcMapface,
            procedural_character: false,
            upscale: Upscale::Rcas,
            present_to_window: true,
            occlusion: false,
            occlusion_span: 0,
            occlusion_batch: true,
            pyramid_once: false,
            pyramid_tile: false,
            pyramid_block: false,
            late_preprocess_direct: true,
            indirect_phase_filter: true,
            triangle_cull: 0,
            prepass_order: false,
            cluster_cull: 0,
            cluster_cutouts: false,
            cluster_kernel: false,
            world: WorldVariant::default(),
        }
    }
}

/// How the camera that presents the scene image stretches it to the window below 100% scale, by
/// `SKATE_UPSCALE` above (graphics_menu.rs `choose_upscaler`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Upscale {
    /// FSR 1's sharpening (RCAS) on the image's bilinear stretch.
    Rcas,
    /// FSR 1's edge-adaptive upscale (EASU).
    Easu,
    /// The image's bilinear filter alone.
    Bilinear,
}

/// The world shader's variant, by `SKATE_WORLD_TRIM`, `SKATE_WORLD_PACKED`, `SKATE_WORLD_LOD`,
/// `SKATE_WORLD_PROBE`, `SKATE_PREPASS_PROBE`, `SKATE_WORLD_FETCH`, `SKATE_WORLD_PAGE_UNIFORM`,
/// `SKATE_WORLD_FEATURES`, `SKATE_WORLD_SLOT_WATERFALL`, `SKATE_WORLD_GPR48`, `SKATE_CUTOUT_FLAT`,
/// `SKATE_VIS_CULL`, `SKATE_VIS_CULL_AFTER_TEST`, `SKATE_WORLD_SHORT_LIVES`, `SKATE_WORLD_LEAN`
/// and `SKATE_WORLD_IMPLICIT` above. Every world material carries it, and keys its pipelines on
/// it (retail_render.rs `WorldMaterial::specialize`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WorldVariant {
    pub trim: bool,
    pub packed: bool,
    pub explicit_lod: bool,
    /// 0, or what `SKATE_WORLD_PROBE` leaves of the main pass: 1 its interpolation, 2 its
    /// geometry, 3 all but its texel traffic, 4 all but its texture lookups, 5 all but its page
    /// switches, 6 all but its waterfall's repeats.
    pub probe: u8,
    /// 0, or what `SKATE_PREPASS_PROBE` leaves of the view prepass's alpha test: 1 all but its
    /// texel traffic, 2 all but its lookup, 3 all but its page switch, 4 none of it, 5 none of it
    /// in a fragment stage that still runs, 6 only a `discard` that reads the UV.
    pub prepass_probe: u8,
    /// `SKATE_WORLD_FETCH`: the main pass fetches and interpolates its fragment's corners itself.
    pub fetch: bool,
    /// `SKATE_WORLD_PAGE_UNIFORM`: a page lookup reads its subgroup's page directly when the
    /// whole subgroup names one, given the page array and the device's subgroups.
    pub page_uniform: bool,
    /// `SKATE_WORLD_FEATURES`: the main pass compiles only the parts of the fragment its
    /// materials reach.
    pub features: bool,
    /// `SKATE_WORLD_SLOT_WATERFALL`: the main pass shades a subgroup's fragments one material at
    /// a time, given the device's subgroups.
    pub slot_waterfall: bool,
    /// `SKATE_WORLD_GPR48`: the opaque classes' main-pass fragment is compiled in at most 48
    /// registers.
    pub gpr48: bool,
    /// `SKATE_CUTOUT_FLAT`: the depth pipelines' alpha test takes its material's diffuse slot
    /// and cutoff flat from the vertex stage.
    pub cutout_flat: bool,
    /// 0, or the classes whose view prepass marks its triangles for `SKATE_VIS_CULL`: 1 all,
    /// 2 the opaque ones. The variable's `3` marks as `1` does, and sets 1 here.
    pub vis_cull: u8,
    /// `SKATE_VIS_CULL_AFTER_TEST`: the cutout classes, but for the water families, mark after
    /// their alpha test. Only with `vis_cull` 1, as no other marks a cutout.
    pub mark_after_test: bool,
    /// `SKATE_WORLD_SHORT_LIVES`: the main pass reads each texture where it folds the texel in.
    pub short_lives: bool,
    /// `SKATE_WORLD_LEAN`: the main pass leaves out the `foliage_debug` test and, with
    /// `short_lives`, builds a tangent frame only where the map supplied none.
    pub lean: bool,
    /// `SKATE_WORLD_IMPLICIT`: the main pass's page lookups take their level of detail from the
    /// quad, but for the water families' and with `explicit_lod` or `probe` 3.
    pub implicit_lod: bool,
}

impl Default for WorldVariant {
    fn default() -> Self {
        Self {
            trim: true,
            packed: true,
            explicit_lod: false,
            probe: 0,
            prepass_probe: 0,
            fetch: false,
            page_uniform: false,
            features: false,
            slot_waterfall: false,
            gpr48: false,
            cutout_flat: true,
            vis_cull: 0,
            mark_after_test: false,
            short_lives: false,
            lean: false,
            implicit_lod: false,
        }
    }
}

impl RenderOptions {
    pub(crate) fn from_env() -> Self {
        Self::parse(|name| std::env::var(name).ok())
    }

    /// The options `var` names, by the environment variables above.
    pub(crate) fn parse(var: impl Fn(&str) -> Option<String>) -> Self {
        let defaults = Self::default();
        // On or off when set to either, else the default.
        let flag = |name: &str, default: bool| match var(name).as_deref().map(str::trim) {
            Some("1" | "true" | "on") => true,
            Some("0" | "false" | "off") => false,
            _ => default,
        };
        let depth_prepass = flag("SKATE_DEPTH_PREPASS", defaults.depth_prepass);
        // Bevy draws at most four cascades per light (bevy_pbr `MAX_CASCADES_PER_LIGHT`).
        let sun_cascades = var("SKATE_SUN_CASCADES")
            .and_then(|value| value.trim().parse::<usize>().ok())
            .map_or(defaults.sun_cascades, |cascades| cascades.clamp(1, 4));
        // The cascades start at 0.1 m (`CascadeShadowConfigBuilder::minimum_distance`).
        let sun_shadow_distance = var("SKATE_SUN_SHADOW_DISTANCE")
            .and_then(|value| value.trim().parse::<f32>().ok())
            .filter(|distance| distance.is_finite() && *distance >= 1.)
            .unwrap_or(defaults.sun_shadow_distance);
        let shadow_filter = match var("SKATE_SHADOW_FILTER").as_deref().map(str::trim) {
            Some("hardware") => ShadowFilteringMethod::Hardware2x2,
            Some("temporal") => ShadowFilteringMethod::Temporal,
            _ => defaults.shadow_filter,
        };
        let draw_distance = var("SKATE_DRAW_DISTANCE")
            .and_then(|value| value.trim().parse::<f32>().ok())
            .filter(|distance| distance.is_finite() && *distance >= 1.);
        let world_shadows = flag("SKATE_WORLD_SHADOWS", defaults.world_shadows);
        let shadow_bounds = flag("SKATE_SHADOW_BOUNDS", defaults.shadow_bounds);
        let shadow_bounds_tight =
            flag("SKATE_SHADOW_BOUNDS_TIGHT", defaults.shadow_bounds_tight);
        let shadow_bounds_check =
            flag("SKATE_SHADOW_BOUNDS_CHECK", defaults.shadow_bounds_check);
        let shadow_lookups = flag("SKATE_SHADOW_LOOKUPS", defaults.shadow_lookups);
        let hide_skater = flag("SKATE_HIDE_SKATER", defaults.hide_skater);
        let sun_receivers = flag("SKATE_SUN_RECEIVERS", defaults.sun_receivers);
        let sun_scissor = flag("SKATE_SUN_SCISSOR", defaults.sun_scissor);
        let prepass_depth_copy = flag("SKATE_PREPASS_DEPTH_COPY", defaults.prepass_depth_copy);
        let cascade_skip = flag("SKATE_CASCADE_SKIP", defaults.cascade_skip);
        let hud_retain = flag("SKATE_HUD_RETAIN", defaults.hud_retain);
        let hud_rect = flag("SKATE_HUD_RECT", defaults.hud_rect);
        let present_direct = flag("SKATE_PRESENT_DIRECT", defaults.present_direct);
        let tone_direct = flag("SKATE_TONE_DIRECT", defaults.tone_direct);
        let tone_gamma = flag("SKATE_TONE_GAMMA", defaults.tone_gamma);
        let procedural_hdr = flag("SKATE_PROCEDURAL_HDR", defaults.procedural_hdr);
        let procedural_tonemapping =
            match var("SKATE_PROCEDURAL_TONEMAPPING").as_deref().map(str::trim) {
                Some(name) if name.eq_ignore_ascii_case("none") => Tonemapping::None,
                Some(name) if name.eq_ignore_ascii_case("aces") => Tonemapping::AcesFitted,
                Some(name) if name.eq_ignore_ascii_case("tony") => Tonemapping::TonyMcMapface,
                _ => defaults.procedural_tonemapping,
            };
        let procedural_character =
            flag("SKATE_PROCEDURAL_CHARACTER", defaults.procedural_character);
        let upscale = match var("SKATE_UPSCALE").as_deref().map(str::trim) {
            Some(name) if name.eq_ignore_ascii_case("rcas") => Upscale::Rcas,
            Some(name) if name.eq_ignore_ascii_case("easu") => Upscale::Easu,
            Some(name) if name.eq_ignore_ascii_case("bilinear") => Upscale::Bilinear,
            _ => defaults.upscale,
        };
        let present_to_window = flag("SKATE_PRESENT_TO_WINDOW", defaults.present_to_window);
        let occlusion = flag("SKATE_OCCLUSION", defaults.occlusion);
        let occlusion_span = var("SKATE_OCCLUSION_SPAN")
            .and_then(|value| value.trim().parse::<u32>().ok())
            .map_or(defaults.occlusion_span, |span| span.min(32));
        let occlusion_batch = flag("SKATE_OCCLUSION_BATCH", defaults.occlusion_batch);
        let pyramid_once = flag("SKATE_PYRAMID_ONCE", defaults.pyramid_once);
        let pyramid_tile = flag("SKATE_PYRAMID_TILE", defaults.pyramid_tile);
        let pyramid_block = flag("SKATE_PYRAMID_BLOCK", defaults.pyramid_block);
        let late_preprocess_direct =
            flag("SKATE_LATE_PREPROCESS_DIRECT", defaults.late_preprocess_direct);
        let indirect_phase_filter =
            flag("SKATE_INDIRECT_PHASE_FILTER", defaults.indirect_phase_filter);
        let triangle_cull = match var("SKATE_TRIANGLE_CULL").as_deref().map(str::trim) {
            Some("nodepth") => bevy::pbr::TRIANGLE_CULL_NO_DEPTH,
            Some(value) => value
                .parse::<u32>()
                .map_or(defaults.triangle_cull, |span| span.min(32)),
            None => defaults.triangle_cull,
        };
        let triangle_cull_debug = var("SKATE_TRIANGLE_CULL_DEBUG")
            .and_then(|value| value.trim().parse::<u32>().ok())
            .map_or(0, |bits| (bits & 63) * bevy::pbr::TRIANGLE_CULL_DEBUG_MIRRORS_ONLY);
        let triangle_cull_v1 = if flag("SKATE_TRIANGLE_CULL_V1", false) {
            bevy::pbr::TRIANGLE_CULL_V1
        } else {
            0
        };
        let triangle_cull = if triangle_cull == 0 {
            0
        } else {
            triangle_cull | triangle_cull_debug | triangle_cull_v1
        };
        let vis_cull = var("SKATE_VIS_CULL")
            .and_then(|value| value.trim().parse::<u8>().ok())
            .filter(|vis_cull| *vis_cull <= 3)
            .unwrap_or(defaults.world.vis_cull);
        let marks_kernel = match var("SKATE_VIS_CULL_KERNEL").as_deref().map(str::trim) {
            Some("0") => bevy::pbr::TRIANGLE_CULL_MARKS_IN_CULL,
            Some("1") => 0,
            Some("2") => bevy::pbr::TRIANGLE_CULL_MARKS_COALESCED,
            Some("4") => {
                bevy::pbr::TRIANGLE_CULL_MARKS_BALLOT | bevy::pbr::TRIANGLE_CULL_MARKS_BALLOT_8
            }
            _ => bevy::pbr::TRIANGLE_CULL_MARKS_BALLOT,
        };
        // The marks take the place of the cull's own tests, which only the second version has.
        let triangle_cull = match vis_cull {
            0 => triangle_cull,
            1 => bevy::pbr::TRIANGLE_CULL_MARKS | marks_kernel | triangle_cull_debug,
            2 => {
                bevy::pbr::TRIANGLE_CULL_MARKS
                    | bevy::pbr::TRIANGLE_CULL_MARKS_OPAQUE_ONLY
                    | marks_kernel
                    | triangle_cull_debug
            }
            _ => {
                bevy::pbr::TRIANGLE_CULL_MARKS
                    | bevy::pbr::TRIANGLE_CULL_MARKS_INVERT
                    | marks_kernel
                    | triangle_cull_debug
            }
        };
        // `3` marks as `1` does, and the materials keep the pipelines they have for it.
        let vis_cull = if vis_cull == 3 { 1 } else { vis_cull };
        let prepass_order = flag("SKATE_PREPASS_ORDER", defaults.prepass_order);
        let cluster_cull = var("SKATE_CLUSTER_CULL")
            .and_then(|value| value.trim().parse::<u32>().ok())
            .map_or(defaults.cluster_cull, |span| span.min(32));
        let cluster_cutouts = flag("SKATE_CLUSTER_CUTOUTS", defaults.cluster_cutouts);
        let cluster_kernel = flag("SKATE_CLUSTER_KERNEL", defaults.cluster_kernel);
        let world = WorldVariant {
            trim: flag("SKATE_WORLD_TRIM", defaults.world.trim),
            packed: flag("SKATE_WORLD_PACKED", defaults.world.packed),
            explicit_lod: flag("SKATE_WORLD_LOD", defaults.world.explicit_lod),
            probe: var("SKATE_WORLD_PROBE")
                .and_then(|value| value.trim().parse::<u8>().ok())
                .filter(|probe| *probe <= 6)
                .unwrap_or(defaults.world.probe),
            prepass_probe: var("SKATE_PREPASS_PROBE")
                .and_then(|value| value.trim().parse::<u8>().ok())
                .filter(|probe| *probe <= 6)
                .unwrap_or(defaults.world.prepass_probe),
            fetch: flag("SKATE_WORLD_FETCH", defaults.world.fetch),
            page_uniform: flag("SKATE_WORLD_PAGE_UNIFORM", defaults.world.page_uniform),
            features: flag("SKATE_WORLD_FEATURES", defaults.world.features),
            slot_waterfall: flag("SKATE_WORLD_SLOT_WATERFALL", defaults.world.slot_waterfall),
            gpr48: flag("SKATE_WORLD_GPR48", defaults.world.gpr48),
            cutout_flat: flag("SKATE_CUTOUT_FLAT", defaults.world.cutout_flat),
            vis_cull,
            mark_after_test: vis_cull == 1
                && flag("SKATE_VIS_CULL_AFTER_TEST", defaults.world.mark_after_test),
            short_lives: flag("SKATE_WORLD_SHORT_LIVES", defaults.world.short_lives),
            lean: flag("SKATE_WORLD_LEAN", defaults.world.lean),
            implicit_lod: flag("SKATE_WORLD_IMPLICIT", defaults.world.implicit_lod),
        };
        Self {
            depth_prepass,
            sun_cascades,
            sun_shadow_distance,
            shadow_filter,
            draw_distance,
            world_shadows,
            shadow_bounds,
            shadow_bounds_tight,
            shadow_bounds_check,
            shadow_lookups,
            hide_skater,
            sun_receivers,
            sun_scissor,
            prepass_depth_copy,
            cascade_skip,
            hud_retain,
            hud_rect,
            present_direct,
            tone_direct,
            tone_gamma,
            procedural_hdr,
            procedural_tonemapping,
            procedural_character,
            upscale,
            present_to_window,
            occlusion,
            occlusion_span,
            occlusion_batch,
            pyramid_once,
            pyramid_tile,
            pyramid_block,
            late_preprocess_direct,
            indirect_phase_filter,
            triangle_cull,
            prepass_order,
            cluster_cull,
            cluster_cutouts,
            cluster_kernel,
            world,
        }
    }

    /// The scene sun's cascades. Unset, the 10 m first cascade and 100 m reach the game always had.
    pub(crate) fn sun_cascade_config(&self) -> CascadeShadowConfig {
        CascadeShadowConfigBuilder {
            num_cascades: self.sun_cascades,
            maximum_distance: self.sun_shadow_distance,
            first_cascade_far_bound: 10_f32.min(self.sun_shadow_distance * 0.5),
            ..default()
        }
        .build()
    }
}

/// `SKATE_WORLD_HOIST`, read at the first world pipeline (retail_render.rs
/// `WorldMaterial::specialize`).
pub(crate) fn world_hoist() -> bool {
    static HOIST: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        let hoist = std::env::var("SKATE_WORLD_HOIST")
            .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "on"));
        if hoist {
            eprintln!("[Skate] render options: SKATE_WORLD_HOIST");
        }
        hoist
    });
    *HOIST
}

/// `SKATE_WORLD_PAGE_ARRAY`, read at the world material's bind group layout (retail_render.rs
/// `WorldMaterial::bind_group_layout_entries`).
pub(crate) fn world_page_array() -> bool {
    static PAGE_ARRAY: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        let page_array = std::env::var("SKATE_WORLD_PAGE_ARRAY")
            .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "on"));
        if page_array {
            eprintln!("[Skate] render options: SKATE_WORLD_PAGE_ARRAY");
        }
        page_array
    });
    *PAGE_ARRAY
}

/// `SKATE_WORLD_SHADOW_SPLIT`, read when the first map's material table is built
/// (retail_render.rs `MaterialTable::build`).
pub(crate) fn world_shadow_split() -> bool {
    static SHADOW_SPLIT: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        let shadow_split = std::env::var("SKATE_WORLD_SHADOW_SPLIT")
            .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "on"));
        if shadow_split {
            eprintln!("[Skate] render options: SKATE_WORLD_SHADOW_SPLIT");
        }
        shadow_split
    });
    *SHADOW_SPLIT
}

/// Whether a map builds the vertex copy `SKATE_WORLD_FETCH` reads (skate_world.rs
/// `FetchGeometry`): when the variant is on at launch, or a `SKATE_PERF_MATRIX` step turns it on.
/// Without the copy the variant reads a placeholder, so it is read once, at the first map.
pub(crate) fn world_fetch_geometry() -> bool {
    static FETCH: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        let var = |name: &str| std::env::var(name).ok();
        let fetch = fetch_geometry_wanted(var);
        eprintln!("[Skate] world fetch geometry: {}", if fetch { "built" } else { "not built" });
        fetch
    });
    *FETCH
}

/// Whether a map builds `SKATE_VIS_CULL`'s marks (skate_world.rs `spawn`), which the world
/// material then binds where the device can mark (retail_render.rs `vis_cull_supported`): when
/// the variable is 1, 2 or 3 at launch, or a `SKATE_PERF_MATRIX` step sets `vcull` to one. Not
/// in a session that builds the fetch copy, whose leaves' tags hold where their triangles start
/// in it. Read once, before Bevy derives the world material's bind group layout.
pub(crate) fn world_marks() -> bool {
    static MARKS: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        let var = |name: &str| std::env::var(name).ok();
        if !marks_wanted(var) {
            return false;
        }
        let fetch = fetch_geometry_wanted(var);
        eprintln!(
            "[Skate] world marks: {}",
            if fetch { "not built, as the fetch copy is" } else { "built" }
        );
        !fetch
    });
    *MARKS
}

fn marks_wanted(var: impl Fn(&str) -> Option<String>) -> bool {
    let on = |value: &str| matches!(value.trim(), "1" | "2" | "3");
    var("SKATE_VIS_CULL").is_some_and(|value| on(&value))
        || var("SKATE_PERF_MATRIX").is_some_and(|matrix| {
            matrix.split([';', ',']).any(|item| {
                item.split_once('=').is_some_and(|(key, value)| key.trim() == "vcull" && on(value))
            })
        })
}

/// Whether a map builds `SKATE_CLUSTER_CULL`'s boxes (skate_world.rs `push_cluster_boxes`): when
/// the variable is above 0 at launch, or a `SKATE_PERF_MATRIX` step sets `ccull` above 0, and the
/// map builds the marks, as the pass finds a leaf's boxes by where its marks start. Read once, at
/// the first map.
pub(crate) fn world_clusters() -> bool {
    static CLUSTERS: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        let var = |name: &str| std::env::var(name).ok();
        if !clusters_wanted(var) {
            return false;
        }
        let marks = world_marks();
        eprintln!(
            "[Skate] world cluster boxes: {}",
            if marks { "built" } else { "not built, as the marks aren't" }
        );
        marks
    });
    *CLUSTERS
}

fn clusters_wanted(var: impl Fn(&str) -> Option<String>) -> bool {
    let on = |value: &str| value.trim().parse::<u32>().is_ok_and(|span| span > 0);
    var("SKATE_CLUSTER_CULL").is_some_and(|value| on(&value))
        || var("SKATE_PERF_MATRIX").is_some_and(|matrix| {
            matrix.split([';', ',']).any(|item| {
                item.split_once('=').is_some_and(|(key, value)| key.trim() == "ccull" && on(value))
            })
        })
}

fn fetch_geometry_wanted(var: impl Fn(&str) -> Option<String>) -> bool {
    let on = |value: &str| matches!(value.trim(), "1" | "true" | "on");
    var("SKATE_WORLD_FETCH").is_some_and(|value| on(&value))
        || var("SKATE_PERF_MATRIX").is_some_and(|matrix| {
            matrix.split([';', ',']).any(|item| {
                item.split_once('=').is_some_and(|(key, value)| key.trim() == "fetch" && on(value))
            })
        })
}

pub(crate) struct RenderOptionsPlugin;

impl Plugin for RenderOptionsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(RenderOptions::from_env()).init_resource::<ShadowScissors>().add_systems(
            PostUpdate,
            (
                apply,
                world_shadows,
                world_variant,
                draw_distance
                    .after(TransformSystems::Propagate)
                    .before(VisibilitySystems::VisibilityPropagate),
                hide_skater.before(VisibilitySystems::VisibilityPropagate),
            ),
        );
        if let Some(render) = app.get_sub_app_mut(bevy::render::RenderApp) {
            render.add_systems(bevy::render::ExtractSchedule, extract_render_skips);
        }
    }
}

/// Frames that `SKATE_VIS_CULL`'s marks wait, once the options or the world variant change,
/// before the cull keeps only the marked triangles, and after them, until the render world's
/// pipeline cache has nothing left to compile. The world materials take a new variant in a later
/// frame's `PostUpdate` (`world_variant`), and their draws are specialized again after that, at
/// first with pipelines still to compile, which Bevy doesn't draw with. Until the prepass draws
/// with marking pipelines, nothing marks, and the cull would keep no triangle: on the Switch, the
/// black frame at each `SKATE_PERF_MATRIX` step that turned the marks on (findings 46).
/// Meanwhile the cull is off and the leaves are drawn whole.
const MARKS_HOLD_FRAMES: u32 = 4;

/// Frames after which the marks are kept even with pipelines still waiting, as one whose shader
/// never loads is forever. The cull still waits for the prepass's own (triangle_cull.rs).
const MARKS_HOLD_MAX_FRAMES: u32 = 150;

/// `extract_render_skips`' state for `MARKS_HOLD_FRAMES`: the cull setting and world variant the
/// marks last waited on, the frames since they changed, and whether the wait is over.
#[derive(Default)]
struct MarksHold {
    options: Option<(u32, WorldVariant)>,
    frames: u32,
    released: bool,
}

/// Puts `SKATE_PREPASS_DEPTH_COPY` and `SKATE_CASCADE_SKIP` into the render world, where the
/// prepass and shadow pass nodes read them, with the rectangles `SKATE_SUN_SCISSOR` keeps the
/// sun's cascades to (retail_shadow_bounds.rs `sun_receivers`), `SKATE_OCCLUSION_SPAN`,
/// `SKATE_OCCLUSION_BATCH` and
/// `SKATE_LATE_PREPROCESS_DIRECT` where the mesh preprocessing nodes do,
/// `SKATE_INDIRECT_PHASE_FILTER` where the indirect-parameter building nodes do,
/// `SKATE_PYRAMID_ONCE`, `SKATE_PYRAMID_TILE` and `SKATE_PYRAMID_BLOCK` where the depth
/// pyramid's does, and
/// `SKATE_TRIANGLE_CULL` and `SKATE_CLUSTER_CULL` where the triangle culling passes do
/// (vendor/README.md).
fn extract_render_skips(
    mut commands: Commands,
    options: bevy::render::Extract<Res<RenderOptions>>,
    main_scissors: bevy::render::Extract<Option<Res<ShadowScissors>>>,
    copy_skipped: Option<Res<SkipPrepassDepthCopy>>,
    cascades_skipped: Option<Res<SkipClearShadowLayers>>,
    scissors: Option<ResMut<ShadowScissors>>,
    pipeline_cache: Option<Res<bevy::render::render_resource::PipelineCache>>,
    mut hold: Local<MarksHold>,
) {
    bevy::pbr::OCCLUSION_TEST_SPAN
        .store(options.occlusion_span, std::sync::atomic::Ordering::Relaxed);
    bevy::pbr::OCCLUSION_TEST_BATCH
        .store(options.occlusion_batch, std::sync::atomic::Ordering::Relaxed);
    bevy::pbr::LATE_PREPROCESS_DIRECT
        .store(options.late_preprocess_direct, std::sync::atomic::Ordering::Relaxed);
    bevy::pbr::INDIRECT_PHASE_FILTER
        .store(options.indirect_phase_filter, std::sync::atomic::Ordering::Relaxed);
    bevy::core_pipeline::experimental::mip_generation::SKIP_LATE_DEPTH_PYRAMID
        .store(options.pyramid_once, std::sync::atomic::Ordering::Relaxed);
    bevy::core_pipeline::experimental::mip_generation::PYRAMID_TILE
        .store(options.pyramid_tile, std::sync::atomic::Ordering::Relaxed);
    bevy::core_pipeline::experimental::mip_generation::PYRAMID_BLOCK
        .store(options.pyramid_block, std::sync::atomic::Ordering::Relaxed);
    // The fetch variant's leaves carry their first triangle in their `MeshTag`, not the pass's
    // bits, and draw from it. Where nothing marks, the marks would keep no triangle.
    let key = (options.triangle_cull, options.world);
    if hold.options != Some(key) {
        *hold = MarksHold { options: Some(key), frames: 0, released: false };
    }
    hold.frames = hold.frames.saturating_add(1);
    if !hold.released && hold.frames > MARKS_HOLD_FRAMES {
        hold.released = hold.frames > MARKS_HOLD_MAX_FRAMES
            || pipeline_cache.is_none_or(|cache| cache.waiting_pipelines().next().is_none());
        if hold.released && crate::retail_render::retail_map() && options.triangle_cull & bevy::pbr::TRIANGLE_CULL_MARKS != 0 {
            info!("[Skate] SKATE_VIS_CULL: marks kept from the change's frame {}", hold.frames);
        }
    }
    let unmarked = options.triangle_cull & bevy::pbr::TRIANGLE_CULL_MARKS != 0
        && (!crate::retail_render::vis_cull_supported()
            || !hold.released
            || !crate::retail_render::retail_map());
    let triangle_cull =
        if options.world.fetch || unmarked { 0 } else { options.triangle_cull };
    bevy::pbr::TRIANGLE_CULL.store(triangle_cull, std::sync::atomic::Ordering::Relaxed);
    // The clusters' passes feed the cull, and find a leaf's boxes by where its marks start.
    let cutouts = if options.cluster_cutouts {
        bevy::pbr::TRIANGLE_CULL_CLUSTERS_CUTOUTS
    } else {
        0
    };
    let kernel =
        u32::from(options.cluster_kernel) << bevy::pbr::TRIANGLE_CULL_CLUSTERS_KERNEL_SHIFT;
    bevy::pbr::TRIANGLE_CULL_CLUSTERS.store(
        if triangle_cull != 0 && world_clusters() && options.cluster_cull != 0 {
            options.cluster_cull | cutouts | kernel
        } else {
            0
        },
        std::sync::atomic::Ordering::Relaxed,
    );
    if options.prepass_depth_copy == copy_skipped.is_some() {
        if options.prepass_depth_copy {
            commands.remove_resource::<SkipPrepassDepthCopy>();
        } else {
            commands.insert_resource(SkipPrepassDepthCopy);
        }
    }
    if options.cascade_skip != cascades_skipped.is_some() {
        if options.cascade_skip {
            commands.init_resource::<SkipClearShadowLayers>();
        } else {
            commands.remove_resource::<SkipClearShadowLayers>();
        }
    }
    match (main_scissors.as_deref(), scissors) {
        (Some(main), Some(mut scissors)) => {
            scissors.size = main.size;
            scissors.rects.clone_from(&main.rects);
        }
        (Some(main), None) => commands.insert_resource(main.clone()),
        (None, Some(_)) => commands.remove_resource::<ShadowScissors>(),
        (None, None) => {}
    }
}

/// Puts the options on the gameplay camera and the scene sun, when either is new or the options
/// change.
#[allow(clippy::type_complexity)]
pub(crate) fn apply(
    options: Res<RenderOptions>,
    mut commands: Commands,
    cameras: Query<
        (Entity, Has<DepthPrepass>, Has<OcclusionCulling>, Option<&ShadowFilteringMethod>),
        With<crate::camera::GameplayCamera>,
    >,
    mut suns: Query<&mut CascadeShadowConfig, With<SceneSun>>,
    new_cameras: Query<(), Added<crate::camera::GameplayCamera>>,
    new_suns: Query<(), Added<SceneSun>>,
) {
    if !options.is_changed() && new_cameras.is_empty() && new_suns.is_empty() {
        return;
    }
    if options.is_changed() {
        eprintln!("[Skate] render options: {:?}", *options);
    }
    for (entity, prepass, culling, filter) in &cameras {
        let mut camera = commands.entity(entity);
        if options.depth_prepass && !prepass {
            camera.insert(DepthPrepass);
        } else if !options.depth_prepass && prepass && options.is_changed() {
            camera.remove::<DepthPrepass>();
        }
        let occlusion = options.occlusion && options.depth_prepass;
        if occlusion && !culling {
            camera.insert(OcclusionCulling);
        } else if !occlusion && culling && options.is_changed() {
            camera.remove::<OcclusionCulling>();
        }
        // Bevy treats a camera without the component as `Gaussian` (bevy_pbr mesh.rs).
        if filter.copied().unwrap_or_default() != options.shadow_filter {
            camera.insert(options.shadow_filter);
        }
    }
    let config = options.sun_cascade_config();
    for mut cascades in &mut suns {
        if cascades.bounds != config.bounds {
            *cascades = config.clone();
        }
    }
}

/// Takes the world leaves out of the shadow casters under `SKATE_WORLD_SHADOWS=0`, and puts them
/// back when a `SKATE_PERF_MATRIX` step turns it on again. Bevy re-extracts a mesh when
/// `NotShadowCaster` is removed as well as when it's added (bevy_pbr
/// `extract_meshes_for_gpu_building`).
fn world_shadows(
    options: Res<RenderOptions>,
    mut commands: Commands,
    leaves: Query<
        (Entity, Has<NotShadowCaster>),
        With<MeshMaterial3d<crate::retail_render::WorldMaterial>>,
    >,
    new_leaves: Query<(), Added<MeshMaterial3d<crate::retail_render::WorldMaterial>>>,
) {
    if !options.is_changed() && new_leaves.is_empty() {
        return;
    }
    for (leaf, not_caster) in &leaves {
        if options.world_shadows && not_caster {
            commands.entity(leaf).try_remove::<NotShadowCaster>();
        } else if !options.world_shadows && !not_caster {
            commands.entity(leaf).try_insert(NotShadowCaster);
        }
    }
}

/// The world variant a map's materials take: `options.world`, with the marks off on a map that
/// isn't retail, so nothing marks and the cull keeps nothing (`retail_render::retail_map`).
pub(crate) fn map_world_variant(world: WorldVariant) -> WorldVariant {
    if crate::retail_render::retail_map() {
        world
    } else {
        WorldVariant { vis_cull: 0, mark_after_test: false, ..world }
    }
}

/// Puts each map's world variant on every world material, so a `SKATE_PERF_MATRIX` step can switch
/// the world's shader variant. Bevy prepares a changed material again and specializes its draws
/// again (bevy_pbr `specialize_material_meshes` checks `AssetChanged`), which compiles the
/// variant's pipelines the first time. A map builds its materials with the launch variant
/// (retail_render.rs `WorldMaterial::with_pages`), so for a retail map this changes nothing; a
/// custom map's materials lose the marks when it commits.
fn world_variant(
    options: Res<RenderOptions>,
    materials: Option<ResMut<Assets<crate::retail_render::WorldMaterial>>>,
) {
    let Some(mut materials) = materials else { return };
    let wanted = map_world_variant(options.world);
    let stale: Vec<_> = materials
        .iter()
        .filter(|(_, material)| material.variant != wanted)
        .map(|(id, _)| id)
        .collect();
    for id in stale {
        if let Some(material) = materials.get_mut(id) {
            material.variant = wanted;
        }
    }
}

/// Hides the world leaves beyond `SKATE_DRAW_DISTANCE`, measured to the nearest point of each
/// leaf's bounds. A leaf holds up to 16384 triangles (skate_world.rs) and in open ground spans
/// hundreds of metres, so the distance to its centre, which Bevy's `VisibilityRange` uses, would
/// hide the ground under the skater. Hidden leaves leave the shadow cascades too.
fn draw_distance(
    options: Res<RenderOptions>,
    cameras: Query<&GlobalTransform, With<crate::camera::GameplayCamera>>,
    mut leaves: Query<
        (&Aabb, &mut Visibility),
        With<MeshMaterial3d<crate::retail_render::WorldMaterial>>,
    >,
) {
    let Some(reach) = options.draw_distance else {
        if options.is_changed() {
            for (_, mut visibility) in &mut leaves {
                visibility.set_if_neq(Visibility::Inherited);
            }
        }
        return;
    };
    let Some(eye) = cameras.iter().next().map(GlobalTransform::translation) else {
        return;
    };
    for (bounds, mut visibility) in &mut leaves {
        visibility.set_if_neq(if beyond(bounds, eye, reach) {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        });
    }
}

fn beyond(bounds: &Aabb, eye: Vec3, reach: f32) -> bool {
    let nearest = eye.clamp(bounds.min().into(), bounds.max().into());
    eye.distance_squared(nearest) > reach * reach
}

/// Hides the skater under `SKATE_HIDE_SKATER`, and shows it again when a `SKATE_PERF_MATRIX`
/// step turns it off. What the skater draws hangs from its root (world.rs `PlayerRoot`), which
/// nothing below sets `Visible`, so the root's `Hidden` hides all of it. The animation binds only
/// to skins it can see (animation.rs `bind`), so the root stays shown until it has bound.
fn hide_skater(
    options: Res<RenderOptions>,
    animation: Option<Res<crate::animation::AnimationStatus>>,
    mut roots: Query<&mut Visibility, With<crate::world::PlayerRoot>>,
    mut hid: Local<bool>,
) {
    if options.hide_skater && animation.is_none_or(|animation| animation.ready) {
        for mut visibility in &mut roots {
            visibility.set_if_neq(Visibility::Hidden);
        }
        *hid = true;
    } else if std::mem::take(&mut *hid) {
        for mut visibility in &mut roots {
            visibility.set_if_neq(Visibility::Inherited);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(pairs: &[(&str, &str)]) -> RenderOptions {
        RenderOptions::parse(|name| {
            pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| value.to_string())
        })
    }

    #[test]
    fn unset_keeps_the_previous_rendering() {
        let options = parse(&[]);
        assert_eq!(options, RenderOptions::default());
        let config = options.sun_cascade_config();
        let previous = CascadeShadowConfigBuilder {
            maximum_distance: 100.,
            first_cascade_far_bound: 10.,
            ..default()
        }
        .build();
        assert_eq!(config.bounds, previous.bounds);
        assert_eq!(config.minimum_distance, previous.minimum_distance);
        assert_eq!(config.overlap_proportion, previous.overlap_proportion);
    }

    #[test]
    fn values_are_parsed_and_clamped() {
        let options = parse(&[
            ("SKATE_DEPTH_PREPASS", "1"),
            ("SKATE_SUN_CASCADES", "9"),
            ("SKATE_SUN_SHADOW_DISTANCE", "8"),
            ("SKATE_SHADOW_FILTER", "hardware"),
        ]);
        assert!(options.depth_prepass);
        assert_eq!(options.sun_cascades, 4);
        assert_eq!(options.sun_shadow_distance, 8.);
        assert_eq!(options.shadow_filter, ShadowFilteringMethod::Hardware2x2);
        // A reach under 20 m pulls the first cascade in rather than tripping the builder.
        assert_eq!(options.sun_cascade_config().bounds.len(), 4);
        assert_eq!(parse(&[("SKATE_SUN_CASCADES", "0")]).sun_cascades, 1);
        assert_eq!(parse(&[("SKATE_SUN_SHADOW_DISTANCE", "nan")]).sun_shadow_distance, 100.);
        assert!(!parse(&[("SKATE_DEPTH_PREPASS", "0")]).depth_prepass);
        assert_eq!(parse(&[("SKATE_DRAW_DISTANCE", "250")]).draw_distance, Some(250.));
        assert_eq!(parse(&[("SKATE_DRAW_DISTANCE", "0")]).draw_distance, None);
        assert!(!parse(&[("SKATE_WORLD_SHADOWS", "0")]).world_shadows);
        assert!(parse(&[("SKATE_WORLD_SHADOWS", "1")]).world_shadows);
        assert!(!parse(&[("SKATE_SHADOW_BOUNDS", "0")]).shadow_bounds);
        assert!(parse(&[("SKATE_SHADOW_BOUNDS", "1")]).shadow_bounds);
        assert!(!parse(&[]).shadow_bounds_tight);
        assert!(parse(&[("SKATE_SHADOW_BOUNDS_TIGHT", "1")]).shadow_bounds_tight);
        assert!(!parse(&[]).shadow_bounds_check);
        assert!(parse(&[("SKATE_SHADOW_BOUNDS_CHECK", "1")]).shadow_bounds_check);
        assert!(parse(&[]).shadow_lookups);
        assert!(!parse(&[("SKATE_SHADOW_LOOKUPS", "0")]).shadow_lookups);
        assert!(!parse(&[]).hide_skater);
        assert!(parse(&[("SKATE_HIDE_SKATER", "1")]).hide_skater);
        assert!(!parse(&[("SKATE_SUN_RECEIVERS", "0")]).sun_receivers);
        assert!(parse(&[("SKATE_SUN_RECEIVERS", "1")]).sun_receivers);
        assert!(parse(&[]).sun_scissor);
        assert!(!parse(&[("SKATE_SUN_SCISSOR", "0")]).sun_scissor);
        assert!(parse(&[("SKATE_SUN_SCISSOR", "1")]).sun_scissor);
        assert!(parse(&[("SKATE_PREPASS_DEPTH_COPY", "1")]).prepass_depth_copy);
        assert!(!parse(&[("SKATE_CASCADE_SKIP", "0")]).cascade_skip);
        assert!(!parse(&[("SKATE_HUD_RETAIN", "0")]).hud_retain);
        assert!(parse(&[("SKATE_HUD_RETAIN", "1")]).hud_retain);
        assert!(!parse(&[]).hud_rect);
        assert!(parse(&[("SKATE_HUD_RECT", "1")]).hud_rect);
        assert!(!parse(&[]).prepass_order);
        assert!(parse(&[("SKATE_PREPASS_ORDER", "1")]).prepass_order);
        assert!(!parse(&[("SKATE_PREPASS_ORDER", "0")]).prepass_order);
        assert_eq!(parse(&[]).cluster_cull, 0);
        assert_eq!(parse(&[("SKATE_CLUSTER_CULL", "8")]).cluster_cull, 8);
        assert_eq!(parse(&[("SKATE_CLUSTER_CULL", "99")]).cluster_cull, 32);
        assert_eq!(parse(&[("SKATE_CLUSTER_CULL", "x")]).cluster_cull, 0);
        assert!(!parse(&[]).cluster_cutouts);
        assert!(parse(&[("SKATE_CLUSTER_CUTOUTS", "1")]).cluster_cutouts);
        assert!(!parse(&[("SKATE_CLUSTER_CUTOUTS", "0")]).cluster_cutouts);
        assert!(!parse(&[]).cluster_kernel);
        assert!(parse(&[("SKATE_CLUSTER_KERNEL", "1")]).cluster_kernel);
        assert!(!parse(&[("SKATE_CLUSTER_KERNEL", "0")]).cluster_kernel);
        assert!(!parse(&[("SKATE_PRESENT_DIRECT", "0")]).present_direct);
        assert!(parse(&[]).present_direct);
        assert!(!parse(&[("SKATE_TONE_DIRECT", "0")]).tone_direct);
        assert!(parse(&[]).tone_direct);
        assert!(parse(&[]).tone_gamma);
        assert!(!parse(&[("SKATE_TONE_GAMMA", "0")]).tone_gamma);
        assert!(parse(&[("SKATE_OCCLUSION", "1")]).occlusion);
        assert!(!parse(&[]).occlusion);
        assert_eq!(parse(&[]).occlusion_span, 0);
        assert_eq!(parse(&[("SKATE_OCCLUSION_SPAN", "8")]).occlusion_span, 8);
        assert_eq!(parse(&[("SKATE_OCCLUSION_SPAN", "99")]).occlusion_span, 32);
        assert_eq!(parse(&[("SKATE_OCCLUSION_SPAN", "-1")]).occlusion_span, 0);
        assert!(parse(&[]).occlusion_batch);
        assert!(!parse(&[("SKATE_OCCLUSION_BATCH", "0")]).occlusion_batch);
        assert!(!parse(&[]).pyramid_once);
        assert!(parse(&[("SKATE_PYRAMID_ONCE", "1")]).pyramid_once);
        assert!(!parse(&[]).pyramid_tile);
        assert!(parse(&[("SKATE_PYRAMID_TILE", "1")]).pyramid_tile);
        assert!(!parse(&[("SKATE_PYRAMID_TILE", "0")]).pyramid_tile);
        assert!(!parse(&[]).pyramid_block);
        assert!(parse(&[("SKATE_PYRAMID_BLOCK", "1")]).pyramid_block);
        assert!(!parse(&[("SKATE_PYRAMID_BLOCK", "0")]).pyramid_block);
        assert!(parse(&[]).late_preprocess_direct);
        assert!(!parse(&[("SKATE_LATE_PREPROCESS_DIRECT", "0")]).late_preprocess_direct);
        assert!(parse(&[("SKATE_LATE_PREPROCESS_DIRECT", "1")]).late_preprocess_direct);
        assert!(parse(&[]).indirect_phase_filter);
        assert!(!parse(&[("SKATE_INDIRECT_PHASE_FILTER", "0")]).indirect_phase_filter);
        assert!(parse(&[("SKATE_INDIRECT_PHASE_FILTER", "1")]).indirect_phase_filter);
        assert_eq!(parse(&[]).triangle_cull, 0);
        assert_eq!(parse(&[("SKATE_TRIANGLE_CULL", "8")]).triangle_cull, 8);
        assert_eq!(parse(&[("SKATE_TRIANGLE_CULL", "99")]).triangle_cull, 32);
        assert_eq!(parse(&[("SKATE_TRIANGLE_CULL", "0")]).triangle_cull, 0);
        assert_eq!(parse(&[("SKATE_TRIANGLE_CULL", "-1")]).triangle_cull, 0);
        assert_eq!(
            parse(&[("SKATE_TRIANGLE_CULL", " nodepth ")]).triangle_cull,
            bevy::pbr::TRIANGLE_CULL_NO_DEPTH
        );
        assert_eq!(
            parse(&[("SKATE_TRIANGLE_CULL", "8"), ("SKATE_TRIANGLE_CULL_DEBUG", "6")])
                .triangle_cull,
            8 | bevy::pbr::TRIANGLE_CULL_DEBUG_EMPTY | bevy::pbr::TRIANGLE_CULL_DEBUG_NO_SWAP
        );
        assert_eq!(
            parse(&[("SKATE_TRIANGLE_CULL", "8"), ("SKATE_TRIANGLE_CULL_DEBUG", "1")])
                .triangle_cull,
            8 | bevy::pbr::TRIANGLE_CULL_DEBUG_MIRRORS_ONLY
        );
        assert_eq!(
            parse(&[("SKATE_TRIANGLE_CULL", "8"), ("SKATE_TRIANGLE_CULL_DEBUG", "49")])
                .triangle_cull,
            8 | bevy::pbr::TRIANGLE_CULL_DEBUG_MIRRORS_ONLY
                | bevy::pbr::TRIANGLE_CULL_DEBUG_CLEAR
                | bevy::pbr::TRIANGLE_CULL_DEBUG_STATS_ONLY
        );
        assert_eq!(
            parse(&[("SKATE_TRIANGLE_CULL", "8"), ("SKATE_TRIANGLE_CULL_DEBUG", "8")])
                .triangle_cull,
            8 | bevy::pbr::TRIANGLE_CULL_DEBUG_NO_STATS
        );
        // The debug bits never turn the pass on.
        assert_eq!(parse(&[("SKATE_TRIANGLE_CULL_DEBUG", "7")]).triangle_cull, 0);
        assert_eq!(
            parse(&[("SKATE_TRIANGLE_CULL", "8"), ("SKATE_TRIANGLE_CULL_V1", "1")]).triangle_cull,
            8 | bevy::pbr::TRIANGLE_CULL_V1
        );
        assert_eq!(parse(&[("SKATE_TRIANGLE_CULL_V1", "1")]).triangle_cull, 0);
    }

    #[test]
    fn a_flag_set_to_neither_keeps_its_default() {
        let default = RenderOptions::default().depth_prepass;
        assert_eq!(parse(&[("SKATE_DEPTH_PREPASS", "")]).depth_prepass, default);
        assert_eq!(parse(&[("SKATE_DEPTH_PREPASS", "maybe")]).depth_prepass, default);
        assert!(parse(&[("SKATE_DEPTH_PREPASS", "on")]).depth_prepass);
        assert!(!parse(&[("SKATE_DEPTH_PREPASS", "off")]).depth_prepass);
        assert!(parse(&[("SKATE_WORLD_SHADOWS", "maybe")]).world_shadows);
    }

    #[test]
    fn world_variants_are_parsed() {
        assert_eq!(parse(&[]).world, WorldVariant::default());
        let world = parse(&[
            ("SKATE_WORLD_TRIM", "0"),
            ("SKATE_WORLD_PACKED", "off"),
            ("SKATE_WORLD_LOD", "1"),
            ("SKATE_WORLD_PROBE", "2"),
            ("SKATE_PREPASS_PROBE", "1"),
        ])
        .world;
        assert_eq!(
            world,
            WorldVariant {
                trim: false,
                packed: false,
                explicit_lod: true,
                probe: 2,
                prepass_probe: 1,
                fetch: false,
                page_uniform: false,
                features: false,
                slot_waterfall: false,
                gpr48: false,
                cutout_flat: true,
                vis_cull: 0,
                mark_after_test: false,
                short_lives: false,
                lean: false,
                implicit_lod: false,
            }
        );
        assert_eq!(parse(&[("SKATE_WORLD_PROBE", "3")]).world.probe, 3);
        assert_eq!(parse(&[("SKATE_WORLD_PROBE", "4")]).world.probe, 4);
        assert_eq!(parse(&[("SKATE_WORLD_PROBE", "5")]).world.probe, 5);
        assert_eq!(parse(&[("SKATE_WORLD_PROBE", "6")]).world.probe, 6);
        assert_eq!(parse(&[("SKATE_WORLD_PROBE", "7")]).world.probe, 0);
        assert_eq!(parse(&[("SKATE_WORLD_PROBE", "x")]).world.probe, 0);
        assert_eq!(parse(&[("SKATE_PREPASS_PROBE", "4")]).world.prepass_probe, 4);
        assert_eq!(parse(&[("SKATE_PREPASS_PROBE", "5")]).world.prepass_probe, 5);
        assert_eq!(parse(&[("SKATE_PREPASS_PROBE", "6")]).world.prepass_probe, 6);
        assert_eq!(parse(&[("SKATE_PREPASS_PROBE", "7")]).world.prepass_probe, 0);
        assert_eq!(parse(&[("SKATE_PREPASS_PROBE", "x")]).world.prepass_probe, 0);
        assert_eq!(parse(&[("SKATE_PREPASS_PROBE", "3")]).world.probe, 0);
        assert!(parse(&[("SKATE_WORLD_FETCH", "1")]).world.fetch);
        assert!(!parse(&[]).world.page_uniform);
        assert!(parse(&[("SKATE_WORLD_PAGE_UNIFORM", "1")]).world.page_uniform);
        assert!(!parse(&[]).world.features);
        assert!(parse(&[("SKATE_WORLD_FEATURES", "1")]).world.features);
        assert!(!parse(&[("SKATE_WORLD_FEATURES", "0")]).world.features);
        assert!(!parse(&[]).world.slot_waterfall);
        assert!(parse(&[("SKATE_WORLD_SLOT_WATERFALL", "1")]).world.slot_waterfall);
        assert!(!parse(&[("SKATE_WORLD_SLOT_WATERFALL", "0")]).world.slot_waterfall);
        assert!(!parse(&[]).world.gpr48);
        assert!(parse(&[("SKATE_WORLD_GPR48", "1")]).world.gpr48);
        assert!(!parse(&[]).world.short_lives);
        assert!(parse(&[("SKATE_WORLD_SHORT_LIVES", "1")]).world.short_lives);
        assert!(!parse(&[("SKATE_WORLD_SHORT_LIVES", "0")]).world.short_lives);
        assert!(!parse(&[]).world.lean);
        assert!(parse(&[("SKATE_WORLD_LEAN", "1")]).world.lean);
        assert!(!parse(&[("SKATE_WORLD_LEAN", "0")]).world.lean);
        assert!(!parse(&[]).world.implicit_lod);
        assert!(parse(&[("SKATE_WORLD_IMPLICIT", "1")]).world.implicit_lod);
        assert!(!parse(&[("SKATE_WORLD_IMPLICIT", "0")]).world.implicit_lod);
        assert!(parse(&[]).world.cutout_flat);
        assert!(!parse(&[("SKATE_CUTOUT_FLAT", "0")]).world.cutout_flat);
        assert!(parse(&[("SKATE_CUTOUT_FLAT", "1")]).world.cutout_flat);
        assert_eq!(parse(&[]).world.vis_cull, 0);
        assert_eq!(parse(&[("SKATE_VIS_CULL", "1")]).world.vis_cull, 1);
        assert_eq!(parse(&[("SKATE_VIS_CULL", " 2 ")]).world.vis_cull, 2);
        assert_eq!(parse(&[("SKATE_VIS_CULL", "3")]).world.vis_cull, 1);
        assert_eq!(parse(&[("SKATE_VIS_CULL", "4")]).world.vis_cull, 0);
        assert_eq!(parse(&[("SKATE_VIS_CULL", "x")]).world.vis_cull, 0);
    }

    #[test]
    fn marking_after_the_alpha_test_needs_the_cutouts_to_mark() {
        let after = |cull: &str, value: &str| {
            parse(&[("SKATE_VIS_CULL", cull), ("SKATE_VIS_CULL_AFTER_TEST", value)])
                .world
                .mark_after_test
        };
        assert!(after("1", "1"));
        assert!(after("3", " 1 "));
        assert!(!after("1", "0"));
        assert!(!after("2", "1"));
        assert!(!after("0", "1"));
        assert!(!parse(&[("SKATE_VIS_CULL", "1")]).world.mark_after_test);
        assert!(!parse(&[("SKATE_VIS_CULL_AFTER_TEST", "1")]).world.mark_after_test);
    }

    #[test]
    fn the_marks_take_the_place_of_the_cull_tests() {
        use bevy::pbr::{
            TRIANGLE_CULL_MARKS, TRIANGLE_CULL_MARKS_BALLOT, TRIANGLE_CULL_MARKS_INVERT,
            TRIANGLE_CULL_MARKS_OPAQUE_ONLY,
        };
        assert_eq!(
            parse(&[("SKATE_VIS_CULL", "1")]).triangle_cull,
            TRIANGLE_CULL_MARKS | TRIANGLE_CULL_MARKS_BALLOT
        );
        assert_eq!(
            parse(&[("SKATE_VIS_CULL", "2")]).triangle_cull,
            TRIANGLE_CULL_MARKS | TRIANGLE_CULL_MARKS_OPAQUE_ONLY | TRIANGLE_CULL_MARKS_BALLOT
        );
        assert_eq!(
            parse(&[("SKATE_VIS_CULL", "3")]).triangle_cull,
            TRIANGLE_CULL_MARKS | TRIANGLE_CULL_MARKS_INVERT | TRIANGLE_CULL_MARKS_BALLOT
        );
        assert_eq!(
            parse(&[
                ("SKATE_TRIANGLE_CULL", "8"),
                ("SKATE_TRIANGLE_CULL_V1", "1"),
                ("SKATE_VIS_CULL", "1"),
            ])
            .triangle_cull,
            TRIANGLE_CULL_MARKS | TRIANGLE_CULL_MARKS_BALLOT
        );
        assert_eq!(
            parse(&[("SKATE_TRIANGLE_CULL", "8"), ("SKATE_VIS_CULL", "0")]).triangle_cull,
            8
        );
        assert_eq!(
            parse(&[("SKATE_VIS_CULL", "1"), ("SKATE_VIS_CULL_KERNEL", " 0 ")]).triangle_cull,
            TRIANGLE_CULL_MARKS | bevy::pbr::TRIANGLE_CULL_MARKS_IN_CULL
        );
        assert_eq!(
            parse(&[("SKATE_VIS_CULL", "2"), ("SKATE_VIS_CULL_KERNEL", "1")]).triangle_cull,
            TRIANGLE_CULL_MARKS | TRIANGLE_CULL_MARKS_OPAQUE_ONLY
        );
        assert_eq!(
            parse(&[("SKATE_VIS_CULL", "1"), ("SKATE_VIS_CULL_KERNEL", " 1 ")]).triangle_cull,
            TRIANGLE_CULL_MARKS
        );
        assert_eq!(
            parse(&[("SKATE_VIS_CULL", "2"), ("SKATE_VIS_CULL_KERNEL", "2")]).triangle_cull,
            TRIANGLE_CULL_MARKS
                | TRIANGLE_CULL_MARKS_OPAQUE_ONLY
                | bevy::pbr::TRIANGLE_CULL_MARKS_COALESCED
        );
        assert_eq!(
            parse(&[("SKATE_VIS_CULL", "1"), ("SKATE_VIS_CULL_KERNEL", "3")]).triangle_cull,
            TRIANGLE_CULL_MARKS | bevy::pbr::TRIANGLE_CULL_MARKS_BALLOT
        );
        assert_eq!(
            parse(&[("SKATE_VIS_CULL", "2"), ("SKATE_VIS_CULL_KERNEL", "4")]).triangle_cull,
            TRIANGLE_CULL_MARKS
                | TRIANGLE_CULL_MARKS_OPAQUE_ONLY
                | bevy::pbr::TRIANGLE_CULL_MARKS_BALLOT
                | bevy::pbr::TRIANGLE_CULL_MARKS_BALLOT_8
        );
        assert_eq!(
            parse(&[("SKATE_TRIANGLE_CULL", "8"), ("SKATE_VIS_CULL_KERNEL", "3")]).triangle_cull,
            8
        );
        assert_eq!(
            parse(&[("SKATE_TRIANGLE_CULL", "8"), ("SKATE_VIS_CULL_KERNEL", "2")]).triangle_cull,
            8
        );
        assert_eq!(
            parse(&[("SKATE_TRIANGLE_CULL", "8"), ("SKATE_VIS_CULL_KERNEL", "0")]).triangle_cull,
            8
        );
    }

    #[test]
    fn marks_follow_the_launch_and_the_matrix() {
        let wanted = |pairs: &[(&str, &str)]| {
            marks_wanted(|name| {
                pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| value.to_string())
            })
        };
        assert!(!wanted(&[]));
        assert!(wanted(&[("SKATE_VIS_CULL", "1")]));
        assert!(wanted(&[("SKATE_VIS_CULL", "2")]));
        assert!(wanted(&[("SKATE_VIS_CULL", "3")]));
        assert!(!wanted(&[("SKATE_VIS_CULL", "0")]));
        assert!(wanted(&[("SKATE_PERF_MATRIX", "base;vcull=1;base")]));
        assert!(wanted(&[("SKATE_PERF_MATRIX", "base; probe=2, vcull = 2")]));
        assert!(wanted(&[("SKATE_PERF_MATRIX", "base;vcull=3,probe=2")]));
        assert!(!wanted(&[("SKATE_PERF_MATRIX", "base;vcull=0;probe=1")]));
    }

    #[test]
    fn clusters_follow_the_launch_and_the_matrix() {
        let wanted = |pairs: &[(&str, &str)]| {
            clusters_wanted(|name| {
                pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| value.to_string())
            })
        };
        assert!(!wanted(&[]));
        assert!(wanted(&[("SKATE_CLUSTER_CULL", "8")]));
        assert!(wanted(&[("SKATE_CLUSTER_CULL", " 1 ")]));
        assert!(!wanted(&[("SKATE_CLUSTER_CULL", "0")]));
        assert!(!wanted(&[("SKATE_CLUSTER_CULL", "on")]));
        assert!(wanted(&[("SKATE_PERF_MATRIX", "base;vcull=3,ccull=8;base")]));
        assert!(wanted(&[("SKATE_PERF_MATRIX", "base; probe=2, ccull = 4")]));
        assert!(!wanted(&[("SKATE_PERF_MATRIX", "base;ccull=0;vcull=1")]));
    }

    #[test]
    fn fetch_geometry_follows_the_launch_and_the_matrix() {
        let wanted = |pairs: &[(&str, &str)]| {
            fetch_geometry_wanted(|name| {
                pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| value.to_string())
            })
        };
        assert!(!wanted(&[]));
        assert!(wanted(&[("SKATE_WORLD_FETCH", "1")]));
        assert!(!wanted(&[("SKATE_WORLD_FETCH", "0")]));
        assert!(wanted(&[("SKATE_PERF_MATRIX", "base;fetch=1,probe=1")]));
        assert!(wanted(&[("SKATE_PERF_MATRIX", "base; probe=2, fetch = on")]));
        assert!(!wanted(&[("SKATE_PERF_MATRIX", "base;fetch=0;probe=1")]));
    }

    #[test]
    fn draw_distance_measures_to_the_nearest_point() {
        // 400 m across, its centre 200 m away, its near edge under the camera.
        let ground = Aabb::from_min_max(Vec3::new(0., -1., -200.), Vec3::new(400., 0., 200.));
        assert!(!beyond(&ground, Vec3::new(1., 2., 0.), 50.));
        assert!(beyond(&ground, Vec3::new(-100., 2., 0.), 50.));
    }
}

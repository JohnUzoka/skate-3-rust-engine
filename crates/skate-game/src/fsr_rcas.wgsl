// FSR 1's robust contrast-adaptive sharpening (RCAS) on a bilinear stretch, for the presentation
// pass (graphics_menu.rs `Sharpener`), ported from AMD FidelityFX Super Resolution 1.0's
// ffx_fsr1.h (`FsrRcasF`). Each window pixel takes the scene image's bilinear sample under it and
// the four one window pixel away, and sharpens that cross by the most negative lobe that keeps
// every channel inside 0-1, limited and scaled by the sharpness. Five filtered reads and a few
// dozen operations a pixel, where EASU (fsr_easu.wgsl) takes twelve reads and its edge analysis.
// The scene image is read as linear, which FSR's limiter takes as well as perceptual values.
//
// Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy of this software
// and associated documentation files (the "Software"), to deal in the Software without
// restriction, including without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all copies or
// substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
// BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
// NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
// DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
#import bevy_ui::ui_vertex_output::UiVertexOutput

@group(1) @binding(0) var scene: texture_2d<f32>;
@group(1) @binding(1) var scene_sampler: sampler;

// The sharpness in stops, as `FsrRcasCon` takes it: 0 is the strongest and each stop halves it.
// AMD's samples use 0.2.
const SHARPNESS: f32 = 0.2;
// The most negative lobe before the results look unnatural (FSR_RCAS_LIMIT).
const LIMIT: f32 = 0.25 - 1.0 / 16.0;

fn tap(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(scene, scene_sampler, uv, 0.0).rgb;
}

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    // One window pixel, in the scene image's coordinates.
    let px = vec2<f32>(dpdx(in.uv).x, dpdy(in.uv).y);
    //   b
    // d e f
    //   h
    let b = tap(in.uv - vec2<f32>(0.0, px.y));
    let d = tap(in.uv - vec2<f32>(px.x, 0.0));
    let e = tap(in.uv);
    let f = tap(in.uv + vec2<f32>(px.x, 0.0));
    let h = tap(in.uv + vec2<f32>(0.0, px.y));
    let mn = min(min(b, d), min(f, h));
    let mx = max(max(b, d), max(f, h));
    // The lobe at which each channel would leave 0-1; the guards keep a black or a white ring
    // from dividing by zero.
    let hit_min = min(mn, e) / max(4.0 * mx, vec3<f32>(1.0 / 65536.0));
    let hit_max = (1.0 - max(mx, e)) / min(4.0 * mn - 4.0, vec3<f32>(-1.0 / 65536.0));
    let lobe_rgb = max(-hit_min, hit_max);
    let lobe = max(-LIMIT, min(max(max(lobe_rgb.r, lobe_rgb.g), lobe_rgb.b), 0.0)) * exp2(-SHARPNESS);
    return vec4<f32>((lobe * (b + d + f + h) + e) / (4.0 * lobe + 1.0), 1.0);
}

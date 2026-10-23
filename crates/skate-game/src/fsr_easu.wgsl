// FSR 1's edge-adaptive upscale (EASU) for the presentation pass (graphics_menu.rs `Upscaler`),
// ported from AMD FidelityFX Super Resolution 1.0's ffx_fsr1.h (`FsrEasuF`). Each output pixel
// takes the 12 scene pixels around it, a Lanczos-2 window turned along the local edge and
// stretched across it, clamped to the 4 nearest pixels' range so it doesn't ring. Taps are
// filtered as their square roots (FSR wants a perceptual space, and the scene image is read as
// linear) and the result squared back.
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

fn tap(p: vec2<i32>, last: vec2<i32>) -> vec3<f32> {
    return sqrt(max(textureLoad(scene, clamp(p, vec2<i32>(0), last), 0).rgb, vec3<f32>(0.0)));
}

// Luma times 2.
fn luma(c: vec3<f32>) -> f32 {
    return c.b * 0.5 + (c.r * 0.5 + c.g);
}

// One bilinear corner's share of the edge direction and length, from the '+' around its pixel c:
//   a
// b c d
//   e
fn edge(w: f32, a: f32, b: f32, c: f32, d: f32, e: f32, acc: vec3<f32>) -> vec3<f32> {
    let dir_x = d - b;
    let len_x = clamp(abs(dir_x) / max(max(abs(d - c), abs(c - b)), 1.0 / 65536.0), 0.0, 1.0);
    let dir_y = e - a;
    let len_y = clamp(abs(dir_y) / max(max(abs(e - c), abs(c - a)), 1.0 / 65536.0), 0.0, 1.0);
    return acc + vec3<f32>(dir_x * w, dir_y * w, (len_x * len_x + len_y * len_y) * w);
}

// One tap's weight: offset `off` from the sample point, turned by `dir`, scaled by `len2`, through
// the windowed Lanczos-2 approximation with negative lobe `lob`, clipped at `clp`.
fn weight(off: vec2<f32>, dir: vec2<f32>, len2: vec2<f32>, lob: f32, clp: f32) -> f32 {
    let v = vec2<f32>(off.x * dir.x + off.y * dir.y, off.y * dir.x - off.x * dir.y) * len2;
    let d2 = min(dot(v, v), clp);
    var w_b = 2.0 / 5.0 * d2 - 1.0;
    var w_a = lob * d2 - 1.0;
    w_b = w_b * w_b;
    w_a = w_a * w_a;
    w_b = 25.0 / 16.0 * w_b - (25.0 / 16.0 - 1.0);
    return w_b * w_a;
}

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(scene));
    let last = size - 1;
    // The sample point in scene pixels, and pixel f under it:
    //     b c
    //   e f g h
    //   i j k l
    //     n o
    let pos = in.uv * vec2<f32>(size) - 0.5;
    let fp = floor(pos);
    let pp = pos - fp;
    let f0 = vec2<i32>(fp);

    let b = tap(f0 + vec2<i32>(0, -1), last);
    let c = tap(f0 + vec2<i32>(1, -1), last);
    let e = tap(f0 + vec2<i32>(-1, 0), last);
    let f = tap(f0, last);
    let g = tap(f0 + vec2<i32>(1, 0), last);
    let h = tap(f0 + vec2<i32>(2, 0), last);
    let i = tap(f0 + vec2<i32>(-1, 1), last);
    let j = tap(f0 + vec2<i32>(0, 1), last);
    let k = tap(f0 + vec2<i32>(1, 1), last);
    let l = tap(f0 + vec2<i32>(2, 1), last);
    let n = tap(f0 + vec2<i32>(0, 2), last);
    let o = tap(f0 + vec2<i32>(1, 2), last);

    let b_l = luma(b);
    let c_l = luma(c);
    let e_l = luma(e);
    let f_l = luma(f);
    let g_l = luma(g);
    let h_l = luma(h);
    let i_l = luma(i);
    let j_l = luma(j);
    let k_l = luma(k);
    let l_l = luma(l);
    let n_l = luma(n);
    let o_l = luma(o);

    // The edge's direction and length, from f, g, j and k by their bilinear weights.
    var acc = vec3<f32>(0.0);
    acc = edge((1.0 - pp.x) * (1.0 - pp.y), b_l, e_l, f_l, g_l, j_l, acc);
    acc = edge(pp.x * (1.0 - pp.y), c_l, f_l, g_l, h_l, k_l, acc);
    acc = edge((1.0 - pp.x) * pp.y, f_l, i_l, j_l, k_l, n_l, acc);
    acc = edge(pp.x * pp.y, g_l, j_l, k_l, l_l, o_l, acc);
    var dir = acc.xy;
    let dir_r = dot(dir, dir);
    if dir_r < 1.0 / 32768.0 {
        dir = vec2<f32>(1.0, 0.0);
    } else {
        dir = dir * inverseSqrt(dir_r);
    }
    // From 0-2 to 0-1, shaped by its square.
    var len = acc.z * 0.5;
    len = len * len;
    // The kernel stretches from 1 along an axis to sqrt(2) on a diagonal, and shrinks across an
    // edge; its window grows from +-sqrt(2) to a little beyond +-2 with the edge.
    let stretch = dot(dir, dir) / max(abs(dir.x), abs(dir.y));
    let len2 = vec2<f32>(1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
    let lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
    let clp = 1.0 / lob;

    var color = vec3<f32>(0.0);
    var total = 0.0;
    var w = weight(vec2<f32>(0.0, -1.0) - pp, dir, len2, lob, clp); color += b * w; total += w;
    w = weight(vec2<f32>(1.0, -1.0) - pp, dir, len2, lob, clp); color += c * w; total += w;
    w = weight(vec2<f32>(-1.0, 1.0) - pp, dir, len2, lob, clp); color += i * w; total += w;
    w = weight(vec2<f32>(0.0, 1.0) - pp, dir, len2, lob, clp); color += j * w; total += w;
    w = weight(vec2<f32>(0.0, 0.0) - pp, dir, len2, lob, clp); color += f * w; total += w;
    w = weight(vec2<f32>(-1.0, 0.0) - pp, dir, len2, lob, clp); color += e * w; total += w;
    w = weight(vec2<f32>(1.0, 1.0) - pp, dir, len2, lob, clp); color += k * w; total += w;
    w = weight(vec2<f32>(2.0, 1.0) - pp, dir, len2, lob, clp); color += l * w; total += w;
    w = weight(vec2<f32>(2.0, 0.0) - pp, dir, len2, lob, clp); color += h * w; total += w;
    w = weight(vec2<f32>(1.0, 0.0) - pp, dir, len2, lob, clp); color += g * w; total += w;
    w = weight(vec2<f32>(1.0, 2.0) - pp, dir, len2, lob, clp); color += o * w; total += w;
    w = weight(vec2<f32>(0.0, 2.0) - pp, dir, len2, lob, clp); color += n * w; total += w;

    // Within the range of the 4 nearest, so the negative lobes don't ring.
    let lo = min(min(f, g), min(j, k));
    let hi = max(max(f, g), max(j, k));
    let out = clamp(color / total, lo, hi);
    return vec4<f32>(out * out, 1.0);
}

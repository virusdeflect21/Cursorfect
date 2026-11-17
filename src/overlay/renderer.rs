//! Direct3D 11 renderer presented through DirectComposition, with an HDR effect pipeline.
//!
//! Per frame:
//! 1. **World pass** (while Sandevistan is active): every monitor's live desktop image is
//!    obtained with DXGI Desktop Duplication and redrawn in graded black & white: filmic
//!    S-curve, vignette, animated film grain, chromatic fringing and a lens-warp shimmer on
//!    the travelling wave front. The overlay window is excluded from capture
//!    (`WDA_EXCLUDEFROMCAPTURE`), so it never re-captures itself. The real hardware cursor is
//!    composited by Windows above everything and therefore stays in full colour.
//! 2. **Effect pass** into a 16-bit float (HDR) target: afterimages (copies of the real
//!    cursor bitmap with neon halos), trails, sparks, rings and light rays. Colours may go
//!    far above 1.0 ("white-hot" cores).
//! 3. **Bloom**: a 6-level mip chain (13-tap downsample with soft threshold and firefly
//!    suppression, 9-tap tent upsample) gives every bright element a physically wide glow.
//! 4. **Composite**: HDR + bloom are tonemapped (soft shoulder, highlights roll off to white)
//!    and dithered (no 8-bit banding in smooth glows), then blended over the world pass.
//!
//! wgpu cannot present a *composition* swap chain with per-pixel alpha on Windows in a
//! supported way, so this focused D3D11 + DirectComposition path is used instead.
//!
//! # Safety notes
//! All COM objects are reference-counted `windows` interface wrappers, released automatically
//! on drop. Every raw pointer handed to D3D (descs, shader bytes, mapped buffers) is derived
//! from live Rust data for the duration of the call. Duplicated frames are always released
//! (`ReleaseFrame`) before the next acquire.

use anyhow::{Context, anyhow};
use glam::Vec2;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, HWND};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_APPEND_ALIGNED_ELEMENT, D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_VERTEX_BUFFER, D3D11_BLEND_DESC,
    D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BUFFER_DESC,
    D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_COMPARISON_NEVER, D3D11_CPU_ACCESS_READ,
    D3D11_CPU_ACCESS_WRITE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
    D3D11_INPUT_ELEMENT_DESC, D3D11_INPUT_PER_VERTEX_DATA, D3D11_MAP_READ, D3D11_MAP_WRITE_DISCARD,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SAMPLER_DESC, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE_ADDRESS_BORDER, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC, D3D11_USAGE_IMMUTABLE, D3D11_USAGE_STAGING,
    D3D11_VIEWPORT, D3D11CreateDevice, ID3D11BlendState, ID3D11Buffer, ID3D11Device,
    ID3D11DeviceContext, ID3D11InputLayout, ID3D11PixelShader, ID3D11RenderTargetView,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R32G32_FLOAT,
    DXGI_FORMAT_R32G32B32A32_FLOAT, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, IDXGIAdapter, IDXGIDevice, IDXGIDevice1,
    IDXGIFactory2, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource, IDXGISwapChain1,
    IDXGISwapChain2,
};
use windows::core::{Interface, PCSTR, s};

use crate::effects::{Batch, Look, MODE_DESKTOP, MODE_SOLID, Vertex, World};
use crate::platform::native::CursorImage;

const SHADER: &str = r#"
cbuffer Frame : register(b0) {
    float4 g0;   // x = time (s), y = width, z = height, w = film grade on/off
    float4 g1;   // x = bloom strength, y = exposure, z = wave-front strength, w = grain
    float4 g2;   // bloom passes: xy = source texel size, z = threshold / radius, w = first pass
                 // world pass:  x = glitch amount (0..1), y = ripple radius (px),
                 //              z = ripple amplitude (0..1), w = lens pinch (0..1);
                 //              ripple centre in g3.xy
    float4 g3;   // xy = ripple centre offset from the wave centre (px), z = draw alpha,
                 // w = look (0 Sandevistan, 1 The World)
    float4 g4;   // The World: x = inversion sphere radius (px), y = flash, z = tint, w = chroma
    float4 g5;   // primary time-stop colour
    float4 g6;   // secondary time-stop colour
};

Texture2D cursorTex : register(t0);
Texture2D deskTex   : register(t1);
Texture2D srcTex    : register(t2);
Texture2D bloomTex  : register(t3);
SamplerState samp   : register(s0);   // linear, clamp
SamplerState sampB  : register(s1);   // linear, transparent border

struct VSIn  { float2 pos : POSITION; float2 uv : TEXCOORD0; float4 col : COLOR0; float4 prm : TEXCOORD1; };
struct PSIn  { float4 pos : SV_Position; float2 uv : TEXCOORD0; float4 col : COLOR0; float4 prm : TEXCOORD1; };
struct FSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };

float hash12(float2 p) {
    float3 p3 = frac(float3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return frac((p3.x + p3.y) * p3.z);
}

float luma(float3 c) { return dot(c, float3(0.2126, 0.7152, 0.0722)); }

PSIn vs_main(VSIn i) {
    PSIn o;
    o.pos = float4(i.pos, 0.0, 1.0);
    o.uv = i.uv;
    o.col = i.col;
    o.prm = i.prm;
    return o;
}

// Full-screen triangle from SV_VertexID (no vertex buffer data needed).
FSOut vs_full(uint id : SV_VertexID) {
    FSOut o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.uv = uv;
    o.pos = float4(uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    return o;
}

float4 world_pass(PSIn i) {
    // col.xy = pixel offset from the wave centre, col.z = contrast, col.w = 1: colour floods
    // back (inverted wave). prm.y = radius, prm.z = feather, prm.w = amount.
    float film = g0.w;
    float2 off = i.col.xy;
    float d = length(off);
    float2 dir = d > 0.5 ? off / d : float2(0.0, 0.0);
    float2 px = float2(abs(ddx(i.uv.x)), abs(ddy(i.uv.y)));   // uv per pixel
    float rel = i.prm.y - d;
    float band = exp(-(rel * rel) / (2.0 * 130.0 * 130.0)) * g1.z;
    // The wave front refracts the desktop in rippling air waves (several crests).
    float2 uv = i.uv - dir * px * band * sin(rel * 0.075) * 30.0;
    // Lens pinch: space is sucked towards the centre (strongest close to it).
    uv -= off * px * g2.w * (0.035 + 0.09 * exp(-d / 420.0));
    float2 q = i.uv - 0.5;
    float edge = dot(q, q);
    // Chromatic fringing on the wave front and towards the screen edges.
    float2 o = (dir * band * 9.0 + q * 16.0 * edge * (1.0 - g3.w)) * px * film;
    // Air ripple: concentric pressure waves expanding from the activation point refract the
    // desktop (and shimmer its brightness slightly), like a heat / shock wave in the air.
    float shimmer = 0.0;
    if (g2.z > 0.001) {
        float2 roff = off - g3.xy;
        float rd = length(roff);
        float2 rdir = rd > 0.5 ? roff / rd : float2(0.0, 0.0);
        float wv;
        if (g2.y < 0.0) {
            // Continuous train of waves flowing inwards (time giving way to the stand).
            float env = smoothstep(40.0, 320.0, rd) * (1.0 - smoothstep(800.0, 1700.0, rd));
            wv = sin(rd * 0.038 + g0.x * 10.0) * env;
        } else {
            float x = rd - g2.y;
            float packet = exp(-(x * x) / (2.0 * 130.0 * 130.0)) * smoothstep(240.0, 60.0, x);
            wv = sin(x * 0.055) * packet;
        }
        uv -= rdir * px * wv * g2.z * 14.0;
        shimmer = wv * g2.z;
    }
    // Digital glitch: horizontal slices tear sideways (re-rolled 24x per second) with a hard
    // RGB split, on activation / dash / deactivation.
    float gl = g2.x;
    float torn = 0.0;
    if (gl > 0.001) {
        float frameSeed = floor(g0.x * 24.0);
        float fine = hash12(float2(floor(i.pos.y / 14.0), frameSeed));
        float coarse = hash12(float2(floor(i.pos.y / 70.0) + 31.0, frameSeed));
        torn = max(step(1.0 - gl * 0.30, fine), step(1.0 - gl * 0.55, coarse));
        float shift = (hash12(float2(floor(i.pos.y / 14.0), frameSeed + 17.0)) - 0.5) * 2.0;
        uv.x += shift * gl * 70.0 * torn * px.x;
        o += float2(gl * (3.0 + 9.0 * torn), 0.0) * px;
    }
    float inv = g4.x;
    if (g3.w > 0.5 && inv > 0.5) {
        // Chromatic split on the rim of the inversion sphere.
        float rim = exp(-((d - inv) * (d - inv)) / (2.0 * 26.0 * 26.0));
        o += dir * px * rim * 11.0 * g4.w;
    }
    float3 c  = deskTex.Sample(samp, uv).rgb;
    float3 cr = deskTex.Sample(samp, uv + o).rgb;
    float3 cb = deskTex.Sample(samp, uv - o).rgb;
    float wave = saturate((i.prm.y - d) / max(i.prm.z, 1.0));
    if (i.col.w > 0.5) wave = 1.0 - wave;
    float k = wave * i.prm.w;
    float n = hash12(i.pos.xy + frac(g0.x * 7.13) * 1000.0) - 0.5;
    if (g3.w > 0.5) {
        // ---- The World: frozen time. Mostly desaturated, split-toned violet shadows / gold
        // highlights, a colour-inverting sphere, golden flash.
        float3 col = float3(cr.r, c.g, cb.b) + shimmer * 0.015;
        float l = luma(col);
        float3 shadowTone = g6.rgb * 0.36;
        float3 highTone = lerp(g5.rgb, float3(1.0, 1.0, 1.0), 0.18);
        float3 tone = lerp(shadowTone, highTone, smoothstep(0.03, 0.95, l)) * (0.30 + 0.85 * l);
        float3 desat = lerp(col, float3(l, l, l), 0.9);
        float3 graded = lerp(desat, tone, 0.75 * g4.z);
        graded = saturate((graded - 0.5) * 1.15 + 0.5);
        float vigT = 1.0 - 0.40 * smoothstep(0.2, 0.85, length(q * float2(1.1, 1.0)));
        graded *= vigT;
        float3 outc = lerp(col, graded, k);
        if (inv > 0.5) {
            float inside = saturate((inv - d) / 5.0);
            float3 neg = 1.0 - outc;
            // Negative with a cold cast, like the show's inverted frames.
            neg = lerp(neg, neg * float3(0.78, 0.92, 1.12), 0.6);
            outc = lerp(outc, neg, inside);
        }
        outc = lerp(outc, lerp(g5.rgb, float3(1.0, 1.0, 1.0), 0.2), saturate(g4.y));
        outc += n * g1.w * film * 0.045;
        float a = g3.z;
        return float4(saturate(outc) * a, a);
    }
    float3 l3 = float3(luma(cr), luma(c), luma(cb));
    l3 = saturate((l3 - 0.5) * i.col.z + 0.5);
    // Filmic S-curve: deep blacks, bright paper whites.
    l3 = lerp(l3, l3 * l3 * (3.0 - 2.0 * l3), 0.5);
    float vig = 1.0 - 0.45 * smoothstep(0.18, 0.8, length(q * float2(1.1, 1.0)));
    float3 mono = l3 * lerp(1.0, vig, k) * float3(0.982, 1.0, 1.035) * (1.0 + torn * 0.18)
                + shimmer * 0.015;
    // Animated grain, strongest in the mid-tones like real film.
    float mid = 1.0 - abs(l3.g * 2.0 - 1.0);
    mono += n * g1.w * film * (0.35 + 0.65 * mid) * 0.11;
    // The colour side of the front shimmers too (sampled at the warped uv).
    float3 colour = c + shimmer * 0.015;
    float a = g3.z;
    return float4(saturate(lerp(colour, mono, k)) * a, a);
}

float4 ps_main(PSIn i) : SV_Target {
    float m = i.prm.x;
    if (m > 3.5 && m < 4.5) {
        return world_pass(i);
    }
    float a = 1.0;
    float3 rgb = i.col.rgb;
    if (m < 0.5) {            // soft disc / glow: gaussian falloff, prm.y = sharpness
        float d = length(i.uv);
        float sharp = i.prm.y > 0.0 ? i.prm.y : 4.0;
        a = exp(-d * d * sharp) * (1.0 - smoothstep(0.78, 1.0, d));
    } else if (m < 1.5) {     // ring: centre at prm.y, half width prm.z (uv units)
        float d = length(i.uv);
        float x = (d - i.prm.y) / i.prm.z;
        a = exp(-x * x * 2.4) * (1.0 - smoothstep(0.94, 1.0, d));
    } else if (m < 2.5) {     // solid
        a = 1.0;
    } else if (m < 3.5) {     // ribbon: gaussian across u
        float u = abs(i.uv.x);
        a = exp(-u * u * 3.2) * (1.0 - smoothstep(0.82, 1.0, u));
    } else if (m < 5.5) {     // cursor sprite tinted with the echo colour
        float4 t = cursorTex.Sample(sampB, i.uv);
        float lum = dot(t.rgb, float3(0.299, 0.587, 0.114));
        rgb = i.col.rgb * (0.30 + 0.85 * lum) + lum * lum * 0.25 * max(1.0, luma(i.col.rgb));
        a = t.a;
    } else {                  // neon halo around the cursor silhouette (dilated alpha)
        float2 r = i.prm.yz;
        float acc = 0.0;
        [unroll] for (int s = 0; s < 12; s++) {
            float ang = s * 0.5235988;
            float2 dd = float2(cos(ang), sin(ang));
            acc += cursorTex.Sample(sampB, i.uv + dd * r).a * 0.55;
            acc += cursorTex.Sample(sampB, i.uv + dd * r * 0.45).a;
        }
        float inner = cursorTex.Sample(sampB, i.uv).a;
        a = saturate(acc / 15.0);
        a = a * a * (3.0 - 2.0 * a) * (1.0 - inner * 0.8);
    }
    a *= i.col.a;
    // Premultiplied output; prm.w blends between "over" (0) and additive (1).
    return float4(rgb * a, a * (1.0 - i.prm.w));
}

float3 tap(float2 uv) { return srcTex.Sample(samp, uv).rgb; }

// 13-tap downsample (Jimenez 2014): wide, stable, no aliasing shimmer.
float4 ps_down(FSOut i) : SV_Target {
    float2 t = g2.xy;
    float2 uv = i.uv;
    float3 a = tap(uv + t * float2(-2, -2));
    float3 b = tap(uv + t * float2( 0, -2));
    float3 c = tap(uv + t * float2( 2, -2));
    float3 d = tap(uv + t * float2(-2,  0));
    float3 e = tap(uv);
    float3 f = tap(uv + t * float2( 2,  0));
    float3 g = tap(uv + t * float2(-2,  2));
    float3 h = tap(uv + t * float2( 0,  2));
    float3 k = tap(uv + t * float2( 2,  2));
    float3 j1 = tap(uv + t * float2(-1, -1));
    float3 j2 = tap(uv + t * float2( 1, -1));
    float3 j3 = tap(uv + t * float2(-1,  1));
    float3 j4 = tap(uv + t * float2( 1,  1));
    float3 col = e * 0.125 + (a + c + g + k) * 0.03125 + (b + d + f + h) * 0.0625
               + (j1 + j2 + j3 + j4) * 0.125;
    if (g2.w > 0.5) {
        // Soft-knee threshold, then firefly suppression (Karis average).
        float l = luma(col);
        float thr = g2.z;
        float knee = thr * 0.7 + 1e-4;
        float soft = clamp(l - thr + knee, 0.0, 2.0 * knee);
        soft = soft * soft / (4.0 * knee + 1e-5);
        col *= max(soft, l - thr) / max(l, 1e-4);
        col /= 1.0 + luma(col) * 0.5;
    }
    return float4(col, 1.0);
}

// 9-tap tent upsample, blended additively into the next larger mip.
float4 ps_up(FSOut i) : SV_Target {
    float2 t = g2.xy * g2.z;
    float2 uv = i.uv;
    float3 s = tap(uv + t * float2(-1, -1)) + tap(uv + t * float2(1, -1))
             + tap(uv + t * float2(-1,  1)) + tap(uv + t * float2(1,  1))
             + 2.0 * (tap(uv + t * float2(0, -1)) + tap(uv + t * float2(-1, 0))
                    + tap(uv + t * float2(1,  0)) + tap(uv + t * float2(0,  1)))
             + 4.0 * tap(uv);
    return float4(s / 16.0, 0.0);
}

float4 ps_composite(FSOut i) : SV_Target {
    float4 fx = srcTex.Load(int3(i.pos.xy, 0));
    float3 bloom = float3(0.0, 0.0, 0.0);
    if (g1.x > 0.0) bloom = bloomTex.Sample(samp, i.uv).rgb * g1.x;
    float3 hdr = fx.rgb * g1.y + bloom;
    float peak = max(hdr.r, max(hdr.g, hdr.b));
    float3 col = hdr;
    if (peak > 0.82) {
        // Soft shoulder: no hard clipping; very bright light turns white-hot like a real lens.
        float mapped = 0.82 + 0.18 * (1.0 - exp(-(peak - 0.82) / 0.18));
        col = hdr * (mapped / peak);
        float hot = saturate((peak - 1.0) / 3.0);
        col = lerp(col, float3(mapped, mapped, mapped), hot * 0.85);
    }
    // Dither: smooth glows without 8-bit banding.
    float dith = (hash12(i.pos.xy + frac(g0.x) * 517.0) - 0.5) / 255.0;
    col = max(col + dith * step(0.004, peak), 0.0);
    return float4(col, saturate(fx.a));
}
"#;

const VERTEX_BYTES: usize = std::mem::size_of::<Vertex>();
/// Room reserved at the start of the vertex buffer for world-pass quads (6 per monitor).
const DESK_VERTS: usize = 6 * 16;
/// Room for The World's window previews / vacated areas (6 vertices each).
const CROP_VERTS: usize = 6 * 96;
/// Where the effect batch starts in the vertex buffer.
const FX_BASE: usize = DESK_VERTS + CROP_VERTS;

/// A piece of the frozen desktop drawn somewhere else (a dragged window), or a flat fill.
#[derive(Clone, Copy, Debug)]
pub struct Crop {
    /// Source rectangle in virtual-desktop px (l, t, r, b); `None` draws `fill` instead.
    pub src: Option<[f32; 4]>,
    pub dst: [f32; 4],
    pub alpha: f32,
    /// Straight-alpha colour for fills.
    pub fill: [f32; 4],
}
const BLOOM_LEVELS: usize = 6;
const HDR_FORMAT: DXGI_FORMAT = DXGI_FORMAT_R16G16B16A16_FLOAT;

/// Per-frame look settings handed to the renderer.
#[derive(Clone, Copy, Debug)]
pub struct Grade {
    /// Seconds (animates the film grain).
    pub time: f32,
    pub bloom: bool,
    /// Bloom amount (0..~2).
    pub bloom_strength: f32,
    pub film: bool,
    /// Film grain amount (0 for compressible recordings).
    pub grain: f32,
}

impl Default for Grade {
    fn default() -> Self {
        Self {
            time: 0.0,
            bloom: true,
            bloom_strength: 0.55,
            film: true,
            grain: 1.0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FrameConsts {
    g0: [f32; 4],
    g1: [f32; 4],
    g2: [f32; 4],
    g3: [f32; 4],
    g4: [f32; 4],
    g5: [f32; 4],
    g6: [f32; 4],
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameResult {
    Ok,
    /// The GPU device was removed/reset: the whole renderer must be rebuilt.
    DeviceLost,
}

struct DeskOutput {
    /// None: a still image (demo mode) instead of a live capture.
    dup: Option<IDXGIOutputDuplication>,
    /// Monitor rectangle in virtual-desktop pixels.
    rect: [i32; 4],
    tex: Option<ID3D11Texture2D>,
    srv: Option<ID3D11ShaderResourceView>,
    size: (u32, u32),
}

/// An offscreen render target that can also be sampled.
struct Target {
    _tex: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    srv: ID3D11ShaderResourceView,
    w: u32,
    h: u32,
}

/// HDR effect target + bloom mip chain.
struct Hdr {
    fx: Target,
    mips: Vec<Target>,
}

struct Shaders {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    vs_full: ID3D11VertexShader,
    ps_down: ID3D11PixelShader,
    ps_up: ID3D11PixelShader,
    ps_composite: ID3D11PixelShader,
}

pub struct Renderer {
    // Keep composition objects alive as long as the swap chain is presented.
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    device: ID3D11Device,
    adapter: IDXGIAdapter,
    ctx: ID3D11DeviceContext,
    swapchain: IDXGISwapChain1,
    back: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    sh: Shaders,
    layout: ID3D11InputLayout,
    blend: ID3D11BlendState,
    blend_add: ID3D11BlendState,
    blend_none: ID3D11BlendState,
    sampler: ID3D11SamplerState,
    sampler_border: ID3D11SamplerState,
    cbuf: ID3D11Buffer,
    vb: ID3D11Buffer,
    vb_capacity: usize,
    width: u32,
    height: u32,
    origin: Vec2,
    hdr: Option<Hdr>,
    cursor_srv: Option<ID3D11ShaderResourceView>,
    cursor_handle: isize,
    /// Desktop Duplication outputs (created on demand while the world pass is used).
    desk: Vec<DeskOutput>,
    /// The overlay is excluded from capture, so duplicating the desktop is safe.
    capture_safe: bool,
    desk_retry: Option<std::time::Instant>,
    desk_vertices: Vec<Vertex>,
    crop_vertices: Vec<Vertex>,
    /// Per crop: (first vertex, desk output index or None for fills, alpha).
    crop_draws: Vec<(u32, Option<usize>, f32)>,
    /// The World: keep showing the captured frame instead of the live desktop.
    frozen: bool,
    /// Whether the last presented frame contained any pixels.
    pub dirty_surface: bool,
    frames: u64,
    latency_wait: Option<HANDLE>,
}

impl Drop for Renderer {
    fn drop(&mut self) {
        if let Some(h) = self.latency_wait.take() {
            // SAFETY: the waitable handle belongs to us and is closed exactly once.
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    }
}

fn create_device() -> anyhow::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let levels = [
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_0,
    ];
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut device = None;
        let mut ctx = None;
        // SAFETY: out-pointers reference live locals; the level slice outlives the call.
        let r = unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
        };
        match (r, device, ctx) {
            (Ok(()), Some(d), Some(c)) => {
                if driver == D3D_DRIVER_TYPE_WARP {
                    tracing::warn!("no hardware D3D11 device; using WARP software rasterizer");
                }
                return Ok((d, c));
            }
            (Err(e), _, _) => tracing::warn!("D3D11CreateDevice({driver:?}) failed: {e}"),
            _ => {}
        }
    }
    Err(anyhow!("could not create a Direct3D 11 device"))
}

fn compile(entry: PCSTR, target: PCSTR) -> anyhow::Result<ID3DBlob> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source slice is valid for the call; out-pointers reference live locals.
    let r = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            s!("cursorfect.hlsl"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = r {
        let msg = errors.as_ref().map(blob_text).unwrap_or_default();
        return Err(anyhow!("shader compile failed: {e} {msg}"));
    }
    code.ok_or_else(|| anyhow!("shader compiler returned no bytecode"))
}

fn blob_text(b: &ID3DBlob) -> String {
    // SAFETY: the blob owns `GetBufferSize()` readable bytes at `GetBufferPointer()`.
    unsafe {
        let s = std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize());
        String::from_utf8_lossy(s).into_owned()
    }
}

fn blob_bytes(b: &ID3DBlob) -> &[u8] {
    // SAFETY: as above; the returned slice borrows `b`, which keeps the memory alive.
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
}

fn pixel_shader(device: &ID3D11Device, entry: PCSTR) -> anyhow::Result<ID3D11PixelShader> {
    let blob = compile(entry, s!("ps_4_0"))?;
    let mut ps = None;
    // SAFETY: bytecode slice borrowed from the live blob.
    unsafe { device.CreatePixelShader(blob_bytes(&blob), None, Some(&mut ps)) }
        .context("pixel shader")?;
    ps.ok_or_else(|| anyhow!("no pixel shader"))
}

fn create_target(device: &ID3D11Device, w: u32, h: u32) -> anyhow::Result<Target> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w.max(1),
        Height: h.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: HDR_FORMAT,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    // SAFETY: desc and out-params are locals.
    unsafe {
        let mut tex = None;
        device
            .CreateTexture2D(&desc, None, Some(&mut tex))
            .context("HDR texture")?;
        let tex = tex.ok_or_else(|| anyhow!("no HDR texture"))?;
        let mut rtv = None;
        device
            .CreateRenderTargetView(&tex, None, Some(&mut rtv))
            .context("HDR RTV")?;
        let mut srv = None;
        device
            .CreateShaderResourceView(&tex, None, Some(&mut srv))
            .context("HDR SRV")?;
        Ok(Target {
            _tex: tex,
            rtv: rtv.ok_or_else(|| anyhow!("no HDR RTV"))?,
            srv: srv.ok_or_else(|| anyhow!("no HDR SRV"))?,
            w: w.max(1),
            h: h.max(1),
        })
    }
}

fn create_hdr(device: &ID3D11Device, w: u32, h: u32) -> anyhow::Result<Hdr> {
    let fx = create_target(device, w, h)?;
    let mut mips = Vec::with_capacity(BLOOM_LEVELS);
    let (mut mw, mut mh) = (w, h);
    for _ in 0..BLOOM_LEVELS {
        mw = (mw / 2).max(1);
        mh = (mh / 2).max(1);
        mips.push(create_target(device, mw, mh)?);
        if mw <= 2 || mh <= 2 {
            break;
        }
    }
    Ok(Hdr { fx, mips })
}

fn blend_state(
    device: &ID3D11Device,
    enable: bool,
    dest: windows::Win32::Graphics::Direct3D11::D3D11_BLEND,
) -> anyhow::Result<ID3D11BlendState> {
    let mut d = D3D11_BLEND_DESC::default();
    let rt = &mut d.RenderTarget[0];
    rt.BlendEnable = enable.into();
    rt.SrcBlend = D3D11_BLEND_ONE; // premultiplied
    rt.DestBlend = dest;
    rt.BlendOp = D3D11_BLEND_OP_ADD;
    rt.SrcBlendAlpha = D3D11_BLEND_ONE;
    rt.DestBlendAlpha = dest;
    rt.BlendOpAlpha = D3D11_BLEND_OP_ADD;
    rt.RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8;
    let mut out = None;
    // SAFETY: desc and out-param are locals.
    unsafe { device.CreateBlendState(&d, Some(&mut out)) }.context("blend state")?;
    out.ok_or_else(|| anyhow!("no blend state"))
}

fn sampler(device: &ID3D11Device, border: bool) -> anyhow::Result<ID3D11SamplerState> {
    let mode = if border {
        D3D11_TEXTURE_ADDRESS_BORDER
    } else {
        D3D11_TEXTURE_ADDRESS_CLAMP
    };
    let sdesc = D3D11_SAMPLER_DESC {
        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: mode,
        AddressV: mode,
        AddressW: mode,
        ComparisonFunc: D3D11_COMPARISON_NEVER,
        BorderColor: [0.0; 4],
        MaxLOD: f32::MAX,
        ..Default::default()
    };
    let mut out = None;
    // SAFETY: desc and out-param are locals.
    unsafe { device.CreateSamplerState(&sdesc, Some(&mut out)) }.context("sampler")?;
    out.ok_or_else(|| anyhow!("no sampler"))
}

impl Renderer {
    pub fn new(
        hwnd: HWND,
        origin: Vec2,
        width: u32,
        height: u32,
        capture_safe: bool,
    ) -> anyhow::Result<Self> {
        let (device, ctx) = create_device()?;
        let dxgi_device: IDXGIDevice = device.cast().context("device -> IDXGIDevice")?;
        // SAFETY: plain COM calls on live objects; all out-params are locals.
        unsafe {
            if let Ok(d1) = dxgi_device.cast::<IDXGIDevice1>() {
                let _ = d1.SetMaximumFrameLatency(1);
            }
            let adapter = dxgi_device.GetAdapter().context("GetAdapter")?;
            let factory: IDXGIFactory2 = adapter.GetParent().context("adapter -> factory")?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
            };
            let swapchain = factory
                .CreateSwapChainForComposition(&device, &desc, None)
                .context("CreateSwapChainForComposition")?;

            // DirectComposition: window target -> visual -> swap chain.
            let dcomp: IDCompositionDevice =
                DCompositionCreateDevice(&dxgi_device).context("DCompositionCreateDevice")?;
            let target = dcomp
                .CreateTargetForHwnd(hwnd, true)
                .context("CreateTargetForHwnd")?;
            let visual = dcomp.CreateVisual().context("CreateVisual")?;
            visual.SetContent(&swapchain).context("SetContent")?;
            target.SetRoot(&visual).context("SetRoot")?;
            dcomp.Commit().context("Commit")?;
            // Frame pacing: wait until the display has taken the previous frame before drawing
            // the next one, so every refresh shows exactly one new frame.
            let latency_wait = swapchain.cast::<IDXGISwapChain2>().ok().and_then(|sc2| {
                let _ = sc2.SetMaximumFrameLatency(1);
                let h = sc2.GetFrameLatencyWaitableObject();
                (!h.is_invalid()).then_some(h)
            });
            if latency_wait.is_none() {
                tracing::info!("no frame-latency waitable object; pacing with DwmFlush");
            }

            let vs_blob = compile(s!("vs_main"), s!("vs_4_0"))?;
            let mut vs = None;
            device
                .CreateVertexShader(blob_bytes(&vs_blob), None, Some(&mut vs))
                .context("vertex shader")?;
            let vsf_blob = compile(s!("vs_full"), s!("vs_4_0"))?;
            let mut vs_full = None;
            device
                .CreateVertexShader(blob_bytes(&vsf_blob), None, Some(&mut vs_full))
                .context("full-screen vertex shader")?;
            let sh = Shaders {
                vs: vs.ok_or_else(|| anyhow!("no VS"))?,
                ps: pixel_shader(&device, s!("ps_main"))?,
                vs_full: vs_full.ok_or_else(|| anyhow!("no full-screen VS"))?,
                ps_down: pixel_shader(&device, s!("ps_down"))?,
                ps_up: pixel_shader(&device, s!("ps_up"))?,
                ps_composite: pixel_shader(&device, s!("ps_composite"))?,
            };
            let elem = |name: PCSTR, index: u32, fmt, offset: u32| D3D11_INPUT_ELEMENT_DESC {
                SemanticName: name,
                SemanticIndex: index,
                Format: fmt,
                InputSlot: 0,
                AlignedByteOffset: offset,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            };
            let elems = [
                elem(s!("POSITION"), 0, DXGI_FORMAT_R32G32_FLOAT, 0),
                elem(s!("TEXCOORD"), 0, DXGI_FORMAT_R32G32_FLOAT, 8),
                elem(s!("COLOR"), 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 16),
                elem(
                    s!("TEXCOORD"),
                    1,
                    DXGI_FORMAT_R32G32B32A32_FLOAT,
                    D3D11_APPEND_ALIGNED_ELEMENT,
                ),
            ];
            let mut layout = None;
            device
                .CreateInputLayout(&elems, blob_bytes(&vs_blob), Some(&mut layout))
                .context("input layout")?;

            let blend = blend_state(&device, true, D3D11_BLEND_INV_SRC_ALPHA)?;
            let blend_add = blend_state(&device, true, D3D11_BLEND_ONE)?;
            let blend_none = blend_state(&device, false, D3D11_BLEND_INV_SRC_ALPHA)?;

            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<FrameConsts>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                StructureByteStride: 0,
            };
            let mut cbuf = None;
            device
                .CreateBuffer(&cb_desc, None, Some(&mut cbuf))
                .context("constant buffer")?;

            let vb_capacity = Batch::MAX_VERTS + FX_BASE;
            let vb_desc = D3D11_BUFFER_DESC {
                ByteWidth: (vb_capacity * VERTEX_BYTES) as u32,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
                StructureByteStride: 0,
            };
            let mut vb = None;
            device
                .CreateBuffer(&vb_desc, None, Some(&mut vb))
                .context("vertex buffer")?;

            let back: ID3D11Texture2D = swapchain.GetBuffer(0).context("GetBuffer")?;
            let mut rtv = None;
            device
                .CreateRenderTargetView(&back, None, Some(&mut rtv))
                .context("RTV")?;

            // The HDR pipeline is optional: without it effects render straight to the swap chain.
            let hdr = match create_hdr(&device, width, height) {
                Ok(h) => Some(h),
                Err(e) => {
                    tracing::warn!("HDR pipeline unavailable ({e:#}); using 8-bit rendering");
                    None
                }
            };

            Ok(Self {
                _dcomp: dcomp,
                _target: target,
                _visual: visual,
                device: device.clone(),
                adapter,
                ctx,
                swapchain,
                back,
                rtv: rtv.ok_or_else(|| anyhow!("no RTV"))?,
                sh,
                layout: layout.ok_or_else(|| anyhow!("no layout"))?,
                blend,
                blend_add,
                blend_none,
                sampler: sampler(&device, false)?,
                sampler_border: sampler(&device, true)?,
                cbuf: cbuf.ok_or_else(|| anyhow!("no constant buffer"))?,
                vb: vb.ok_or_else(|| anyhow!("no VB"))?,
                vb_capacity,
                width,
                height,
                origin,
                hdr,
                cursor_srv: None,
                cursor_handle: 0,
                desk: Vec::new(),
                capture_safe,
                desk_retry: None,
                desk_vertices: Vec::with_capacity(DESK_VERTS),
                crop_vertices: Vec::with_capacity(CROP_VERTS),
                crop_draws: Vec::new(),
                frozen: false,
                dirty_surface: false,
                frames: 0,
                latency_wait,
            })
        }
    }

    pub fn cursor_handle(&self) -> isize {
        self.cursor_handle
    }

    pub fn has_cursor_texture(&self) -> bool {
        self.cursor_srv.is_some()
    }

    pub fn hdr_available(&self) -> bool {
        self.hdr.is_some()
    }

    /// Upload the real cursor bitmap used for the afterimages.
    pub fn set_cursor(&mut self, img: &CursorImage) {
        self.cursor_handle = img.handle;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: img.width,
            Height: img.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: img.rgba.as_ptr().cast(),
            SysMemPitch: img.width * 4,
            SysMemSlicePitch: 0,
        };
        // SAFETY: `init` points at `img.rgba`, which outlives the call and holds width*height*4 bytes.
        unsafe {
            let mut tex = None;
            if self
                .device
                .CreateTexture2D(&desc, Some(&init), Some(&mut tex))
                .is_err()
            {
                return;
            }
            let Some(tex) = tex else { return };
            let mut srv = None;
            if self
                .device
                .CreateShaderResourceView(&tex, None, Some(&mut srv))
                .is_ok()
            {
                self.cursor_srv = srv;
            }
        }
    }

    /// Can the black & white world pass be shown?
    pub fn world_supported(&self) -> bool {
        self.capture_safe
    }

    /// The World: grab a fresh image of every monitor and keep showing it (frozen) until
    /// [`Renderer::unfreeze`]. Returns false if no monitor could be captured (e.g. the secure
    /// desktop is up): time must not appear to stop then.
    pub fn freeze(&mut self) -> bool {
        self.frozen = false;
        // Already capturing (the build-up runs the live world pass): take the newest frame
        // and freeze on it, no stall.
        if !self.desk.is_empty() && self.desk.iter().all(|d| d.srv.is_some()) {
            self.update_duplication_timeout(0);
            if !self.desk.is_empty() && self.desk.iter().all(|d| d.srv.is_some()) {
                self.frozen = true;
                return true;
            }
        }
        // A fresh duplication delivers the whole screen on its first frame.
        self.desk.clear();
        self.desk_retry = None;
        self.ensure_duplication();
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(400) {
            self.update_duplication_timeout(40);
            if !self.desk.is_empty() && self.desk.iter().all(|d| d.srv.is_some()) {
                break;
            }
        }
        let ok = !self.desk.is_empty() && self.desk.iter().any(|d| d.srv.is_some());
        self.frozen = ok;
        if !ok {
            tracing::warn!("time stop: could not capture the screen");
        }
        ok
    }

    pub fn unfreeze(&mut self) {
        self.frozen = false;
    }

    /// Demo mode (README screenshots): show this BGRA image as "the desktop" instead of a live
    /// capture, so no real screen content ends up in the output.
    pub fn set_demo_desktop(&mut self, bgra: &[u8], w: u32, h: u32) -> anyhow::Result<()> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: bgra.as_ptr().cast(),
            SysMemPitch: w * 4,
            SysMemSlicePitch: 0,
        };
        // SAFETY: `init` points at `bgra` (w*h*4 bytes), alive for the call.
        unsafe {
            let mut tex = None;
            self.device
                .CreateTexture2D(&desc, Some(&init), Some(&mut tex))
                .context("demo texture")?;
            let tex = tex.ok_or_else(|| anyhow!("no demo texture"))?;
            let mut srv = None;
            self.device
                .CreateShaderResourceView(&tex, None, Some(&mut srv))
                .context("demo SRV")?;
            let (x, y) = (self.origin.x as i32, self.origin.y as i32);
            self.desk = vec![DeskOutput {
                dup: None,
                rect: [x, y, x + w as i32, y + h as i32],
                tex: Some(tex),
                srv,
                size: (w, h),
            }];
        }
        self.capture_safe = true;
        Ok(())
    }

    /// Start capturing ahead of a freeze so the freeze itself is instant.
    pub fn prewarm(&mut self) {
        if self.capture_safe && self.desk.is_empty() {
            self.ensure_duplication();
            self.update_duplication_timeout(0);
        }
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    /// Release desktop duplication (when the world pass is no longer needed).
    pub fn release_world(&mut self) {
        self.frozen = false;
        if !self.desk.is_empty() {
            tracing::debug!("releasing desktop duplication");
        }
        self.desk.clear();
    }

    fn ensure_duplication(&mut self) {
        if !self.capture_safe || !self.desk.is_empty() {
            return;
        }
        if self
            .desk_retry
            .is_some_and(|t| std::time::Instant::now() < t)
        {
            return;
        }
        // SAFETY: COM calls on live objects with local out-params.
        unsafe {
            let mut i = 0;
            while let Ok(output) = self.adapter.EnumOutputs(i) {
                i += 1;
                let Ok(desc) = output.GetDesc() else { continue };
                if !(desc.Rotation == DXGI_MODE_ROTATION_IDENTITY
                    || desc.Rotation == DXGI_MODE_ROTATION_UNSPECIFIED)
                {
                    tracing::info!("skipping rotated monitor for the world pass");
                    continue;
                }
                let Ok(o1) = output.cast::<IDXGIOutput1>() else {
                    continue;
                };
                match o1.DuplicateOutput(&self.device) {
                    Ok(dup) => {
                        let r = desc.DesktopCoordinates;
                        self.desk.push(DeskOutput {
                            dup: Some(dup),
                            rect: [r.left, r.top, r.right, r.bottom],
                            tex: None,
                            srv: None,
                            size: (0, 0),
                        });
                    }
                    Err(e) => tracing::warn!("DuplicateOutput failed: {e}"),
                }
            }
        }
        if self.desk.is_empty() {
            // e.g. secure desktop / hybrid-GPU laptops where outputs belong to another adapter.
            self.desk_retry = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
        } else {
            tracing::info!(
                "desktop duplication active on {} monitor(s)",
                self.desk.len()
            );
        }
    }

    /// Pull the newest desktop image of every monitor into our own textures.
    fn update_duplication(&mut self) {
        self.update_duplication_timeout(0);
    }

    fn update_duplication_timeout(&mut self, timeout_ms: u32) {
        let mut lost = false;
        for out in &mut self.desk {
            let Some(dup) = out.dup.clone() else {
                continue;
            };
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            // SAFETY: out-params are locals; every successful acquire is paired with ReleaseFrame.
            unsafe {
                match dup.AcquireNextFrame(timeout_ms, &mut info, &mut res) {
                    Ok(()) => {
                        if info.LastPresentTime != 0 {
                            if let Some(src) =
                                res.as_ref().and_then(|r| r.cast::<ID3D11Texture2D>().ok())
                            {
                                let mut sd = D3D11_TEXTURE2D_DESC::default();
                                src.GetDesc(&mut sd);
                                if out.tex.is_none() || out.size != (sd.Width, sd.Height) {
                                    let desc = D3D11_TEXTURE2D_DESC {
                                        Usage: D3D11_USAGE_DEFAULT,
                                        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                                        CPUAccessFlags: 0,
                                        MiscFlags: 0,
                                        MipLevels: 1,
                                        ArraySize: 1,
                                        SampleDesc: DXGI_SAMPLE_DESC {
                                            Count: 1,
                                            Quality: 0,
                                        },
                                        ..sd
                                    };
                                    let mut tex = None;
                                    let mut srv = None;
                                    if self
                                        .device
                                        .CreateTexture2D(&desc, None, Some(&mut tex))
                                        .is_ok()
                                    {
                                        if let Some(t) = tex.as_ref() {
                                            let _ = self.device.CreateShaderResourceView(
                                                t,
                                                None,
                                                Some(&mut srv),
                                            );
                                        }
                                    }
                                    out.tex = tex;
                                    out.srv = srv;
                                    out.size = (sd.Width, sd.Height);
                                }
                                if let Some(dst) = out.tex.as_ref() {
                                    self.ctx.CopyResource(dst, &src);
                                }
                            }
                        }
                        drop(res);
                        let _ = dup.ReleaseFrame();
                    }
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
                    Err(e) => {
                        if e.code() != DXGI_ERROR_ACCESS_LOST {
                            tracing::warn!("AcquireNextFrame failed: {e}");
                        }
                        lost = true;
                    }
                }
            }
        }
        if lost {
            // Mode change, UAC prompt, fullscreen app...: rebuild on the next frame.
            self.desk.clear();
            self.desk_retry =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
        }
    }

    fn build_world_vertices(&mut self, w: &World) {
        self.desk_vertices.clear();
        let (ow, oh) = (self.width as f32, self.height as f32);
        for out in &self.desk {
            if out.srv.is_none() {
                continue;
            }
            let [l, t, r, b] = out.rect.map(|v| v as f32);
            let corners = [
                (l, t, 0.0, 0.0),
                (r, t, 1.0, 0.0),
                (r, b, 1.0, 1.0),
                (l, b, 0.0, 1.0),
            ];
            for i in [0usize, 1, 2, 0, 2, 3] {
                let (x, y, u, v) = corners[i];
                let q = Vec2::new(x, y) - self.origin;
                let off = Vec2::new(x, y) - w.center;
                self.desk_vertices.push(Vertex {
                    pos: [q.x / ow * 2.0 - 1.0, 1.0 - q.y / oh * 2.0],
                    uv: [u, v],
                    color: [off.x, off.y, w.contrast, if w.invert { 1.0 } else { 0.0 }],
                    params: [MODE_DESKTOP, w.radius, w.feather, w.amount],
                });
            }
        }
    }

    fn set_consts(&self, c: &FrameConsts) {
        // SAFETY: `c` is a live #[repr(C)] struct of exactly the buffer's size.
        unsafe {
            self.ctx.UpdateSubresource(
                &self.cbuf,
                0,
                None,
                (c as *const FrameConsts).cast(),
                0,
                0,
            );
        }
    }

    fn viewport(&self, w: u32, h: u32) {
        let vp = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: w as f32,
            Height: h as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        // SAFETY: plain state call with a local struct.
        unsafe { self.ctx.RSSetViewports(Some(&[vp])) };
    }

    /// Unbind the post-processing inputs so a texture can become a render target.
    fn unbind_post_inputs(&self) {
        // SAFETY: plain state call.
        unsafe { self.ctx.PSSetShaderResources(2, Some(&[None, None])) };
    }

    /// Bloom: downsample `fx` through the mip chain, then upsample-accumulate back to mip 0.
    fn bloom(&self, hdr: &Hdr, consts: &mut FrameConsts) {
        if hdr.mips.is_empty() {
            return;
        }
        // SAFETY: plain state / draw calls on live objects; SRVs are unbound before the same
        // texture is bound as a render target.
        unsafe {
            self.ctx.VSSetShader(&self.sh.vs_full, None);
            self.ctx
                .OMSetBlendState(&self.blend_none, Some(&[0.0; 4]), 0xFFFF_FFFF);
            self.ctx.PSSetShader(&self.sh.ps_down, None);
            let mut src = &hdr.fx;
            for (i, dst) in hdr.mips.iter().enumerate() {
                self.unbind_post_inputs();
                self.ctx
                    .OMSetRenderTargets(Some(&[Some(dst.rtv.clone())]), None);
                self.viewport(dst.w, dst.h);
                consts.g2 = [
                    1.0 / src.w as f32,
                    1.0 / src.h as f32,
                    0.75, // threshold: only genuinely bright (HDR) light blooms, not normal-coloured shapes
                    if i == 0 { 1.0 } else { 0.0 },
                ];
                self.set_consts(consts);
                self.ctx
                    .PSSetShaderResources(2, Some(&[Some(src.srv.clone())]));
                self.ctx.Draw(3, 0);
                src = dst;
            }
            self.ctx
                .OMSetBlendState(&self.blend_add, Some(&[0.0; 4]), 0xFFFF_FFFF);
            self.ctx.PSSetShader(&self.sh.ps_up, None);
            for i in (0..hdr.mips.len() - 1).rev() {
                let (dst, small) = (&hdr.mips[i], &hdr.mips[i + 1]);
                self.unbind_post_inputs();
                self.ctx
                    .OMSetRenderTargets(Some(&[Some(dst.rtv.clone())]), None);
                self.viewport(dst.w, dst.h);
                consts.g2 = [1.0 / small.w as f32, 1.0 / small.h as f32, 1.0, 0.0];
                self.set_consts(consts);
                self.ctx
                    .PSSetShaderResources(2, Some(&[Some(small.srv.clone())]));
                self.ctx.Draw(3, 0);
            }
            self.unbind_post_inputs();
        }
    }

    fn build_crop_vertices(&mut self, w: &World, crops: &[Crop]) {
        self.crop_vertices.clear();
        self.crop_draws.clear();
        let (ow, oh) = (self.width as f32, self.height as f32);
        let ndc = |x: f32, y: f32, origin: Vec2| {
            let q = Vec2::new(x, y) - origin;
            [q.x / ow * 2.0 - 1.0, 1.0 - q.y / oh * 2.0]
        };
        for c in crops {
            if self.crop_vertices.len() + 6 > CROP_VERTS {
                break;
            }
            let [l, t, r, b] = c.dst;
            let first = self.crop_vertices.len() as u32;
            match c.src {
                Some([sl, st, sr, sb]) => {
                    let (cx, cy) = ((sl + sr) * 0.5, (st + sb) * 0.5);
                    let Some((idx, out)) = self.desk.iter().enumerate().find(|(_, o)| {
                        let [ml, mt, mr, mb] = o.rect.map(|v| v as f32);
                        o.srv.is_some() && cx >= ml && cx < mr && cy >= mt && cy < mb
                    }) else {
                        continue;
                    };
                    let [ml, mt, mr, mb] = out.rect.map(|v| v as f32);
                    let u = |x: f32| (x - ml) / (mr - ml);
                    let v = |y: f32| (y - mt) / (mb - mt);
                    let corners = [
                        (l, t, u(sl), v(st)),
                        (r, t, u(sr), v(st)),
                        (r, b, u(sr), v(sb)),
                        (l, b, u(sl), v(sb)),
                    ];
                    for i in [0usize, 1, 2, 0, 2, 3] {
                        let (x, y, uu, vv) = corners[i];
                        let off = Vec2::new(x, y) - w.center;
                        self.crop_vertices.push(Vertex {
                            pos: ndc(x, y, self.origin),
                            uv: [uu, vv],
                            color: [off.x, off.y, w.contrast, if w.invert { 1.0 } else { 0.0 }],
                            params: [MODE_DESKTOP, w.radius, w.feather, w.amount],
                        });
                    }
                    self.crop_draws.push((first, Some(idx), c.alpha));
                }
                None => {
                    let corners = [(l, t), (r, t), (r, b), (l, b)];
                    let a = (c.fill[3] * c.alpha).clamp(0.0, 1.0);
                    for i in [0usize, 1, 2, 0, 2, 3] {
                        let (x, y) = corners[i];
                        self.crop_vertices.push(Vertex {
                            pos: ndc(x, y, self.origin),
                            uv: [0.0, 0.0],
                            color: [c.fill[0], c.fill[1], c.fill[2], a],
                            params: [MODE_SOLID, 0.0, 0.0, 0.0],
                        });
                    }
                    self.crop_draws.push((first, None, 1.0));
                }
            }
        }
    }

    /// Block until the display is ready for a new frame (at most ~one refresh).
    fn wait_for_display(&self) {
        // SAFETY: valid waitable handle owned by this renderer / plain DWM call.
        unsafe {
            if let Some(h) = self.latency_wait {
                let _ = WaitForSingleObjectEx(h, 40, true);
            }
            // Composition swap chains are not throttled by Present on every system: waiting for
            // the next desktop composition is what reliably locks us to the refresh rate.
            let _ = DwmFlush();
        }
    }

    /// Render one frame (an empty batch and no world pass clears the overlay to transparent).
    pub fn render(
        &mut self,
        batch: &Batch,
        world: Option<&World>,
        crops: &[Crop],
        grade: &Grade,
    ) -> FrameResult {
        self.wait_for_display();
        let mut front = 0.0;
        let mut glitch = 0.0;
        let mut ripple = [0.0f32; 4];
        let mut look = [0.0f32; 4];
        let mut look_id = 0.0;
        let mut pinch = 0.0;
        let mut primary = [0.0; 4];
        let mut secondary = [0.0; 4];
        match world {
            Some(w) if w.amount > 0.001 && self.capture_safe => {
                if !(self.frozen && !self.desk.is_empty()) {
                    self.ensure_duplication();
                    self.update_duplication();
                }
                self.build_world_vertices(w);
                self.build_crop_vertices(w, crops);
                front = w.front;
                glitch = w.glitch;
                let (c, r, a) = w.ripple;
                ripple = [r, a, c.x - w.center.x, c.y - w.center.y];
                if let Look::TimeStop {
                    inversion,
                    flash,
                    tint,
                    chroma,
                    pinch: p,
                    primary: c1,
                    secondary: c2,
                } = w.look
                {
                    look_id = 1.0;
                    look = [inversion, flash, tint, chroma];
                    pinch = p;
                    primary = [c1[0], c1[1], c1[2], 0.0];
                    secondary = [c2[0], c2[1], c2[2], 0.0];
                }
            }
            _ => {
                self.desk_vertices.clear();
                self.crop_vertices.clear();
                self.crop_draws.clear();
            }
        }
        let nd = self.desk_vertices.len().min(DESK_VERTS);
        let nc = self.crop_vertices.len().min(CROP_VERTS);
        let n = batch.verts.len().min(self.vb_capacity - FX_BASE);
        let mut consts = FrameConsts {
            g0: [
                grade.time,
                self.width as f32,
                self.height as f32,
                if grade.film { 1.0 } else { 0.0 },
            ],
            g1: [
                if grade.bloom {
                    grade.bloom_strength
                } else {
                    0.0
                },
                1.0,
                front,
                grade.grain,
            ],
            g2: [glitch, ripple[0], ripple[1], pinch],
            g3: [ripple[2], ripple[3], 1.0, look_id],
            g4: look,
            g5: primary,
            g6: secondary,
        };
        self.set_consts(&consts);
        // SAFETY: all objects are live; the mapped pointer is written only within the buffer's
        // capacity (`nd <= DESK_VERTS`, `n <= capacity - DESK_VERTS`) and unmapped before drawing.
        unsafe {
            self.ctx
                .ClearRenderTargetView(&self.rtv, &[0.0, 0.0, 0.0, 0.0]);
            if n + nd + nc > 0 {
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                if self
                    .ctx
                    .Map(&self.vb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))
                    .is_ok()
                {
                    let dst = mapped.pData.cast::<u8>();
                    std::ptr::copy_nonoverlapping(
                        self.desk_vertices.as_ptr().cast::<u8>(),
                        dst,
                        nd * VERTEX_BYTES,
                    );
                    std::ptr::copy_nonoverlapping(
                        self.crop_vertices.as_ptr().cast::<u8>(),
                        dst.add(DESK_VERTS * VERTEX_BYTES),
                        nc * VERTEX_BYTES,
                    );
                    std::ptr::copy_nonoverlapping(
                        batch.verts.as_ptr().cast::<u8>(),
                        dst.add(FX_BASE * VERTEX_BYTES),
                        n * VERTEX_BYTES,
                    );
                    self.ctx.Unmap(&self.vb, 0);

                    self.ctx.IASetInputLayout(&self.layout);
                    self.ctx
                        .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                    let stride = VERTEX_BYTES as u32;
                    let offset = 0u32;
                    let vb = Some(self.vb.clone());
                    self.ctx
                        .IASetVertexBuffers(0, 1, Some(&vb), Some(&stride), Some(&offset));
                    self.ctx
                        .PSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
                    self.ctx.PSSetSamplers(
                        0,
                        Some(&[
                            Some(self.sampler.clone()),
                            Some(self.sampler_border.clone()),
                        ]),
                    );
                    self.ctx
                        .OMSetBlendState(&self.blend, Some(&[0.0; 4]), 0xFFFF_FFFF);
                    self.ctx.VSSetShader(&self.sh.vs, None);
                    self.ctx.PSSetShader(&self.sh.ps, None);

                    // World pass: one quad per monitor with its own desktop texture.
                    self.viewport(self.width, self.height);
                    self.ctx
                        .OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), None);
                    let mut first = 0u32;
                    for out in &self.desk {
                        let Some(srv) = out.srv.as_ref() else {
                            continue;
                        };
                        if (first as usize) >= nd {
                            break;
                        }
                        self.ctx.PSSetShaderResources(1, Some(&[Some(srv.clone())]));
                        self.ctx.Draw(6, first);
                        first += 6;
                    }
                    // The World: vacated areas and dragged-window previews, drawn from the
                    // frozen image (graded like the rest of the world).
                    for &(start, out, alpha) in &self.crop_draws {
                        if (start as usize) + 6 > nc {
                            break;
                        }
                        if let Some(srv) = out.and_then(|i| self.desk.get(i)).and_then(|o| o.srv.as_ref()) {
                            self.ctx.PSSetShaderResources(1, Some(&[Some(srv.clone())]));
                        }
                        consts.g3[2] = alpha;
                        self.set_consts(&consts);
                        self.ctx.Draw(6, (DESK_VERTS as u32) + start);
                    }
                    consts.g3[2] = 1.0;
                    self.set_consts(&consts);

                    // Effect pass.
                    if n > 0 {
                        self.ctx
                            .PSSetShaderResources(0, Some(&[self.cursor_srv.clone()]));
                        match self.hdr.as_ref() {
                            Some(hdr) => {
                                self.unbind_post_inputs();
                                self.ctx
                                    .ClearRenderTargetView(&hdr.fx.rtv, &[0.0, 0.0, 0.0, 0.0]);
                                self.ctx
                                    .OMSetRenderTargets(Some(&[Some(hdr.fx.rtv.clone())]), None);
                                self.ctx.Draw(n as u32, FX_BASE as u32);
                                if grade.bloom {
                                    self.bloom(hdr, &mut consts);
                                }
                                // Composite HDR + bloom over the world pass.
                                consts.g2 = [0.0; 4];
                                self.set_consts(&consts);
                                self.viewport(self.width, self.height);
                                self.ctx
                                    .OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), None);
                                self.ctx
                                    .OMSetBlendState(&self.blend, Some(&[0.0; 4]), 0xFFFF_FFFF);
                                self.ctx.VSSetShader(&self.sh.vs_full, None);
                                self.ctx.PSSetShader(&self.sh.ps_composite, None);
                                let bloom_srv = hdr.mips.first().map(|m| m.srv.clone());
                                self.ctx.PSSetShaderResources(
                                    2,
                                    Some(&[Some(hdr.fx.srv.clone()), bloom_srv]),
                                );
                                self.ctx.Draw(3, 0);
                                self.unbind_post_inputs();
                            }
                            None => {
                                self.ctx.Draw(n as u32, FX_BASE as u32);
                            }
                        }
                    }
                }
            }
            // Synced to the display: every frame is shown for exactly one refresh, so motion
            // is perfectly even (no dropped / doubled frames from free-running presents).
            let hr = self.swapchain.Present(1, DXGI_PRESENT(0));
            self.dirty_surface = n + nd + nc > 0;
            self.frames += 1;
            if hr.is_err() {
                tracing::warn!("Present failed: {hr:?}");
            }
            if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET {
                return FrameResult::DeviceLost;
            }
        }
        FrameResult::Ok
    }


    /// Debug aid: write the last rendered frame to a .bmp (premultiplied colour over black).
    /// Needed because the overlay is deliberately invisible to screen capture.
    pub fn dump_bmp(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.width,
            Height: self.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        // SAFETY: staging texture mapped for reading; rows are read within RowPitch bounds.
        unsafe {
            let mut staging = None;
            self.device
                .CreateTexture2D(&desc, None, Some(&mut staging))?;
            let staging = staging.ok_or_else(|| anyhow!("no staging texture"))?;
            self.ctx.CopyResource(&staging, &self.back);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let (w, h) = (self.width as usize, self.height as usize);
            let mut out = Vec::with_capacity(54 + w * h * 3);
            let row = (w * 3).div_ceil(4) * 4;
            let size = 54 + row * h;
            out.extend_from_slice(b"BM");
            out.extend_from_slice(&(size as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&54u32.to_le_bytes());
            out.extend_from_slice(&40u32.to_le_bytes());
            out.extend_from_slice(&(w as i32).to_le_bytes());
            out.extend_from_slice(&(h as i32).to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&24u16.to_le_bytes());
            out.extend_from_slice(&[0u8; 24]);
            for y in (0..h).rev() {
                let src = std::slice::from_raw_parts(
                    m.pData.cast::<u8>().add(y * m.RowPitch as usize),
                    w * 4,
                );
                for x in 0..w {
                    out.extend_from_slice(&src[x * 4..x * 4 + 3]);
                }
                out.extend(std::iter::repeat_n(0u8, row - w * 3));
            }
            self.ctx.Unmap(&staging, 0);
            std::fs::write(path, out)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod shader_tests {
    use super::*;

    #[test]
    fn time_stop_shader_compiles() {
        compile(s!("ps_main"), s!("ps_4_0")).expect("time-stop pixel shader");
        assert_eq!(std::mem::size_of::<FrameConsts>(), 7 * 16);
    }
}


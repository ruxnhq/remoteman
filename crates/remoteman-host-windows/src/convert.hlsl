// The driver's RGB texture to the encoder's 10-bit limited-range Y'CbCr, written straight into
// its input. Two kinds of picture come out:
//
// - SDR: BT.709 primaries, transfer and matrix. The same numbers as the Linux host's
//   crates/remoteman-gpu/shaders/convert.comp.
// - HDR (`pq_out`): BT.2020 primaries, the PQ transfer (SMPTE ST 2084, absolute nits) and the
//   BT.2020 non-constant-luminance matrix, as docs/PROTOCOL.md ("HDR") asks.
//
// What comes in is one of three: as painted (gamma-encoded sRGB, an 8- or 10-bit SDR desktop),
// linear light (a 16-bit float scRGB surface: BT.709 primaries, 1.0 = 80 nits), or HDR10
// (10-bit, already PQ with BT.2020 primaries).
//
// Two layouts come out. 4:2:0 in a P010 texture, drawn one plane at a time: `luma` into its
// full-size 16-bit plane, `chroma` into its half-size plane of Cb,Cr pairs, chroma being the
// average of each 2x2 block (so it sits in the block's centre, as the stream says). And 4:4:4
// in an R16 texture three times the picture's height, Y then Cb then Cr one under the other,
// drawn in one go by `planar`. Each sample is 10 bits in the high end of 16, so a code q is
// written as (q << 6) / 65535.

Texture2D<float4> src : register(t0);

cbuffer Params : register(b0) {
    // 0: as painted (sRGB). 1: linear scRGB. 2: HDR10 (PQ, BT.2020).
    uint input_kind;
    // For SDR out from linear or HDR10 in: SDR white, in the input's units (scRGB) or nits.
    float white;
    // 1: HDR out (PQ, BT.2020).
    uint pq_out;
    // For HDR out from an SDR desktop: where its white goes, in nits.
    float sdr_white_nits;
    // The picture's height, for the stacked 4:4:4 planes.
    uint height;
    uint3 unused;
};

float4 corner(uint id : SV_VertexID) : SV_Position {
    // One triangle that covers the target.
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
}

float3 srgb_encode(float3 c) {
    return lerp(12.92 * c, 1.055 * pow(max(c, 1e-6), 1.0 / 2.4) - 0.055, step(0.0031308, c));
}

float3 srgb_decode(float3 c) {
    return lerp(c / 12.92, pow(max((c + 0.055) / 1.055, 1e-6), 2.4), step(0.04045, c));
}

// Nits (0 to 10000) to PQ, and back.
float3 pq_encode(float3 nits) {
    float3 y = pow(saturate(nits / 10000.0), 0.1593017578125);
    return pow((0.8359375 + 18.8515625 * y) / (1.0 + 18.6875 * y), 78.84375);
}

float3 pq_decode(float3 e) {
    float3 p = pow(saturate(e), 1.0 / 78.84375);
    return 10000.0 * pow(max(p - 0.8359375, 0.0) / (18.8515625 - 18.6875 * p), 1.0 / 0.1593017578125);
}

// Linear BT.709 primaries to BT.2020 (ITU-R BT.2087), and back.
static const float3x3 TO_2020 = { 0.6274, 0.3293, 0.0433, 0.0691, 0.9195, 0.0114, 0.0164, 0.0880, 0.8956 };
static const float3x3 TO_709 = { 1.6605, -0.5876, -0.0728, -0.1246, 1.1329, -0.0083, -0.0182, -0.1006, 1.1187 };

// The picture's non-linear R'G'B' at a pixel, in the output's colours.
float3 rgb_out(int2 p) {
    float3 c = src.Load(int3(p, 0)).rgb;
    if (pq_out != 0) {
        if (input_kind == 2) return saturate(c);
        float3 nits = input_kind == 1 ? c * 80.0 : srgb_decode(saturate(c)) * sdr_white_nits;
        return pq_encode(max(mul(TO_2020, nits), 0.0));
    }
    if (input_kind == 1) return srgb_encode(saturate(c / white));
    if (input_kind == 2) return srgb_encode(saturate(mul(TO_709, pq_decode(c)) / white));
    return saturate(c);
}

float3 ycc(int2 p) {
    float3 rgb = rgb_out(p);
    // Kr and Kb: BT.2020's for HDR out, BT.709's otherwise.
    float2 k = pq_out != 0 ? float2(0.2627, 0.0593) : float2(0.2126, 0.0722);
    float y = dot(rgb, float3(k.x, 1.0 - k.x - k.y, k.y));
    return float3(y, (rgb.b - y) / (2.0 * (1.0 - k.y)), (rgb.r - y) / (2.0 * (1.0 - k.x)));
}

float luma_code(float y) {
    uint q = (uint)clamp(64.0 + 876.0 * y + 0.5, 0.0, 1023.0);
    return float(q << 6) / 65535.0;
}

float chroma_code(float c) {
    uint q = (uint)clamp(512.0 + 896.0 * c + 0.5, 0.0, 1023.0);
    return float(q << 6) / 65535.0;
}

float luma(float4 at : SV_Position) : SV_Target {
    return luma_code(ycc(int2(at.xy)).x);
}

float2 chroma(float4 at : SV_Position) : SV_Target {
    int2 p = int2(at.xy) * 2;
    float2 m = (ycc(p).yz + ycc(p + int2(1, 0)).yz + ycc(p + int2(0, 1)).yz + ycc(p + int2(1, 1)).yz) * 0.25;
    return float2(chroma_code(m.x), chroma_code(m.y));
}

float planar(float4 at : SV_Position) : SV_Target {
    uint row = (uint)at.y;
    uint plane = row / height;
    float3 v = ycc(int2((int)at.x, (int)(row - plane * height)));
    return plane == 0 ? luma_code(v.x) : chroma_code(plane == 1 ? v.y : v.z);
}

// The same into an R16_UINT texture (the format NVENC may want for planar 4:4:4 instead).
uint planar_uint(float4 at : SV_Position) : SV_Target {
    return (uint)round(planar(at) * 65535.0);
}

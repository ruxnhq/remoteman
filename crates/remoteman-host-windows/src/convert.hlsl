// RGB (as Windows composed it, BT.709 primaries) to 10-bit limited-range BT.709 Y'CbCr 4:2:0,
// written straight into the encoder's P010 input. The same numbers as the Linux host's
// crates/remoteman-gpu/shaders/convert.comp: the matrix, the quantising, and chroma as the
// average of each 2x2 block (so it sits in the block's centre, as the stream says it does).
//
// A P010 texture is drawn into one plane at a time: `luma` into its full-size 16-bit plane,
// `chroma` into its half-size plane of Cb,Cr pairs. Each sample is 10 bits in the high end of
// 16, so a code q is written as (q << 6) / 65535.

Texture2D<float4> src : register(t0);

cbuffer Params : register(b0) {
    // 0: the texture holds the picture as painted (gamma-encoded), as an 8-bit desktop does.
    // 1: it holds linear light (a 16-bit float surface), where 1.0 is `white` times too dim
    //    or bright for the client's white; brought to the range and gamma-encoded here.
    uint linear_in;
    float white;
    uint2 unused;
};

float4 corner(uint id : SV_VertexID) : SV_Position {
    // One triangle that covers the target.
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
}

float3 encoded(float3 c) {
    return lerp(12.92 * c, 1.055 * pow(max(c, 1e-6), 1.0 / 2.4) - 0.055, step(0.0031308, c));
}

float3 ycc(int2 p) {
    float3 rgb = src.Load(int3(p, 0)).rgb;
    if (linear_in != 0) rgb = encoded(saturate(rgb / white));
    rgb = saturate(rgb);
    float y = dot(rgb, float3(0.2126, 0.7152, 0.0722));
    return float3(y, (rgb.b - y) / 1.8556, (rgb.r - y) / 1.5748);
}

float luma(float4 at : SV_Position) : SV_Target {
    float y = ycc(int2(at.xy)).x;
    uint q = (uint)clamp(64.0 + 876.0 * y + 0.5, 0.0, 1023.0);
    return float(q << 6) / 65535.0;
}

float2 chroma(float4 at : SV_Position) : SV_Target {
    int2 p = int2(at.xy) * 2;
    float2 m = (ycc(p).yz + ycc(p + int2(1, 0)).yz + ycc(p + int2(0, 1)).yz + ycc(p + int2(1, 1)).yz) * 0.25;
    uint2 q = (uint2)clamp(512.0 + 896.0 * m + 0.5, 0.0, 1023.0);
    return float2(q << 6) / 65535.0;
}

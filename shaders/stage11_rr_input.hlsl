// Stage 11D adapter: the path tracer stores normal.xyz as [0,1] for its
// reconstruction contract, while DLSS RR requires a signed normalized world
// normal and linear roughness in the alpha channel. Keep this conversion in a
// separate pass so the DXR shader and the SVGF/NRD contracts remain unchanged.
Texture2D<float4> ReconstructionNormalRoughness : register(t0);
Texture2D<float4> ReconstructionNoisyHdr : register(t1);
Texture2D<float4> PrimaryEmissive : register(t2);
RWTexture2D<float4> PackedNormalRoughness : register(u0);
RWTexture2D<float4> RrNoisyHdr : register(u1);

cbuffer AdapterConstants : register(b0)
{
    uint ResetHistory;
};

[numthreads(8, 8, 1)]
void main(uint3 dispatchId : SV_DispatchThreadID)
{
    uint sourceWidth;
    uint sourceHeight;
    ReconstructionNormalRoughness.GetDimensions(sourceWidth, sourceHeight);
    // Render extents are not required to be multiples of the 8x8 group size.
    // Guard the padded edge threads before either the SRV load or UAV store.
    if (dispatchId.x >= sourceWidth || dispatchId.y >= sourceHeight)
    {
        return;
    }

    float4 encoded = ReconstructionNormalRoughness.Load(int3(dispatchId.xy, 0));
    float3 normal = encoded.xyz * 2.0 - 1.0;
    float normalLengthSquared = dot(normal, normal);
    // The path tracer clears an absent guide to encoded (0,0,0). Decoding it
    // first would produce a finite (-1,-1,-1), so validity must be established
    // in encoded space before normalizing. Depth still marks the pixel absent;
    // finite fallback data only keeps the plugin input deterministic.
    bool encodedNormalPresent = any(encoded.xyz != 0.0);
    bool normalValid = encodedNormalPresent && all(isfinite(encoded.xyz))
        && isfinite(normalLengthSquared) && normalLengthSquared >= 1.0e-10;
    if (!normalValid)
    {
        normal = float3(0.0, 0.0, 1.0);
    }
    else
    {
        normal *= rsqrt(normalLengthSquared);
    }
    // A non-finite roughness must never cross the plugin ABI even when depth
    // rejects the guide. Maximally rough is the conservative finite fallback.
    float roughness = isfinite(encoded.w) ? saturate(encoded.w) : 1.0;
    PackedNormalRoughness[dispatchId.xy] = float4(normal, roughness);

    // Directly visible emission is deterministic primary coverage, not a
    // stochastic diffuse/specular lobe. Keep it out of RR so the plugin does
    // not temporally reconstruct a moving halo around hard light edges. The
    // separately stabilized layer is composited after RR. Reflected emission
    // remains in NoisyHdr because PrimaryEmissive is written only at depth 0.
    float3 noisyHdr = ReconstructionNoisyHdr.Load(int3(dispatchId.xy, 0)).xyz;
    float3 primaryEmissive = PrimaryEmissive.Load(int3(dispatchId.xy, 0)).xyz;
    noisyHdr = all(isfinite(noisyHdr)) ? max(noisyHdr, 0.0) : 0.0;
    primaryEmissive = all(isfinite(primaryEmissive)) ? max(primaryEmissive, 0.0) : 0.0;
    RrNoisyHdr[dispatchId.xy] = float4(max(noisyHdr - primaryEmissive, 0.0), 1.0);
}

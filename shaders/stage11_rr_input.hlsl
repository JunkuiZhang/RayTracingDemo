// Stage 11D adapter: the path tracer stores normal.xyz as [0,1] for its
// reconstruction contract, while DLSS RR requires a signed normalized world
// normal and linear roughness in the alpha channel. Keep this conversion in a
// separate pass so the DXR shader and the SVGF/NRD contracts remain unchanged.
Texture2D<float4> ReconstructionNormalRoughness : register(t0);
RWTexture2D<float4> PackedNormalRoughness : register(u0);

cbuffer AdapterConstants : register(b0)
{
    uint ResetHistory;
};

[numthreads(8, 8, 1)]
void main(uint3 dispatchId : SV_DispatchThreadID)
{
    float4 encoded = ReconstructionNormalRoughness.Load(int3(dispatchId.xy, 0));
    float3 normal = encoded.xyz * 2.0 - 1.0;
    float normalLength = length(normal);
    // Invalid/empty guides use a stable +Z normal. RR will reject them using
    // the depth guide; emitting finite data here avoids undefined normalize(0)
    // behavior in the plugin and keeps reset frames deterministic.
    if (any(!isfinite(normal)) || !isfinite(encoded.w) || normalLength < 1.0e-5)
    {
        normal = float3(0.0, 0.0, 1.0);
    }
    else
    {
        normal /= normalLength;
    }
    PackedNormalRoughness[dispatchId.xy] = float4(normal, saturate(encoded.w));
}

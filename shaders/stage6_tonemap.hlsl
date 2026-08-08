Texture2D<float4> FilteredDiffuse : register(t0);
Texture2D<float4> FilteredSpecular : register(t1);
Texture2D<float4> RawDiffuse : register(t2);
// RawSpecular is unmodulated and must not be multiplied by albedo.
Texture2D<float4> RawSpecular : register(t3);
Texture2D<float4> Albedo : register(t4);
Texture2D<float4> NormalRoughness : register(t5);
Texture2D<float> Depth : register(t6);
Texture2D<float2> Motion : register(t7);
Texture2D<float4> Moments : register(t8);
Texture2D<uint> RejectionMask : register(t9);
Texture2D<uint2> HistoryLength : register(t10);
Texture2D<uint> Id : register(t11);
Texture2D<float> HitDistance : register(t12);
RWTexture2D<float4> Output : register(u0);

cbuffer ToneMapConstants : register(b0)
{
    uint DebugMode;
    float Exposure;
};

float3 ToneMap(float3 hdr)
{
    hdr *= Exposure;
    float3 mapped = saturate(
        (hdr * (2.51 * hdr + 0.03))
        / (hdr * (2.43 * hdr + 0.59) + 0.14));
    return pow(mapped, 1.0 / 2.2);
}

float3 RejectionColor(uint mask)
{
    if (mask == 0u)
        return float3(0.0, 0.7, 0.1);
    float3 color = 0;
    if ((mask & 1u) != 0u) color += float3(1, 0, 0);
    if ((mask & 2u) != 0u) color += float3(0, 0, 1);
    if ((mask & 4u) != 0u) color += float3(1, 0, 1);
    if ((mask & 8u) != 0u) color += float3(1, 0.6, 0);
    if ((mask & 16u) != 0u) color += float3(1, 1, 1);
    return saturate(color);
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchThreadId : SV_DispatchThreadID)
{
    uint2 size;
    Output.GetDimensions(size.x, size.y);
    if (any(dispatchThreadId.xy >= size))
        return;
    int2 pixel = int2(dispatchThreadId.xy);

    float3 color;
    if (DebugMode == 0u)
    {
        float3 diffuse = FilteredDiffuse.Load(int3(pixel, 0)).xyz
            * Albedo.Load(int3(pixel, 0)).xyz;
        color = ToneMap(diffuse + FilteredSpecular.Load(int3(pixel, 0)).xyz);
    }
    else if (DebugMode == 1u)
    {
        color = ToneMap(RawDiffuse.Load(int3(pixel, 0)).xyz
            + RawSpecular.Load(int3(pixel, 0)).xyz);
    }
    else if (DebugMode == 2u)
    {
        color = Albedo.Load(int3(pixel, 0)).xyz;
    }
    else if (DebugMode == 3u)
    {
        color = NormalRoughness.Load(int3(pixel, 0)).xyz;
    }
    else if (DebugMode == 4u)
    {
        float depth = Depth.Load(int3(pixel, 0));
        color = depth > 0.0 ? 1.0 - exp(-depth.xxx * 0.5) : 0;
    }
    else if (DebugMode == 5u)
    {
        float2 motion = Motion.Load(int3(pixel, 0));
        color = float3(saturate(abs(motion) * 0.05), 0.0);
    }
    else if (DebugMode == 6u)
    {
        float4 moments = Moments.Load(int3(pixel, 0));
        float variance = max(0.0, moments.y - moments.x * moments.x)
            + max(0.0, moments.w - moments.z * moments.z);
        color = saturate(log2(1.0 + variance) / 4.0).xxx;
    }
    else if (DebugMode == 7u)
    {
        color = RejectionColor(RejectionMask.Load(int3(pixel, 0)));
    }
    else if (DebugMode == 8u)
    {
        uint2 length = HistoryLength.Load(int3(pixel, 0));
        color = float3(saturate(float(length.x) / 64.0), saturate(float(length.y) / 32.0), 0);
    }
    else if (DebugMode == 9u)
    {
        uint id = Id.Load(int3(pixel, 0));
        color = id == 0xFFFFFFFFu
            ? 0
            : frac(float3(0.1031, 0.11369, 0.13787) * float(id + 1u));
    }
    else
    {
        float hitDistance = HitDistance.Load(int3(pixel, 0));
        color = (1.0 - exp(-hitDistance * 0.25)).xxx;
    }
    Output[pixel] = float4(color, 1.0);
}

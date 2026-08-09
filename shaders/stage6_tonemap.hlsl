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
Texture2D<float4> NrdValidation : register(t13);
RWTexture2D<float4> Output : register(u0);

cbuffer ToneMapConstants : register(b0)
{
    uint DebugMode;
    float Exposure;
    uint DenoiserMode;
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

float2 SourcePosition(uint2 outputPixel, uint2 outputSize, uint2 renderSize)
{
    return (float2(outputPixel) + 0.5) * float2(renderSize) / float2(outputSize) - 0.5;
}

void BilinearCoordinates(
    uint2 outputPixel,
    uint2 outputSize,
    uint2 renderSize,
    out int2 p00,
    out int2 p10,
    out int2 p01,
    out int2 p11,
    out float2 fraction)
{
    float2 source = SourcePosition(outputPixel, outputSize, renderSize);
    float2 base = floor(source);
    fraction = frac(source);
    int2 basePixel = int2(base);
    int2 maximum = int2(renderSize) - 1;
    p00 = clamp(basePixel, int2(0, 0), maximum);
    p10 = clamp(basePixel + int2(1, 0), int2(0, 0), maximum);
    p01 = clamp(basePixel + int2(0, 1), int2(0, 0), maximum);
    p11 = clamp(basePixel + int2(1, 1), int2(0, 0), maximum);
}

float4 LoadBilinear(
    Texture2D<float4> source,
    uint2 outputPixel,
    uint2 outputSize,
    uint2 renderSize)
{
    int2 p00, p10, p01, p11;
    float2 fraction;
    BilinearCoordinates(outputPixel, outputSize, renderSize, p00, p10, p01, p11, fraction);
    float4 row0 = lerp(source.Load(int3(p00, 0)), source.Load(int3(p10, 0)), fraction.x);
    float4 row1 = lerp(source.Load(int3(p01, 0)), source.Load(int3(p11, 0)), fraction.x);
    return lerp(row0, row1, fraction.y);
}

float2 LoadBilinear(
    Texture2D<float2> source,
    uint2 outputPixel,
    uint2 outputSize,
    uint2 renderSize)
{
    int2 p00, p10, p01, p11;
    float2 fraction;
    BilinearCoordinates(outputPixel, outputSize, renderSize, p00, p10, p01, p11, fraction);
    float2 row0 = lerp(source.Load(int3(p00, 0)), source.Load(int3(p10, 0)), fraction.x);
    float2 row1 = lerp(source.Load(int3(p01, 0)), source.Load(int3(p11, 0)), fraction.x);
    return lerp(row0, row1, fraction.y);
}

float LoadBilinear(
    Texture2D<float> source,
    uint2 outputPixel,
    uint2 outputSize,
    uint2 renderSize)
{
    int2 p00, p10, p01, p11;
    float2 fraction;
    BilinearCoordinates(outputPixel, outputSize, renderSize, p00, p10, p01, p11, fraction);
    float row0 = lerp(source.Load(int3(p00, 0)), source.Load(int3(p10, 0)), fraction.x);
    float row1 = lerp(source.Load(int3(p01, 0)), source.Load(int3(p11, 0)), fraction.x);
    return lerp(row0, row1, fraction.y);
}

uint2 DiscreteSourcePixel(uint2 outputPixel, uint2 outputSize, uint2 renderSize)
{
    uint2 numerator = uint2(
        2u * outputPixel.x + 1u,
        2u * outputPixel.y + 1u) * renderSize;
    return min(numerator / (2u * outputSize), renderSize - 1u);
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchThreadId : SV_DispatchThreadID)
{
    uint2 size;
    Output.GetDimensions(size.x, size.y);
    if (any(dispatchThreadId.xy >= size))
        return;
    uint2 pixel = dispatchThreadId.xy;
    uint2 renderSize;
    FilteredDiffuse.GetDimensions(renderSize.x, renderSize.y);
    bool nativeSize = all(size == renderSize);

    float3 color;
    if (DebugMode == 0u)
    {
        float3 diffuse = (nativeSize
            ? FilteredDiffuse.Load(int3(pixel, 0))
            : LoadBilinear(FilteredDiffuse, pixel, size, renderSize)).xyz;
        if (DenoiserMode == 0u)
        {
            diffuse *= (nativeSize
                ? Albedo.Load(int3(pixel, 0))
                : LoadBilinear(Albedo, pixel, size, renderSize)).xyz;
        }
        float3 specular = (nativeSize
            ? FilteredSpecular.Load(int3(pixel, 0))
            : LoadBilinear(FilteredSpecular, pixel, size, renderSize)).xyz;
        color = ToneMap(diffuse + specular);
    }
    else if (DebugMode == 1u)
    {
        float3 diffuse = (nativeSize
            ? RawDiffuse.Load(int3(pixel, 0))
            : LoadBilinear(RawDiffuse, pixel, size, renderSize)).xyz;
        float3 specular = (nativeSize
            ? RawSpecular.Load(int3(pixel, 0))
            : LoadBilinear(RawSpecular, pixel, size, renderSize)).xyz;
        color = ToneMap(diffuse + specular);
    }
    else if (DebugMode == 2u)
    {
        color = (nativeSize
            ? Albedo.Load(int3(pixel, 0))
            : LoadBilinear(Albedo, pixel, size, renderSize)).xyz;
    }
    else if (DebugMode == 3u)
    {
        color = (nativeSize
            ? NormalRoughness.Load(int3(pixel, 0))
            : LoadBilinear(NormalRoughness, pixel, size, renderSize)).xyz;
    }
    else if (DebugMode == 4u)
    {
        float depth = nativeSize
            ? Depth.Load(int3(pixel, 0))
            : LoadBilinear(Depth, pixel, size, renderSize);
        color = depth > 0.0 ? 1.0 - exp(-depth.xxx * 0.5) : 0;
    }
    else if (DebugMode == 5u)
    {
        float2 motion = nativeSize
            ? Motion.Load(int3(pixel, 0))
            : LoadBilinear(Motion, pixel, size, renderSize);
        color = float3(saturate(abs(motion) * 0.05), 0.0);
    }
    else if (DebugMode == 6u)
    {
        float4 moments = nativeSize
            ? Moments.Load(int3(pixel, 0))
            : LoadBilinear(Moments, pixel, size, renderSize);
        float variance = max(0.0, moments.y - moments.x * moments.x)
            + max(0.0, moments.w - moments.z * moments.z);
        color = saturate(log2(1.0 + variance) / 4.0).xxx;
    }
    else if (DebugMode == 7u)
    {
        uint2 sourcePixel = nativeSize
            ? pixel
            : DiscreteSourcePixel(pixel, size, renderSize);
        color = RejectionColor(RejectionMask.Load(int3(sourcePixel, 0)));
    }
    else if (DebugMode == 8u)
    {
        uint2 sourcePixel = nativeSize
            ? pixel
            : DiscreteSourcePixel(pixel, size, renderSize);
        uint2 length = HistoryLength.Load(int3(sourcePixel, 0));
        color = float3(saturate(float(length.x) / 64.0), saturate(float(length.y) / 32.0), 0);
    }
    else if (DebugMode == 9u)
    {
        uint2 sourcePixel = nativeSize
            ? pixel
            : DiscreteSourcePixel(pixel, size, renderSize);
        uint id = Id.Load(int3(sourcePixel, 0));
        color = id == 0xFFFFFFFFu
            ? 0
            : frac(float3(0.1031, 0.11369, 0.13787) * float(id + 1u));
    }
    else if (DebugMode == 10u)
    {
        float hitDistance = nativeSize
            ? HitDistance.Load(int3(pixel, 0))
            : LoadBilinear(HitDistance, pixel, size, renderSize);
        color = (1.0 - exp(-hitDistance * 0.25)).xxx;
    }
    else if (DebugMode == 11u)
    {
        color = DenoiserMode == 1u
            ? (nativeSize
                ? NrdValidation.Load(int3(pixel, 0))
                : LoadBilinear(NrdValidation, pixel, size, renderSize)).xyz
            : 0.0.xxx;
    }
    else
    {
        uint2 sourcePixel = nativeSize
            ? pixel
            : DiscreteSourcePixel(pixel, size, renderSize);
        float depth = Depth.Load(int3(sourcePixel, 0));
        float2 motion = Motion.Load(int3(sourcePixel, 0));
        float3 normal = NormalRoughness.Load(int3(sourcePixel, 0)).xyz * 2.0 - 1.0;
        float hitDistance = HitDistance.Load(int3(sourcePixel, 0));
        color = float3(
            depth > 0.0 && isfinite(depth),
            all(isfinite(motion)),
            hitDistance > 0.0 && all(isfinite(normal)));
    }
    Output[pixel] = float4(color, 1.0);
}

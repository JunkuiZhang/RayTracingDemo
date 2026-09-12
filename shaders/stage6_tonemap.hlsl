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
    // 0 = SVGF split signal, 1 = NRD split radiance, 2 = composed DLSS SR HDR,
    // 3 = RR-reconstructed HDR plus an independent primary-emission layer.
    // DLSS SR returns a complete HDR texture. RR instead excludes direct
    // emissive coverage so it can be stabilized and added exactly once here.
    uint InputMode;
    // 0 = SDR RGBA8/sRGB, 1 = HDR RGB10/PQ BT.2100.
    uint OutputMode;
    float PaperWhiteNits;
    float PeakNits;
};

float3 Rec709ToRec2020(float3 color)
{
    // Scene lighting is authored in linear Rec.709. HDR10 presentation uses
    // the BT.2020 primaries before applying the ST.2084 transfer function.
    const float3x3 conversion = float3x3(
        0.6274040, 0.3292820, 0.0433136,
        0.0690970, 0.9195400, 0.0113612,
        0.0163916, 0.0880132, 0.8955950);
    return max(mul(conversion, color), 0.0);
}

float3 EncodeSt2084(float3 nits)
{
    // SMPTE ST.2084 is absolute and normalized to 10,000 nit.
    const float m1 = 2610.0 / 16384.0;
    const float m2 = 2523.0 / 32.0;
    const float c1 = 3424.0 / 4096.0;
    const float c2 = 2413.0 / 128.0;
    const float c3 = 2392.0 / 128.0;
    float3 powered = pow(saturate(nits / 10000.0), m1);
    return pow((c1 + c2 * powered) / (1.0 + c3 * powered), m2);
}

float3 EncodeHdr10ReferenceColor(float3 linearRec709)
{
    return EncodeSt2084(Rec709ToRec2020(max(linearRec709, 0.0) * PaperWhiteNits));
}

float3 ToneMap(float3 hdr)
{
    hdr = max(hdr * Exposure, 0.0);
    if (OutputMode == 1u)
    {
        // Preserve Rec.709 chromaticity while mapping scene white to the
        // calibrated paper white and rolling highlights into the panel peak.
        float luminance = dot(hdr, float3(0.2126, 0.7152, 0.0722));
        float sourceNits = luminance * PaperWhiteNits;
        float headroomNits = max(PeakNits - PaperWhiteNits, 0.0);
        // Keep diffuse values through paper white linear, then apply a C1
        // continuous shoulder that approaches the panel peak. This avoids
        // changing ordinary material brightness merely because HDR is active.
        float mappedNits = min(sourceNits, PaperWhiteNits);
        if (sourceNits > PaperWhiteNits && headroomNits > 1e-3)
        {
            float highlightNits = sourceNits - PaperWhiteNits;
            mappedNits = PaperWhiteNits
                + headroomNits * (1.0 - exp(-highlightNits / headroomNits));
        }
        else if (headroomNits <= 1e-3)
        {
            mappedNits = min(sourceNits, PeakNits);
        }
        float scale = luminance > 1e-6 ? mappedNits / luminance : 0.0;
        float3 rec2020Nits = Rec709ToRec2020(hdr * scale);
        float maximum = max(rec2020Nits.r, max(rec2020Nits.g, rec2020Nits.b));
        if (maximum > PeakNits)
            rec2020Nits *= PeakNits / maximum;
        float3 encoded = EncodeSt2084(rec2020Nits);
        return all(isfinite(encoded)) ? encoded : 0.0;
    }
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
    if ((mask & 32u) != 0u) color += float3(0, 0.8, 0.8);
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

// RR mixes output-resolution reconstruction resources with render-resolution
// guides in the same descriptor table. Debug views must therefore query the
// extent of the texture they display instead of inheriting t0's extent.
float4 LoadForOutput(Texture2D<float4> source, uint2 outputPixel, uint2 outputSize)
{
    uint2 sourceSize;
    source.GetDimensions(sourceSize.x, sourceSize.y);
    return all(sourceSize == outputSize)
        ? source.Load(int3(outputPixel, 0))
        : LoadBilinear(source, outputPixel, outputSize, sourceSize);
}

float2 LoadForOutput(Texture2D<float2> source, uint2 outputPixel, uint2 outputSize)
{
    uint2 sourceSize;
    source.GetDimensions(sourceSize.x, sourceSize.y);
    return all(sourceSize == outputSize)
        ? source.Load(int3(outputPixel, 0))
        : LoadBilinear(source, outputPixel, outputSize, sourceSize);
}

float LoadForOutput(Texture2D<float> source, uint2 outputPixel, uint2 outputSize)
{
    uint2 sourceSize;
    source.GetDimensions(sourceSize.x, sourceSize.y);
    return all(sourceSize == outputSize)
        ? source.Load(int3(outputPixel, 0))
        : LoadBilinear(source, outputPixel, outputSize, sourceSize);
}

uint2 DiscretePixelForExtent(uint2 outputPixel, uint2 outputSize, uint2 sourceSize)
{
    return all(sourceSize == outputSize)
        ? outputPixel
        : DiscreteSourcePixel(outputPixel, outputSize, sourceSize);
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
        if (InputMode == 0u)
        {
            diffuse *= (nativeSize
                ? Albedo.Load(int3(pixel, 0))
                : LoadBilinear(Albedo, pixel, size, renderSize)).xyz;
        }
        if (InputMode >= 2u)
        {
            float3 composed = diffuse;
            if (InputMode == 3u)
            {
                // On the RR table t1 is an output-resolution, unjittered
                // primary-emission history rather than a specular lobe.
                float3 stableEmissive = FilteredSpecular.Load(int3(pixel, 0)).xyz;
                stableEmissive = all(isfinite(stableEmissive))
                    ? max(stableEmissive, 0.0)
                    : 0.0;
                composed += stableEmissive;
            }
            color = ToneMap(composed);
        }
        else
        {
            float3 specular = (nativeSize
                ? FilteredSpecular.Load(int3(pixel, 0))
                : LoadBilinear(FilteredSpecular, pixel, size, renderSize)).xyz;
            color = ToneMap(diffuse + specular);
        }
    }
    else if (DebugMode == 1u)
    {
        float3 diffuse = LoadForOutput(RawDiffuse, pixel, size).xyz;
        float3 specular = LoadForOutput(RawSpecular, pixel, size).xyz;
        color = ToneMap(diffuse + specular);
    }
    else if (DebugMode == 2u)
    {
        color = LoadForOutput(Albedo, pixel, size).xyz;
    }
    else if (DebugMode == 3u)
    {
        color = LoadForOutput(NormalRoughness, pixel, size).xyz;
    }
    else if (DebugMode == 4u)
    {
        float depth = LoadForOutput(Depth, pixel, size);
        color = depth > 0.0 ? 1.0 - exp(-depth.xxx * 0.5) : 0;
    }
    else if (DebugMode == 5u)
    {
        float2 motion = LoadForOutput(Motion, pixel, size);
        color = float3(saturate(abs(motion) * 0.05), 0.0);
    }
    else if (DebugMode == 6u)
    {
        float4 moments = LoadForOutput(Moments, pixel, size);
        float variance = max(0.0, moments.y - moments.x * moments.x)
            + max(0.0, moments.w - moments.z * moments.z);
        color = saturate(log2(1.0 + variance) / 4.0).xxx;
    }
    else if (DebugMode == 7u)
    {
        uint2 sourceSize;
        RejectionMask.GetDimensions(sourceSize.x, sourceSize.y);
        uint2 sourcePixel = DiscretePixelForExtent(pixel, size, sourceSize);
        color = RejectionColor(RejectionMask.Load(int3(sourcePixel, 0)));
    }
    else if (DebugMode == 8u)
    {
        uint2 sourceSize;
        HistoryLength.GetDimensions(sourceSize.x, sourceSize.y);
        uint2 sourcePixel = DiscretePixelForExtent(pixel, size, sourceSize);
        uint2 length = HistoryLength.Load(int3(sourcePixel, 0));
        color = float3(saturate(float(length.x) / 64.0), saturate(float(length.y) / 32.0), 0);
    }
    else if (DebugMode == 9u)
    {
        uint2 sourceSize;
        Id.GetDimensions(sourceSize.x, sourceSize.y);
        uint2 sourcePixel = DiscretePixelForExtent(pixel, size, sourceSize);
        uint id = Id.Load(int3(sourcePixel, 0));
        color = id == 0xFFFFFFFFu
            ? 0
            : frac(float3(0.1031, 0.11369, 0.13787) * float(id + 1u));
    }
    else if (DebugMode == 10u)
    {
        float hitDistance = LoadForOutput(HitDistance, pixel, size);
        color = (1.0 - exp(-hitDistance * 0.25)).xxx;
    }
    else if (DebugMode == 11u)
    {
        color = InputMode == 1u
            ? LoadForOutput(NrdValidation, pixel, size).xyz
            : 0.0.xxx;
    }
    else if (DebugMode == 12u)
    {
        float2 specularMotion = LoadForOutput(NrdValidation, pixel, size).xy;
        color = InputMode == 3u
            ? float3(saturate(abs(specularMotion) * 0.05), 0.0)
            : 0.0.xxx;
    }
    else if (DebugMode == 13u)
    {
        // t1 is the independently stabilized output-resolution layer only on
        // RR. Other reconstruction paths deliberately show black here.
        float3 stableEmissive = InputMode == 3u
            ? LoadForOutput(FilteredSpecular, pixel, size).xyz
            : 0.0;
        color = InputMode == 3u
            ? ToneMap(all(isfinite(stableEmissive)) ? max(stableEmissive, 0.0) : 0.0)
            : 0.0.xxx;
    }
    else
    {
        float depth = LoadForOutput(Depth, pixel, size);
        float2 motion = LoadForOutput(Motion, pixel, size);
        float3 normal = LoadForOutput(NormalRoughness, pixel, size).xyz * 2.0 - 1.0;
        float hitDistance = LoadForOutput(HitDistance, pixel, size);
        color = float3(
            depth > 0.0 && isfinite(depth),
            all(isfinite(motion)),
            hitDistance > 0.0 && all(isfinite(normal)));
    }
    // Engineering debug colors are linear Rec.709 reference colors rather
    // than scene radiance. Encode them for HDR10 without applying exposure or
    // the highlight shoulder; the normal scene views already called ToneMap.
    bool sceneToneMapped = DebugMode == 0u || DebugMode == 1u || DebugMode == 13u;
    if (OutputMode == 1u && !sceneToneMapped)
        color = EncodeHdr10ReferenceColor(color);
    Output[pixel] = float4(color, 1.0);
}

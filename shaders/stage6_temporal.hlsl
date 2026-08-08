Texture2D<float4> RawDiffuse : register(t0);
Texture2D<float4> RawSpecular : register(t1);
Texture2D<float4> Albedo : register(t2);
Texture2D<float4> NormalRoughness : register(t3);
Texture2D<float> Depth : register(t4);
Texture2D<float2> Motion : register(t5);
Texture2D<uint> Id : register(t6);
Texture2D<float4> WorldPosition : register(t7);
Texture2D<float> HitDistance : register(t8);
Texture2D<float4> PreviousDiffuse : register(t9);
Texture2D<float4> PreviousSpecular : register(t10);
Texture2D<float4> PreviousMoments : register(t11);
Texture2D<float4> PreviousNormalRoughness : register(t12);
Texture2D<float> PreviousDepth : register(t13);
Texture2D<uint2> PreviousHistoryLength : register(t14);
Texture2D<uint> PreviousId : register(t15);
Texture2D<float4> PreviousWorldPosition : register(t16);
Texture2D<float> PreviousHitDistance : register(t17);

RWTexture2D<float4> DiffuseHistory : register(u0);
RWTexture2D<float4> SpecularHistory : register(u1);
RWTexture2D<float4> MomentsHistory : register(u2);
RWTexture2D<float4> NormalHistory : register(u3);
RWTexture2D<float> DepthHistory : register(u4);
RWTexture2D<uint2> HistoryLength : register(u5);
RWTexture2D<uint> IdHistory : register(u6);
RWTexture2D<float4> WorldPositionHistory : register(u7);
RWTexture2D<float> HitDistanceHistory : register(u8);
RWTexture2D<uint> RejectionMask : register(u9);

cbuffer TemporalConstants : register(b0)
{
    uint ResetHistory;
};

float Luminance(float3 color)
{
    return dot(color, float3(0.2126, 0.7152, 0.0722));
}

void NeighborhoodBounds(
    int2 pixel,
    uint2 size,
    uint centerId,
    out float3 diffuseMinimum,
    out float3 diffuseMaximum,
    out float3 specularMinimum,
    out float3 specularMaximum)
{
    diffuseMinimum = float3(1e30, 1e30, 1e30);
    diffuseMaximum = 0;
    specularMinimum = float3(1e30, 1e30, 1e30);
    specularMaximum = 0;
    [unroll]
    for (int y = -1; y <= 1; ++y)
    {
        [unroll]
        for (int x = -1; x <= 1; ++x)
        {
            int2 neighbor = clamp(pixel + int2(x, y), int2(0, 0), int2(size) - 1);
            if (Id.Load(int3(neighbor, 0)) != centerId)
                continue;
            float3 albedo = max(Albedo.Load(int3(neighbor, 0)).xyz, 0.02);
            float3 diffuse = RawDiffuse.Load(int3(neighbor, 0)).xyz / albedo;
            float3 specular = RawSpecular.Load(int3(neighbor, 0)).xyz;
            diffuseMinimum = min(diffuseMinimum, diffuse);
            diffuseMaximum = max(diffuseMaximum, diffuse);
            specularMinimum = min(specularMinimum, specular);
            specularMaximum = max(specularMaximum, specular);
        }
    }
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchThreadId : SV_DispatchThreadID)
{
    uint2 size;
    RawDiffuse.GetDimensions(size.x, size.y);
    if (any(dispatchThreadId.xy >= size))
        return;

    int2 pixel = int2(dispatchThreadId.xy);
    float3 albedo = max(Albedo.Load(int3(pixel, 0)).xyz, 0.02);
    float3 currentDiffuse = RawDiffuse.Load(int3(pixel, 0)).xyz / albedo;
    float3 currentSpecular = RawSpecular.Load(int3(pixel, 0)).xyz;
    float4 currentNormalRoughness = NormalRoughness.Load(int3(pixel, 0));
    float3 currentNormal = currentNormalRoughness.xyz * 2.0 - 1.0;
    float currentDepth = Depth.Load(int3(pixel, 0));
    uint currentId = Id.Load(int3(pixel, 0));
    float4 currentWorldPosition = WorldPosition.Load(int3(pixel, 0));
    float currentHitDistance = HitDistance.Load(int3(pixel, 0));
    float2 previousPixelFloat = float2(pixel) - Motion.Load(int3(pixel, 0));
    int2 previousPixel = int2(round(previousPixelFloat));

    uint rejection = 0;
    if (ResetHistory != 0u)
        rejection |= 16u;
    bool inBounds = all(previousPixelFloat >= 0.0)
        && all(previousPixelFloat <= float2(size) - 1.0);
    if (!inBounds)
        rejection |= 1u;

    bool valid = rejection == 0u && currentDepth > 0.0;
    float4 previousNormalRoughness = 0;
    float previousDepth = 0;
    uint previousId = 0xFFFFFFFFu;
    float4 previousWorldPosition = 0;
    if (valid)
    {
        previousNormalRoughness = PreviousNormalRoughness.Load(int3(previousPixel, 0));
        previousDepth = PreviousDepth.Load(int3(previousPixel, 0));
        previousId = PreviousId.Load(int3(previousPixel, 0));
        previousWorldPosition = PreviousWorldPosition.Load(int3(previousPixel, 0));
        if (previousId != currentId)
            rejection |= 2u;
        if (dot(currentNormal, previousNormalRoughness.xyz * 2.0 - 1.0) < 0.9)
            rejection |= 4u;
        float worldTolerance = max(0.015, currentDepth * 0.015);
        if (previousDepth <= 0.0
            || distance(currentWorldPosition.xyz, previousWorldPosition.xyz) > worldTolerance)
            rejection |= 8u;
        valid = rejection == 0u;
    }
    else if (currentDepth <= 0.0)
    {
        rejection |= 8u;
    }

    float3 diffuse = currentDiffuse;
    float3 specular = currentSpecular;
    uint2 historyLength = uint2(1, 1);
    float diffuseLuminance = Luminance(currentDiffuse);
    float specularLuminance = Luminance(currentSpecular);
    float4 moments = float4(
        diffuseLuminance,
        diffuseLuminance * diffuseLuminance,
        specularLuminance,
        specularLuminance * specularLuminance);

    if (valid)
    {
        float3 diffuseMinimum;
        float3 diffuseMaximum;
        float3 specularMinimum;
        float3 specularMaximum;
        NeighborhoodBounds(
            pixel,
            size,
            currentId,
            diffuseMinimum,
            diffuseMaximum,
            specularMinimum,
            specularMaximum);
        float3 previousDiffuse = clamp(
            PreviousDiffuse.Load(int3(previousPixel, 0)).xyz,
            diffuseMinimum,
            diffuseMaximum);
        float3 previousSpecular = clamp(
            PreviousSpecular.Load(int3(previousPixel, 0)).xyz,
            specularMinimum,
            specularMaximum);
        uint2 previousLength = PreviousHistoryLength.Load(int3(previousPixel, 0));
        float motionMagnitude = length(Motion.Load(int3(pixel, 0)));
        uint diffuseMaximumLength = motionMagnitude > 0.25 ? 16u : 64u;
        uint specularMaximumLength = uint(lerp(4.0, 20.0, currentNormalRoughness.w));
        if (motionMagnitude <= 0.25)
            specularMaximumLength = max(specularMaximumLength, 32u);
        historyLength.x = min(previousLength.x + 1u, diffuseMaximumLength);
        historyLength.y = min(previousLength.y + 1u, specularMaximumLength);
        float diffuseAlpha = rcp(float(max(historyLength.x, 1u)));
        float specularAlpha = rcp(float(max(historyLength.y, 1u)));
        diffuse = lerp(previousDiffuse, currentDiffuse, diffuseAlpha);
        specular = lerp(previousSpecular, currentSpecular, specularAlpha);
        float4 previousMoments = PreviousMoments.Load(int3(previousPixel, 0));
        moments.xy = lerp(previousMoments.xy, moments.xy, diffuseAlpha);
        moments.zw = lerp(previousMoments.zw, moments.zw, specularAlpha);
    }

    DiffuseHistory[pixel] = float4(diffuse, 1.0);
    SpecularHistory[pixel] = float4(specular, 1.0);
    MomentsHistory[pixel] = moments;
    NormalHistory[pixel] = currentNormalRoughness;
    DepthHistory[pixel] = currentDepth;
    HistoryLength[pixel] = historyLength;
    IdHistory[pixel] = currentId;
    WorldPositionHistory[pixel] = currentWorldPosition;
    HitDistanceHistory[pixel] = currentHitDistance;
    RejectionMask[pixel] = rejection;
}

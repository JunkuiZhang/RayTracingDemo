Texture2D<float4> DiffuseInput : register(t0);
Texture2D<float4> SpecularInput : register(t1);
Texture2D<float4> NormalRoughness : register(t2);
Texture2D<float> Depth : register(t3);
Texture2D<float4> Moments : register(t4);
Texture2D<uint> Id : register(t5);
Texture2D<uint2> HistoryLength : register(t6);
Texture2D<float> HitDistance : register(t7);
RWTexture2D<float4> DiffuseOutput : register(u0);
RWTexture2D<float4> SpecularOutput : register(u1);

cbuffer AtrousConstants : register(b0)
{
    uint StepWidth;
    uint Iteration;
};

float Luminance(float3 color)
{
    return dot(color, float3(0.2126, 0.7152, 0.0722));
}

float KernelWeight(int offset)
{
    offset = abs(offset);
    return offset == 0 ? 6.0 : (offset == 1 ? 4.0 : 1.0);
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchThreadId : SV_DispatchThreadID)
{
    uint2 size;
    DiffuseInput.GetDimensions(size.x, size.y);
    if (any(dispatchThreadId.xy >= size))
        return;

    int2 pixel = int2(dispatchThreadId.xy);
    float4 centerDiffuse = DiffuseInput.Load(int3(pixel, 0));
    float4 centerSpecular = SpecularInput.Load(int3(pixel, 0));
    float4 centerNormalRoughness = NormalRoughness.Load(int3(pixel, 0));
    float3 centerNormal = centerNormalRoughness.xyz * 2.0 - 1.0;
    float centerDepth = Depth.Load(int3(pixel, 0));
    uint centerId = Id.Load(int3(pixel, 0));
    float centerHitDistance = HitDistance.Load(int3(pixel, 0));
    if (centerDepth <= 0.0)
    {
        DiffuseOutput[pixel] = centerDiffuse;
        SpecularOutput[pixel] = centerSpecular;
        return;
    }

    float4 moments = Moments.Load(int3(pixel, 0));
    uint2 historyLength = HistoryLength.Load(int3(pixel, 0));
    float diffuseVariance = max(0.0, moments.y - moments.x * moments.x);
    float specularVariance = max(0.0, moments.w - moments.z * moments.z);
    // Short histories need a wider floor so newly revealed pixels do not remain
    // as isolated fireflies while their temporal variance stabilizes.
    float diffusePhi = max(0.03, 2.0 * sqrt(diffuseVariance + rcp(float(historyLength.x + 1u))));
    float specularPhi = max(0.02, 2.0 * sqrt(specularVariance + rcp(float(historyLength.y + 1u))));
    float centerDiffuseLuminance = Luminance(centerDiffuse.xyz);
    float centerSpecularLuminance = Luminance(centerSpecular.xyz);

    float3 diffuseSum = 0;
    float3 specularSum = 0;
    float diffuseWeightSum = 0;
    float specularWeightSum = 0;
    [unroll]
    for (int y = -2; y <= 2; ++y)
    {
        [unroll]
        for (int x = -2; x <= 2; ++x)
        {
            int2 neighbor = pixel + int2(x, y) * int(StepWidth);
            if (any(neighbor < 0) || any(neighbor >= int2(size)))
                continue;
            if (Id.Load(int3(neighbor, 0)) != centerId)
                continue;

            float4 neighborNormalRoughness = NormalRoughness.Load(int3(neighbor, 0));
            float3 neighborNormal = neighborNormalRoughness.xyz * 2.0 - 1.0;
            float neighborDepth = Depth.Load(int3(neighbor, 0));
            float normalWeight = pow(saturate(dot(centerNormal, neighborNormal)), 24.0);
            float depthWeight = exp(
                -abs(neighborDepth - centerDepth)
                / max(0.005, centerDepth * 0.012 * float(StepWidth)));
            float kernel = KernelWeight(x) * KernelWeight(y);

            float3 neighborDiffuse = DiffuseInput.Load(int3(neighbor, 0)).xyz;
            float diffuseColorWeight = exp(
                -abs(Luminance(neighborDiffuse) - centerDiffuseLuminance) / diffusePhi);
            float diffuseWeight = kernel * normalWeight * depthWeight * diffuseColorWeight;
            diffuseSum += neighborDiffuse * diffuseWeight;
            diffuseWeightSum += diffuseWeight;

            float3 neighborSpecular = SpecularInput.Load(int3(neighbor, 0)).xyz;
            float neighborHitDistance = HitDistance.Load(int3(neighbor, 0));
            float hitDistanceWeight = centerNormalRoughness.w > 0.01
                ? exp(-abs(neighborHitDistance - centerHitDistance)
                    / max(0.02, centerHitDistance * (0.05 + centerNormalRoughness.w)))
                : (abs(neighborHitDistance - centerHitDistance) < 0.01 ? 1.0 : 0.0);
            float specularNormalWeight = pow(
                saturate(dot(centerNormal, neighborNormal)),
                lerp(96.0, 16.0, centerNormalRoughness.w));
            float specularColorWeight = exp(
                -abs(Luminance(neighborSpecular) - centerSpecularLuminance) / specularPhi);
            float specularWeight = kernel * depthWeight * specularNormalWeight
                * hitDistanceWeight * specularColorWeight;
            specularSum += neighborSpecular * specularWeight;
            specularWeightSum += specularWeight;
        }
    }

    DiffuseOutput[pixel] = float4(
        diffuseWeightSum > 0.0 ? diffuseSum / diffuseWeightSum : centerDiffuse.xyz,
        1.0);
    SpecularOutput[pixel] = float4(
        specularWeightSum > 0.0 ? specularSum / specularWeightSum : centerSpecular.xyz,
        1.0);
}

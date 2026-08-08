#ifndef ATROUS_LOAD_DIFFUSE
#error ATROUS_LOAD_DIFFUSE must be defined before including stage6_atrous_filter.hlsli
#endif
#ifndef ATROUS_LOAD_SPECULAR
#error ATROUS_LOAD_SPECULAR must be defined before including stage6_atrous_filter.hlsli
#endif
#ifndef ATROUS_LOAD_NORMAL_ROUGHNESS
#error ATROUS_LOAD_NORMAL_ROUGHNESS must be defined before including stage6_atrous_filter.hlsli
#endif
#ifndef ATROUS_LOAD_DEPTH
#error ATROUS_LOAD_DEPTH must be defined before including stage6_atrous_filter.hlsli
#endif
#ifndef ATROUS_LOAD_ID
#error ATROUS_LOAD_ID must be defined before including stage6_atrous_filter.hlsli
#endif
#ifndef ATROUS_LOAD_HIT_DISTANCE
#error ATROUS_LOAD_HIT_DISTANCE must be defined before including stage6_atrous_filter.hlsli
#endif

void FilterAtrousPixel(int2 pixel, uint2 size, int2 tileOrigin)
{
    float4 centerDiffuse = ATROUS_LOAD_DIFFUSE(pixel, tileOrigin);
    float4 centerSpecular = ATROUS_LOAD_SPECULAR(pixel, tileOrigin);
    float4 centerNormalRoughness = ATROUS_LOAD_NORMAL_ROUGHNESS(pixel, tileOrigin);
    float3 centerNormal = centerNormalRoughness.xyz * 2.0 - 1.0;
    float centerDepth = ATROUS_LOAD_DEPTH(pixel, tileOrigin);
    uint centerId = ATROUS_LOAD_ID(pixel, tileOrigin);
    float centerHitDistance = ATROUS_LOAD_HIT_DISTANCE(pixel, tileOrigin);
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
            if (ATROUS_LOAD_ID(neighbor, tileOrigin) != centerId)
                continue;

            float4 neighborNormalRoughness = ATROUS_LOAD_NORMAL_ROUGHNESS(neighbor, tileOrigin);
            float3 neighborNormal = neighborNormalRoughness.xyz * 2.0 - 1.0;
            float neighborDepth = ATROUS_LOAD_DEPTH(neighbor, tileOrigin);
            float normalWeight = pow(saturate(dot(centerNormal, neighborNormal)), 24.0);
            float depthWeight = exp(
                -abs(neighborDepth - centerDepth)
                / max(0.005, centerDepth * 0.012 * float(StepWidth)));
            float kernel = KernelWeight(x) * KernelWeight(y);

            float3 neighborDiffuse = ATROUS_LOAD_DIFFUSE(neighbor, tileOrigin).xyz;
            float diffuseColorWeight = exp(
                -abs(Luminance(neighborDiffuse) - centerDiffuseLuminance) / diffusePhi);
            float diffuseWeight = kernel * normalWeight * depthWeight * diffuseColorWeight;
            diffuseSum += neighborDiffuse * diffuseWeight;
            diffuseWeightSum += diffuseWeight;

            float3 neighborSpecular = ATROUS_LOAD_SPECULAR(neighbor, tileOrigin).xyz;
            float neighborHitDistance = ATROUS_LOAD_HIT_DISTANCE(neighbor, tileOrigin);
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

#undef ATROUS_LOAD_DIFFUSE
#undef ATROUS_LOAD_SPECULAR
#undef ATROUS_LOAD_NORMAL_ROUGHNESS
#undef ATROUS_LOAD_DEPTH
#undef ATROUS_LOAD_ID
#undef ATROUS_LOAD_HIT_DISTANCE

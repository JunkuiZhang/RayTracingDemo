// Stage 9 NRD back-end composition. NRD output is unpacked once, material
// factors are restored once, and primary emissive is added independently.
#include "NRD.hlsli"

Texture2D<float4> DiffuseRadianceHitDistance : register(t0);
Texture2D<float4> SpecularRadianceHitDistance : register(t1);
Texture2D<float4> DiffuseFactor : register(t2);
Texture2D<float4> SpecularFactor : register(t3);
Texture2D<float4> PrimaryEmissive : register(t4);

RWTexture2D<float4> ComposedDiffuse : register(u0);
RWTexture2D<float4> ComposedSpecular : register(u1);

[numthreads(8, 8, 1)]
void main(uint3 dispatchThreadId : SV_DispatchThreadID)
{
    uint2 size;
    ComposedDiffuse.GetDimensions(size.x, size.y);
    if (any(dispatchThreadId.xy >= size))
        return;

    int3 pixel = int3(dispatchThreadId.xy, 0);
    float3 diffuse = REBLUR_BackEnd_UnpackRadianceAndNormHitDist(
        DiffuseRadianceHitDistance.Load(pixel)).xyz;
    float3 specular = REBLUR_BackEnd_UnpackRadianceAndNormHitDist(
        SpecularRadianceHitDistance.Load(pixel)).xyz;
    float3 emissive = max(PrimaryEmissive.Load(pixel).xyz, 0.0.xxx);
    ComposedDiffuse[pixel.xy] = float4(max(diffuse * DiffuseFactor.Load(pixel).xyz, 0.0.xxx), 1.0);
    ComposedSpecular[pixel.xy] = float4(
        max(specular * SpecularFactor.Load(pixel).xyz + emissive, 0.0.xxx),
        1.0);
}

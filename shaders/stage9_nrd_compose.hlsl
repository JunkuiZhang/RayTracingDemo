// Stage 9 NRD back-end composition. NRD output is unpacked once, material
// factors are restored once, and primary emissive is added independently.
#include "NRD.hlsli"

Texture2D<float4> DiffuseRadianceHitDistance : register(t0);
Texture2D<float4> SpecularRadianceHitDistance : register(t1);
Texture2D<float4> DiffuseFactor : register(t2);
Texture2D<float4> SpecularFactor : register(t3);
Texture2D<float4> PrimaryEmissive : register(t4);
Texture2D<float4> TransmissionDiffuseRadianceHitDistance : register(t5);
Texture2D<float4> TransmissionSpecularRadianceHitDistance : register(t6);
Texture2D<float4> TransmissionDiffuseFactor : register(t7);
Texture2D<float4> TransmissionSpecularFactor : register(t8);
Texture2D<float4> TransmissionPrimaryEmissive : register(t9);
Texture2D<float4> TransmissionBaseColor : register(t10);

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
    float transmissionMask = TransmissionBaseColor.Load(pixel).w >= -0.5 ? 1.0 : 0.0;
    float3 transmissionDiffuse = REBLUR_BackEnd_UnpackRadianceAndNormHitDist(
        TransmissionDiffuseRadianceHitDistance.Load(pixel)).xyz;
    float3 transmissionSpecular = REBLUR_BackEnd_UnpackRadianceAndNormHitDist(
        TransmissionSpecularRadianceHitDistance.Load(pixel)).xyz;
    float3 restoredTransmissionDiffuse = transmissionDiffuse
        * TransmissionDiffuseFactor.Load(pixel).xyz
        * transmissionMask;
    float3 restoredTransmissionSpecular = (
        transmissionSpecular * TransmissionSpecularFactor.Load(pixel).xyz
        + max(TransmissionPrimaryEmissive.Load(pixel).xyz, 0.0.xxx))
        * transmissionMask;
    // Reflection and transmission histories stay independent through REBLUR.
    // Fresnel/tint throughput is already carried by the transmission signal,
    // so composition is a plain sum and cannot reintroduce stochastic lobe
    // selection after denoising.
    ComposedDiffuse[pixel.xy] = float4(
        max(diffuse * DiffuseFactor.Load(pixel).xyz + restoredTransmissionDiffuse, 0.0.xxx),
        1.0);
    ComposedSpecular[pixel.xy] = float4(
        max(
            specular * SpecularFactor.Load(pixel).xyz
                + emissive
                + restoredTransmissionSpecular,
            0.0.xxx),
        1.0);
}

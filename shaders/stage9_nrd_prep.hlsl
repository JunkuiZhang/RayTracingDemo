// Stage 9 NRD front-end preparation. The DXR pass deliberately keeps the
// existing SVGF raw signal semantics; this pass demodulates only the NRD copy
// and packs the locked v4.17.3 REBLUR contract.
#include "NRD.hlsli"

Texture2D<float4> RawDiffuse : register(t0);
Texture2D<float4> RawSpecular : register(t1);
Texture2D<float4> BaseColor : register(t2);
Texture2D<float4> NormalRoughness : register(t3);
Texture2D<float> ViewZ : register(t4);
Texture2D<float4> Motion : register(t5);
Texture2D<float> DiffuseHitDistance : register(t6);
Texture2D<float> SpecularHitDistance : register(t7);
Texture2D<float4> PrimaryEmissive : register(t8);
Texture2D<float4> DiffuseGuideMetallic : register(t9);
Texture2D<float4> WorldPosition : register(t10);

RWTexture2D<float4> DiffuseRadianceHitDistance : register(u0);
RWTexture2D<float4> SpecularRadianceHitDistance : register(u1);
RWTexture2D<float4> PackedNormalRoughness : register(u2);
RWTexture2D<float4> NrdMotion : register(u3);
RWTexture2D<float> NrdViewZ : register(u4);
RWTexture2D<float4> DiffuseFactor : register(u5);
RWTexture2D<float4> SpecularFactor : register(u6);

cbuffer NrdPrepConstants : register(b0)
{
    float3 CameraPosition;
}

// The hit-distance defaults are the v4.17.3 ReblurSettings defaults. Keeping
// them here and in the bridge in lockstep is part of the 9D input contract.
static const float3 HIT_DISTANCE_PARAMETERS = float3(3.0, 0.1, 20.0);

float3 SafeDivide(float3 numerator, float3 denominator)
{
    return numerator / max(abs(denominator), 0.01.xxx);
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchThreadId : SV_DispatchThreadID)
{
    uint2 size;
    RawDiffuse.GetDimensions(size.x, size.y);
    if (any(dispatchThreadId.xy >= size))
        return;

    int3 pixel = int3(dispatchThreadId.xy, 0);
    float4 normalRoughness = NormalRoughness.Load(pixel);
    float3 normal = normalize(normalRoughness.xyz * 2.0 - 1.0);
    float roughness = saturate(normalRoughness.w);
    float viewZ = ViewZ.Load(pixel);
    float4 baseColorKind = BaseColor.Load(pixel);
    float3 baseColor = max(baseColorKind.xyz, 0.0.xxx);
    // A2 stores four stable reconstruction classes: opaque, mirror, glass and
    // emissive. PSR overwrites GBufferAlbedo with the replacement surface, so
    // mirror pixels inherit the reflected material class instead of bleeding
    // history across the virtual boundary.
    float materialId = clamp(round(baseColorKind.w), 0.0, 3.0);
    float4 diffuseGuideMetallic = DiffuseGuideMetallic.Load(pixel);
    float3 diffuseAlbedo = max(diffuseGuideMetallic.xyz, 0.0.xxx);
    float metallic = saturate(diffuseGuideMetallic.w);
    float3 rf0 = lerp(0.04.xxx, baseColor, metallic);
    float3 toCamera = CameraPosition - WorldPosition.Load(pixel).xyz;
    float toCameraLengthSquared = dot(toCamera, toCamera);
    float3 view = toCameraLengthSquared > 1.0e-12
        ? toCamera * rsqrt(toCameraLengthSquared)
        : normal;
    float3 diffFactor;
    float3 specFactor;
    NRD_MaterialFactors(normal, view, diffuseAlbedo, rf0, roughness, diffFactor, specFactor);

    float3 emissive = max(PrimaryEmissive.Load(pixel).xyz, 0.0.xxx);
    float3 diffuse = SafeDivide(RawDiffuse.Load(pixel).xyz, diffFactor);
    float3 specular = SafeDivide(max(RawSpecular.Load(pixel).xyz - emissive, 0.0.xxx), specFactor);
    float diffuseHit = DiffuseHitDistance.Load(pixel);
    float specularHit = SpecularHitDistance.Load(pixel);
    float safeViewZ = (viewZ > 0.0 && isfinite(viewZ)) ? viewZ : 1001.0;

    DiffuseRadianceHitDistance[pixel.xy] = REBLUR_FrontEnd_PackRadianceAndNormHitDist(
        diffuse,
        REBLUR_FrontEnd_GetNormHitDist(diffuseHit, safeViewZ, HIT_DISTANCE_PARAMETERS, 1.0),
        true);
    SpecularRadianceHitDistance[pixel.xy] = REBLUR_FrontEnd_PackRadianceAndNormHitDist(
        specular,
        REBLUR_FrontEnd_GetNormHitDist(specularHit, safeViewZ, HIT_DISTANCE_PARAMETERS, roughness),
        true);
    PackedNormalRoughness[pixel.xy] = NRD_FrontEnd_PackNormalAndRoughness(
        normal,
        roughness,
        materialId);
    NrdMotion[pixel.xy] = Motion.Load(pixel);
    NrdViewZ[pixel.xy] = safeViewZ;
    DiffuseFactor[pixel.xy] = float4(diffFactor, 1.0);
    SpecularFactor[pixel.xy] = float4(specFactor, 1.0);
}

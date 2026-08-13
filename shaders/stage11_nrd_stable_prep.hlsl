// Convert one RTXPT-style stable plane into an independent REBLUR signal.
// Branch identity and all virtual-surface guides come from the same record;
// no primary/transmission guide is borrowed across plane histories.
#include "NRD.hlsli"
#include "stage11_path_space.hlsli"

Texture2DArray<float4> PlaneNoisyDiffuse : register(t0);
Texture2DArray<float4> PlaneNoisySpecular : register(t1);
StructuredBuffer<StablePlaneRecord> PlaneGuides : register(t2);
Texture2DArray<uint> PlaneHeaders : register(t3);

RWTexture2D<float4> DiffuseRadianceHitDistance : register(u0);
RWTexture2D<float4> SpecularRadianceHitDistance : register(u1);
RWTexture2D<float4> PackedNormalRoughness : register(u2);
RWTexture2D<float4> NrdMotion : register(u3);
RWTexture2D<float> NrdViewZ : register(u4);
RWTexture2D<float4> DiffuseFactor : register(u5);
RWTexture2D<float4> SpecularFactor : register(u6);

cbuffer StableNrdConstants : register(b0)
{
    uint PlaneIndex;
};

static const float3 HIT_DISTANCE_PARAMETERS = float3(3.0, 0.1, 20.0);

float3 StableSafeDivide(float3 numerator, float3 denominator)
{
    return numerator / max(abs(denominator), 0.01.xxx);
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchThreadId : SV_DispatchThreadID)
{
    uint2 extent;
    NrdViewZ.GetDimensions(extent.x, extent.y);
    uint2 pixel = dispatchThreadId.xy;
    if (any(pixel >= extent))
        return;

    uint branchId = PlaneHeaders.Load(int4(pixel, PlaneIndex, 0));
    if (branchId == STABLE_BRANCH_INVALID)
    {
        DiffuseRadianceHitDistance[pixel] = 0.0;
        SpecularRadianceHitDistance[pixel] = 0.0;
        PackedNormalRoughness[pixel] = NRD_FrontEnd_PackNormalAndRoughness(
            float3(0.0, 0.0, 1.0), 1.0, 0.0);
        NrdMotion[pixel] = 0.0;
        NrdViewZ[pixel] = 1001.0;
        DiffuseFactor[pixel] = 0.0;
        SpecularFactor[pixel] = 0.0;
        return;
    }

    uint address = StablePlaneAddress(pixel, PlaneIndex, extent);
    StablePlaneRecord guide = PlaneGuides[address];
    float3 normal = normalize(guide.data0.xyz);
    float roughness = saturate(guide.data0.w);
    float3 diffuseFactor = max(guide.data1.xyz, 0.0.xxx);
    float viewZ = guide.data1.w;
    float3 specularFactor = max(guide.data2.xyz, 0.0.xxx);
    float materialId = clamp(round(guide.data2.w), 0.0, 3.0);
    float3 motion = guide.data3.xyz;
    float3 emissive = UnpackStableHdr(asuint(guide.data3.w));
    float4 noisyDiffuse = PlaneNoisyDiffuse.Load(int4(pixel, PlaneIndex, 0));
    float4 noisySpecular = PlaneNoisySpecular.Load(int4(pixel, PlaneIndex, 0));
    float safeViewZ = viewZ > 0.0 && isfinite(viewZ) ? viewZ : 1001.0;

    DiffuseRadianceHitDistance[pixel] = REBLUR_FrontEnd_PackRadianceAndNormHitDist(
        StableSafeDivide(noisyDiffuse.xyz, diffuseFactor),
        REBLUR_FrontEnd_GetNormHitDist(
            max(noisyDiffuse.w, 0.0), safeViewZ, HIT_DISTANCE_PARAMETERS, 1.0),
        true);
    SpecularRadianceHitDistance[pixel] = REBLUR_FrontEnd_PackRadianceAndNormHitDist(
        StableSafeDivide(max(noisySpecular.xyz - emissive, 0.0.xxx), specularFactor),
        REBLUR_FrontEnd_GetNormHitDist(
            max(noisySpecular.w, 0.0), safeViewZ, HIT_DISTANCE_PARAMETERS, roughness),
        true);
    PackedNormalRoughness[pixel] = NRD_FrontEnd_PackNormalAndRoughness(
        normal, roughness, materialId);
    NrdMotion[pixel] = float4(motion, 0.0);
    NrdViewZ[pixel] = safeViewZ;
    DiffuseFactor[pixel] = float4(diffuseFactor, 1.0);
    SpecularFactor[pixel] = float4(specularFactor, 1.0);
}

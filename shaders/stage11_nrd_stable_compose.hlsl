// Reverse-order stable-plane merge after every plane has its own REBLUR
// history. Throughput is already carried by each noisy signal, so composition
// restores material factors/emissive exactly once and adds branch radiance.
#include "NRD.hlsli"
#include "stage11_path_space.hlsli"

Texture2D<float4> DiffuseRadianceHitDistance : register(t0);
Texture2D<float4> SpecularRadianceHitDistance : register(t1);
Texture2D<float4> DiffuseFactor : register(t2);
Texture2D<float4> SpecularFactor : register(t3);
StructuredBuffer<StablePlaneRecord> PlaneGuides : register(t4);
Texture2DArray<uint> PlaneHeaders : register(t5);

RWTexture2D<float4> ComposedDiffuse : register(u0);
RWTexture2D<float4> ComposedSpecular : register(u1);

cbuffer StableComposeConstants : register(b0)
{
    uint PlaneIndex;
    uint ClearOutput;
};

[numthreads(8, 8, 1)]
void main(uint3 dispatchThreadId : SV_DispatchThreadID)
{
    uint2 extent;
    ComposedDiffuse.GetDimensions(extent.x, extent.y);
    uint2 pixel = dispatchThreadId.xy;
    if (any(pixel >= extent))
        return;

    float3 diffuse = 0.0;
    float3 specular = 0.0;
    uint branchId = PlaneHeaders.Load(int4(pixel, PlaneIndex, 0));
    if (branchId != STABLE_BRANCH_INVALID)
    {
        uint address = StablePlaneAddress(pixel, PlaneIndex, extent);
        StablePlaneRecord guide = PlaneGuides[address];
        diffuse = REBLUR_BackEnd_UnpackRadianceAndNormHitDist(
            DiffuseRadianceHitDistance.Load(int3(pixel, 0))).xyz
            * DiffuseFactor.Load(int3(pixel, 0)).xyz;
        specular = REBLUR_BackEnd_UnpackRadianceAndNormHitDist(
            SpecularRadianceHitDistance.Load(int3(pixel, 0))).xyz
            * SpecularFactor.Load(int3(pixel, 0)).xyz
            + UnpackStableHdr(asuint(guide.data3.w));
    }

    if (ClearOutput != 0u)
    {
        ComposedDiffuse[pixel] = float4(max(diffuse, 0.0.xxx), 1.0);
        ComposedSpecular[pixel] = float4(max(specular, 0.0.xxx), 1.0);
    }
    else
    {
        ComposedDiffuse[pixel] = float4(
            max(ComposedDiffuse[pixel].xyz + diffuse, 0.0.xxx), 1.0);
        ComposedSpecular[pixel] = float4(
            max(ComposedSpecular[pixel].xyz + specular, 0.0.xxx), 1.0);
    }
}

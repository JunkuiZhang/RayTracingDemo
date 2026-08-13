// Merge RTXPT-style stable planes into one DLSS Ray Reconstruction evaluate.
// Radiance is additive across branches; reconstruction guides come from the
// deterministic dominant plane chosen during build, never from a noisy frame.
#include "stage11_camera.hlsli"
#include "stage11_path_space.hlsli"

Texture2DArray<float4> PlaneNoisyDiffuse : register(t0);
Texture2DArray<float4> PlaneNoisySpecular : register(t1);
StructuredBuffer<StablePlaneRecord> PlaneGuides : register(t2);
Texture2DArray<uint> PlaneHeaders : register(t3);
Texture2D<float4> StableRadiance : register(t4);
Texture2D<float4> StableDiffuseAlbedo : register(t5);
Texture2D<float4> StableSpecularAlbedo : register(t6);

RWTexture2D<float4> PackedNormalRoughness : register(u0);
RWTexture2D<float4> RrNoisyHdr : register(u1);
RWTexture2D<float4> RrPrimaryEmissive : register(u2);
RWTexture2D<float> RrDepth : register(u3);
RWTexture2D<float2> RrMotion : register(u4);
RWTexture2D<float2> RrSpecularMotion : register(u5);

cbuffer StableRrConstants : register(b0)
{
    uint ResetHistory;
};

[numthreads(8, 8, 1)]
void main(uint3 dispatchId : SV_DispatchThreadID)
{
    uint width;
    uint height;
    RrNoisyHdr.GetDimensions(width, height);
    uint2 extent = uint2(width, height);
    uint2 pixel = dispatchId.xy;
    if (any(pixel >= extent))
        return;

    float3 noisyHdr = 0.0;
    float3 primaryEmissive = 0.0;
    [unroll]
    for (uint planeIndex = 0u; planeIndex < STABLE_PLANE_COUNT; ++planeIndex)
    {
        uint branchId = PlaneHeaders.Load(int4(pixel, planeIndex, 0));
        if (branchId == STABLE_BRANCH_INVALID)
            continue;
        uint address = StablePlaneAddress(pixel, planeIndex, extent);
        StablePlaneRecord guide = PlaneGuides[address];
        float3 planeDiffuse = PlaneNoisyDiffuse.Load(int4(pixel, planeIndex, 0)).xyz;
        float3 planeSpecular = PlaneNoisySpecular.Load(int4(pixel, planeIndex, 0)).xyz;
        float3 planeEmissive = UnpackStableHdr(asuint(guide.data3.w));
        // Only a root-plane emissive hit is deterministic direct coverage.
        // Emission reached through a delta branch remains part of RR's noisy
        // path signal so reflected/refracted lights reconstruct normally.
        bool directEmissive = branchId == STABLE_BRANCH_ROOT
            && abs(guide.data2.w - 3.0) < 0.25;
        primaryEmissive += directEmissive ? planeEmissive : 0.0;
        noisyHdr += planeDiffuse + planeSpecular - (directEmissive ? planeEmissive : 0.0);
    }

    uint dominantPlane = min(
        uint(round(StableRadiance.Load(int3(pixel, 0)).w)),
        STABLE_PLANE_COUNT - 1u);
    uint dominantBranch = PlaneHeaders.Load(int4(pixel, dominantPlane, 0));
    if (dominantBranch == STABLE_BRANCH_INVALID)
    {
        PackedNormalRoughness[pixel] = float4(0.0, 0.0, 1.0, 1.0);
        RrDepth[pixel] = 1.0;
        RrMotion[pixel] = 0.0;
        RrSpecularMotion[pixel] = 0.0;
    }
    else
    {
        uint address = StablePlaneAddress(pixel, dominantPlane, extent);
        StablePlaneRecord guide = PlaneGuides[address];
        float3 normal = normalize(guide.data0.xyz);
        PackedNormalRoughness[pixel] = float4(normal, saturate(guide.data0.w));
        RrDepth[pixel] = Stage11DeviceDepthFromViewZ(guide.data1.w);
        float2 motion = ResetHistory != 0u ? 0.0 : guide.data3.xy;
        RrMotion[pixel] = motion;
        RrSpecularMotion[pixel] = motion;
    }

    RrNoisyHdr[pixel] = float4(max(noisyHdr, 0.0.xxx), 1.0);
    RrPrimaryEmissive[pixel] = float4(max(primaryEmissive, 0.0.xxx), 1.0);
}

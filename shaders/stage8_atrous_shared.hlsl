#include "stage6_atrous_bindings.hlsli"

static const uint OUTPUT_TILE_WIDTH = 8;
static const uint SHARED_TILE_WIDTH = 16;
static const uint SHARED_TILE_TEXEL_COUNT = SHARED_TILE_WIDTH * SHARED_TILE_WIDTH;
static const int SHARED_TILE_HALO = 4;

groupshared float4 SharedDiffuse[SHARED_TILE_TEXEL_COUNT];
groupshared float4 SharedSpecular[SHARED_TILE_TEXEL_COUNT];
groupshared float4 SharedNormalRoughness[SHARED_TILE_TEXEL_COUNT];
groupshared float SharedDepth[SHARED_TILE_TEXEL_COUNT];
groupshared float SharedHitDistance[SHARED_TILE_TEXEL_COUNT];
groupshared uint SharedId[SHARED_TILE_TEXEL_COUNT];

uint SharedTileIndex(int2 pixel, int2 tileOrigin)
{
    int2 local = pixel - tileOrigin;
    return uint(local.y) * SHARED_TILE_WIDTH + uint(local.x);
}

float4 LoadSharedDiffuse(int2 pixel, int2 tileOrigin)
{
    return SharedDiffuse[SharedTileIndex(pixel, tileOrigin)];
}

float4 LoadSharedSpecular(int2 pixel, int2 tileOrigin)
{
    return SharedSpecular[SharedTileIndex(pixel, tileOrigin)];
}

float4 LoadSharedNormalRoughness(int2 pixel, int2 tileOrigin)
{
    return SharedNormalRoughness[SharedTileIndex(pixel, tileOrigin)];
}

float LoadSharedDepth(int2 pixel, int2 tileOrigin)
{
    return SharedDepth[SharedTileIndex(pixel, tileOrigin)];
}

uint LoadSharedId(int2 pixel, int2 tileOrigin)
{
    return SharedId[SharedTileIndex(pixel, tileOrigin)];
}

float LoadSharedHitDistance(int2 pixel, int2 tileOrigin)
{
    return SharedHitDistance[SharedTileIndex(pixel, tileOrigin)];
}

#define ATROUS_LOAD_DIFFUSE(coord, tileOrigin) LoadSharedDiffuse((coord), (tileOrigin))
#define ATROUS_LOAD_SPECULAR(coord, tileOrigin) LoadSharedSpecular((coord), (tileOrigin))
#define ATROUS_LOAD_NORMAL_ROUGHNESS(coord, tileOrigin) LoadSharedNormalRoughness((coord), (tileOrigin))
#define ATROUS_LOAD_DEPTH(coord, tileOrigin) LoadSharedDepth((coord), (tileOrigin))
#define ATROUS_LOAD_ID(coord, tileOrigin) LoadSharedId((coord), (tileOrigin))
#define ATROUS_LOAD_HIT_DISTANCE(coord, tileOrigin) LoadSharedHitDistance((coord), (tileOrigin))

#include "stage6_atrous_filter.hlsli"

[numthreads(8, 8, 1)]
void main(
    uint3 dispatchThreadId : SV_DispatchThreadID,
    uint3 groupThreadId : SV_GroupThreadID,
    uint3 groupId : SV_GroupID)
{
    uint2 size;
    DiffuseInput.GetDimensions(size.x, size.y);
    int2 tileOrigin = int2(groupId.xy * OUTPUT_TILE_WIDTH) - SHARED_TILE_HALO;
    uint flatThreadIndex = groupThreadId.y * OUTPUT_TILE_WIDTH + groupThreadId.x;

    for (uint tileIndex = flatThreadIndex;
         tileIndex < SHARED_TILE_TEXEL_COUNT;
         tileIndex += OUTPUT_TILE_WIDTH * OUTPUT_TILE_WIDTH)
    {
        int2 tileOffset = int2(
            int(tileIndex % SHARED_TILE_WIDTH),
            int(tileIndex / SHARED_TILE_WIDTH));
        int2 sourcePixel = tileOrigin + tileOffset;
        bool valid = all(sourcePixel >= 0) && all(sourcePixel < int2(size));
        if (valid)
        {
            SharedDiffuse[tileIndex] = DiffuseInput.Load(int3(sourcePixel, 0));
            SharedSpecular[tileIndex] = SpecularInput.Load(int3(sourcePixel, 0));
            SharedNormalRoughness[tileIndex] = NormalRoughness.Load(int3(sourcePixel, 0));
            SharedDepth[tileIndex] = Depth.Load(int3(sourcePixel, 0));
            SharedHitDistance[tileIndex] = HitDistance.Load(int3(sourcePixel, 0));
            SharedId[tileIndex] = Id.Load(int3(sourcePixel, 0));
        }
        else
        {
            SharedDiffuse[tileIndex] = 0.0;
            SharedSpecular[tileIndex] = 0.0;
            SharedNormalRoughness[tileIndex] = float4(0.5, 0.5, 1.0, 0.0);
            SharedDepth[tileIndex] = 0.0;
            SharedHitDistance[tileIndex] = 0.0;
            SharedId[tileIndex] = 0xffffffffu;
        }
    }

    // All 64 threads must reach this barrier, including threads outside a
    // partial output group. Bounds and empty-depth exits happen afterwards.
    GroupMemoryBarrierWithGroupSync();
    if (any(dispatchThreadId.xy >= size))
        return;

    // The fixed four-pixel halo is valid only for step 1 and step 2. The Rust
    // dispatcher deliberately keeps step 4 and step 8 on the baseline PSO.
    FilterAtrousPixel(int2(dispatchThreadId.xy), size, tileOrigin);
}

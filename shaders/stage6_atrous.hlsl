#include "stage6_atrous_bindings.hlsli"

#define ATROUS_LOAD_DIFFUSE(coord, tileOrigin) DiffuseInput.Load(int3((coord), 0))
#define ATROUS_LOAD_SPECULAR(coord, tileOrigin) SpecularInput.Load(int3((coord), 0))
#define ATROUS_LOAD_NORMAL_ROUGHNESS(coord, tileOrigin) NormalRoughness.Load(int3((coord), 0))
#define ATROUS_LOAD_DEPTH(coord, tileOrigin) Depth.Load(int3((coord), 0))
#define ATROUS_LOAD_ID(coord, tileOrigin) Id.Load(int3((coord), 0))
#define ATROUS_LOAD_HIT_DISTANCE(coord, tileOrigin) HitDistance.Load(int3((coord), 0))

#include "stage6_atrous_filter.hlsli"

[numthreads(8, 8, 1)]
void main(uint3 dispatchThreadId : SV_DispatchThreadID)
{
    uint2 size;
    DiffuseInput.GetDimensions(size.x, size.y);
    if (any(dispatchThreadId.xy >= size))
        return;

    FilterAtrousPixel(int2(dispatchThreadId.xy), size, int2(0, 0));
}

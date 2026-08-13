#ifndef STAGE11_MATERIAL_HLSLI
#define STAGE11_MATERIAL_HLSLI

static const uint MATERIAL_MEDIUM_PRIORITY_MASK = 0x0fu;
static const uint MATERIAL_MEDIUM_THIN_SURFACE = 1u << 4u;

// Keep this layout in lockstep with scene::GpuMaterial. StructuredBuffer uses
// the natural 4-byte scalar layout here, for a total stride of 80 bytes.
struct Material
{
    float4 baseColorFactor;
    float3 emissiveFactor;
    float metallicFactor;
    float roughnessFactor;
    float normalScale;
    float ior;
    uint flags;
    uint baseColorTextureAndSampler;
    uint metallicRoughnessTextureAndSampler;
    uint normalTextureAndSampler;
    uint emissiveTextureAndSampler;
    float3 absorptionCoefficient;
    uint mediumFlags;
};

uint MaterialNestedPriority(Material material)
{
    return material.mediumFlags & MATERIAL_MEDIUM_PRIORITY_MASK;
}

bool MaterialIsThinSurface(Material material)
{
    return (material.mediumFlags & MATERIAL_MEDIUM_THIN_SURFACE) != 0u;
}

#endif

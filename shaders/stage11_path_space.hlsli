#ifndef STAGE11_PATH_SPACE_HLSLI
#define STAGE11_PATH_SPACE_HLSLI

// Persistent path identities used by stable-plane temporal reconstruction.
// A branch starts at one and appends one two-bit lobe ID per delta event.
static const uint STABLE_PLANE_COUNT = 3u;
static const uint STABLE_BRANCH_ROOT = 1u;
static const uint STABLE_BRANCH_JUST_STARTED = 0u;
static const uint STABLE_BRANCH_ENQUEUED = 0xfffffffeu;
static const uint STABLE_BRANCH_INVALID = 0xffffffffu;
static const uint MAX_STABLE_DELTA_VERTICES = 15u;

// The build pass uses these 64 bytes as restart state. The fill pass replaces
// them in place with denoiser guides after loading the restart state locally.
struct StablePlaneRecord
{
    float4 data0;
    float4 data1;
    float4 data2;
    float4 data3;
};

uint StablePlaneAddress(uint2 pixel, uint planeIndex, uint2 extent)
{
    return planeIndex * extent.x * extent.y + pixel.y * extent.x + pixel.x;
}

float2 EncodeStableDirection(float3 direction)
{
    direction /= abs(direction.x) + abs(direction.y) + abs(direction.z);
    float2 encoded = direction.xy;
    if (direction.z < 0.0)
    {
        float2 signNotZero = float2(
            encoded.x >= 0.0 ? 1.0 : -1.0,
            encoded.y >= 0.0 ? 1.0 : -1.0);
        encoded = (1.0 - abs(encoded.yx)) * signNotZero;
    }
    return encoded;
}

float3 DecodeStableDirection(float2 encoded)
{
    float3 direction = float3(encoded, 1.0 - abs(encoded.x) - abs(encoded.y));
    if (direction.z < 0.0)
    {
        float2 signNotZero = float2(
            direction.x >= 0.0 ? 1.0 : -1.0,
            direction.y >= 0.0 ? 1.0 : -1.0);
        direction.xy = (1.0 - abs(direction.yx)) * signNotZero;
    }
    return normalize(direction);
}

// Unsigned RGB9E5-style packing keeps per-plane emissive separate from the
// noisy specular signal without growing the 64-byte restart/guide record.
// The shared exponent is sufficient for radiance and exactly preserves zero.
uint PackStableHdr(float3 value)
{
    value = all(isfinite(value)) ? max(value, 0.0.xxx) : 0.0.xxx;
    float maximum = min(max(value.x, max(value.y, value.z)), 65408.0);
    if (maximum <= 0.0)
        return 0u;
    int exponent = clamp(int(floor(log2(maximum))) + 1, -15, 16);
    float scale = exp2(float(exponent) - 9.0);
    uint3 mantissa = min(uint3(round(value / scale)), 511u.xxx);
    uint biasedExponent = uint(exponent + 15);
    return mantissa.x
        | (mantissa.y << 9u)
        | (mantissa.z << 18u)
        | (biasedExponent << 27u);
}

float3 UnpackStableHdr(uint packed)
{
    if (packed == 0u)
        return 0.0.xxx;
    uint3 mantissa = uint3(packed, packed >> 9u, packed >> 18u) & 511u;
    int exponent = int(packed >> 27u) - 15;
    return float3(mantissa) * exp2(float(exponent) - 9.0);
}

uint StableHashUint(uint value)
{
    value ^= value >> 16u;
    value *= 0x7FEB352Du;
    value ^= value >> 15u;
    value *= 0x846CA68Bu;
    value ^= value >> 16u;
    return value;
}

uint StableSobolDimensionOne(uint sampleIndex)
{
    uint value = 0u;
    uint direction = 0x80000000u;
    while (sampleIndex != 0u)
    {
        if ((sampleIndex & 1u) != 0u)
            value ^= direction;
        sampleIndex >>= 1u;
        direction ^= direction >> 1u;
    }
    return value;
}

uint StableOwenScramble(uint value, uint seed)
{
    value = reversebits(value);
    value ^= value * 0x3D20ADEAu;
    value += seed;
    value *= (seed >> 16u) | 1u;
    value ^= value * 0x05526C56u;
    value ^= value * 0x53A22864u;
    return reversebits(value);
}

float StableUintToUnitFloat(uint value)
{
    return (float(value >> 8u) + 0.5) / 16777216.0;
}

float2 StablePrimarySampleOffset(uint2 pixel, uint frameIndex, uint guideMode)
{
    if (guideMode != 0u)
        return float2(0.5, 0.5);
    uint sampleIndex = frameIndex + 1u;
    uint pixelSeed = StableHashUint(pixel.x ^ StableHashUint(pixel.y + 0x9E3779B9u));
    uint dimensionSeed = StableHashUint(pixelSeed);
    uint x = StableOwenScramble(
        reversebits(sampleIndex), StableHashUint(dimensionSeed ^ 0x68BC21EBu));
    uint y = StableOwenScramble(
        StableSobolDimensionOne(sampleIndex),
        StableHashUint(dimensionSeed ^ 0x02E5BE93u));
    return float2(StableUintToUnitFloat(x), StableUintToUnitFloat(y));
}

bool IsPersistentStableBranch(uint branchId)
{
    return branchId != STABLE_BRANCH_JUST_STARTED
        && branchId != STABLE_BRANCH_ENQUEUED
        && branchId != STABLE_BRANCH_INVALID;
}

uint StableBranchParentLobe(uint branchId)
{
    return branchId & 3u;
}

uint StableBranchVertexIndex(uint branchId)
{
    return firstbithigh(branchId) / 2u + 1u;
}

bool AdvanceStableBranch(uint branchId, uint lobeId, out uint advancedBranchId)
{
    advancedBranchId = STABLE_BRANCH_INVALID;
    if (!IsPersistentStableBranch(branchId)
        || lobeId >= 4u
        || StableBranchVertexIndex(branchId) >= MAX_STABLE_DELTA_VERTICES)
        return false;
    advancedBranchId = (branchId << 2u) | lobeId;
    return true;
}

bool StableBranchHasAncestor(uint branchId, uint ancestorId)
{
    uint branchVertex = StableBranchVertexIndex(branchId);
    uint ancestorVertex = StableBranchVertexIndex(ancestorId);
    return ancestorVertex <= branchVertex
        && (branchId >> ((branchVertex - ancestorVertex) * 2u)) == ancestorId;
}

// Two slots cover the current solid glass and a nested liquid. Overflow is a
// visible diagnostic/fallback condition; callers must never overwrite a slot.
static const uint INTERIOR_SLOT_COUNT = 2u;
static const uint INTERIOR_MATERIAL_MASK = 0x0fffffffu;
static const uint INTERIOR_PRIORITY_SHIFT = 28u;
static const uint MAX_NESTED_PRIORITY = 15u;
static const uint NO_INTERIOR_MATERIAL = 0xffffffffu;

struct PathInteriorList
{
    uint2 slots;

    void Clear()
    {
        slots = uint2(0u, 0u);
    }

    uint MakeSlot(uint materialId, uint assetPriority)
    {
        uint priority = assetPriority == 0u ? MAX_NESTED_PRIORITY : assetPriority;
        return (priority << INTERIOR_PRIORITY_SHIFT)
            | (materialId & INTERIOR_MATERIAL_MASK);
    }

    uint SlotMaterial(uint slot)
    {
        return slot == 0u ? NO_INTERIOR_MATERIAL : slot & INTERIOR_MATERIAL_MASK;
    }

    uint SlotPriority(uint slot)
    {
        return slot >> INTERIOR_PRIORITY_SHIFT;
    }

    void Sort()
    {
        if (slots.x < slots.y)
        {
            uint temporary = slots.x;
            slots.x = slots.y;
            slots.y = temporary;
        }
    }

    uint TopMaterial()
    {
        return SlotMaterial(slots.x);
    }

    uint NextMaterial()
    {
        return SlotMaterial(slots.y);
    }

    uint TopPriority()
    {
        return SlotPriority(slots.x);
    }

    bool IsTrueIntersection(uint assetPriority)
    {
        return assetPriority == 0u || assetPriority >= TopPriority();
    }

    bool Enter(uint materialId, uint assetPriority)
    {
        if (materialId > INTERIOR_MATERIAL_MASK || assetPriority > MAX_NESTED_PRIORITY)
            return false;
        uint slot = MakeSlot(materialId, assetPriority);
        if (slots.x == 0u)
            slots.x = slot;
        else if (slots.y == 0u)
            slots.y = slot;
        else
            return false;
        Sort();
        return true;
    }

    bool Exit(uint materialId)
    {
        if (SlotMaterial(slots.x) == materialId)
            slots.x = 0u;
        else if (SlotMaterial(slots.y) == materialId)
            slots.y = 0u;
        else
            return false;
        Sort();
        return true;
    }
};

#endif

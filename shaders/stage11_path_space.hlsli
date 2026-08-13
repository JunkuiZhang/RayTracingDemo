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

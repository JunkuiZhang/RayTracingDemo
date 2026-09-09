#include "stage11_camera.hlsli"
#include "stage11_material.hlsli"
#include "stage11_path_space.hlsli"
#include "stage11_scene.hlsli"

RaytracingAccelerationStructure Scene : register(t0);
StructuredBuffer<Vertex> Vertices : register(t1);
StructuredBuffer<uint> Indices : register(t2);
StructuredBuffer<Material> Materials : register(t3);
StructuredBuffer<InstanceGpu> Instances : register(t4);

RWStructuredBuffer<StablePlaneRecord> StablePlaneRecords : register(u0);
RWTexture2DArray<uint> StablePlaneHeaders : register(u1);
RWTexture2DArray<float4> StablePlaneNoisyDiffuse : register(u2);
RWTexture2DArray<float4> StablePlaneNoisySpecular : register(u3);
RWTexture2D<float4> StableRadiance : register(u4);
RWStructuredBuffer<uint> StablePlaneCounters : register(u5);

cbuffer FrameConstants : register(b0)
{
    uint FrameIndex;
    float3 CameraPosition;
    float CameraYaw;
    float CameraPitch;
    float2 CameraJitterPx;
    float3 PreviousCameraPosition;
    float PreviousCameraYaw;
    float PreviousCameraPitch;
    uint DlssGuideMode;
    uint NrdEnabled;
    uint ResetHistory;
};

static const uint MAX_BUILD_QUEUE = 6u;
static const uint MAX_BUILD_STEPS = 16u;
static const uint MAX_FALSE_INTERSECTIONS = 8u;
static const uint STABLE_COUNTER_PIXELS_TRACED = 0u;
static const uint STABLE_COUNTER_ACTIVE_PLANE_SLOTS = 1u;
static const uint STABLE_COUNTER_PLANE_COUNT_0 = 2u;
static const uint STABLE_COUNTER_PLANE_OVERFLOW_PIXELS = 6u;
static const uint STABLE_COUNTER_BRANCH_QUEUE_OVERFLOW = 7u;
static const uint STABLE_COUNTER_INTERIOR_OVERFLOW = 8u;
static const uint STABLE_COUNTER_FALSE_INTERSECTION = 9u;
static const uint STABLE_COUNTER_TIR = 10u;
static const uint STABLE_COUNTER_INVALID_MEDIUM_EXIT = 11u;

groupshared uint StablePlaneGroupCounters[12];

struct BuildBranchState
{
    float3 origin;
    float3 direction;
    float3 throughput;
    float sceneLength;
    uint branchId;
    uint depth;
    uint2 interiorSlots;
};

float3 StablePrimaryRayDirection(uint2 pixel, uint2 extent)
{
    float2 sampleOffset = StablePrimarySampleOffset(pixel, FrameIndex, DlssGuideMode);
    float2 uv = (float2(pixel) + sampleOffset + CameraJitterPx) / float2(extent);
    float2 screen = uv * 2.0 - 1.0;
    screen.x *= float(extent.x) / float(extent.y);
    float3 forward;
    float3 right;
    float3 up;
    Stage11CameraBasis(CameraYaw, CameraPitch, forward, right, up);
    return normalize(forward * STAGE11_CAMERA_FOCAL_LENGTH + right * screen.x - up * screen.y);
}

float StableSchlick(float cosine, float incidentIor, float transmittedIor)
{
    float r0 = (incidentIor - transmittedIor) / max(incidentIor + transmittedIor, 1.0e-6);
    r0 *= r0;
    return r0 + (1.0 - r0) * pow(1.0 - cosine, 5.0);
}

float AverageThroughput(float3 throughput)
{
    return (throughput.x + throughput.y + throughput.z) / 3.0;
}

bool EnqueueBranch(
    inout BuildBranchState queue[MAX_BUILD_QUEUE],
    inout uint tail,
    BuildBranchState branch)
{
    if (AverageThroughput(branch.throughput) < 1.0e-5)
        return false;
    // Low-energy pruning is an intentional lobe decision, not queue pressure.
    // It must happen before the bounded-capacity check so the counter only
    // reports a real fork that could not be retained.
    if (tail >= MAX_BUILD_QUEUE)
    {
        InterlockedAdd(StablePlaneGroupCounters[STABLE_COUNTER_BRANCH_QUEUE_OVERFLOW], 1u);
        return false;
    }
    queue[tail++] = branch;
    return true;
}

void ProcessStablePlanePixel(uint2 pixel, uint2 extent)
{
    InterlockedAdd(StablePlaneGroupCounters[STABLE_COUNTER_PIXELS_TRACED], 1u);

    [unroll]
    for (uint plane = 0u; plane < STABLE_PLANE_COUNT; ++plane)
    {
        StablePlaneHeaders[uint3(pixel, plane)] = STABLE_BRANCH_INVALID;
        StablePlaneNoisyDiffuse[uint3(pixel, plane)] = 0.0;
        StablePlaneNoisySpecular[uint3(pixel, plane)] = 0.0;
    }
    StableRadiance[pixel] = 0.0;

    BuildBranchState queue[MAX_BUILD_QUEUE];
    BuildBranchState root;
    root.origin = CameraPosition;
    root.direction = StablePrimaryRayDirection(pixel, extent);
    root.throughput = 1.0.xxx;
    root.sceneLength = 0.0;
    root.branchId = STABLE_BRANCH_ROOT;
    root.depth = 0u;
    root.interiorSlots = 0u;
    queue[0] = root;

    uint head = 0u;
    uint tail = 1u;
    uint planeCount = 0u;
    uint dominantPlane = 0u;
    float dominantWeight = -1.0;
    float3 planeAverageThroughput = 0.0;

    while (head < tail && planeCount < STABLE_PLANE_COUNT)
    {
        BuildBranchState state = queue[head++];
        uint rejectedIntersections = 0u;

        for (uint step = 0u; step < MAX_BUILD_STEPS; ++step)
        {
            RayDesc ray;
            ray.Origin = state.origin;
            ray.Direction = state.direction;
            ray.TMin = 0.001;
            ray.TMax = 1000.0;
            RayQuery<RAY_FLAG_NONE> query;
            query.TraceRayInline(Scene, RAY_FLAG_NONE, 0xff, ray);
            while (query.Proceed())
            {
            }
            if (query.CommittedStatus() != COMMITTED_TRIANGLE_HIT)
                break;

            uint instanceId = query.CommittedInstanceID();
            InstanceGpu instanceData = Instances[instanceId];
            uint primitive = query.CommittedPrimitiveIndex();
            float2 bary = query.CommittedTriangleBarycentrics();
            float3 barycentric = float3(1.0 - bary.x - bary.y, bary.x, bary.y);
            uint3 triangleIndices = uint3(
                Indices[instanceData.indexOffset + primitive * 3u],
                Indices[instanceData.indexOffset + primitive * 3u + 1u],
                Indices[instanceData.indexOffset + primitive * 3u + 2u]);
            uint vertex0 = instanceData.vertexOffset + triangleIndices.x;
            uint vertex1 = instanceData.vertexOffset + triangleIndices.y;
            uint vertex2 = instanceData.vertexOffset + triangleIndices.z;
            float3 localNormal = normalize(
                Vertices[vertex0].normal * barycentric.x
                + Vertices[vertex1].normal * barycentric.y
                + Vertices[vertex2].normal * barycentric.z);
            float3 geometricNormal = normalize(mul(
                localNormal,
                (float3x3)query.CommittedWorldToObject3x4()));
            bool frontFace = query.CommittedTriangleFrontFace();
            float3 normal = frontFace ? geometricNormal : -geometricNormal;
            uint materialIndex = instanceData.materialIndex;
            Material material = Materials[materialIndex];
            bool dielectric = (material.flags & 4u) != 0u;
            bool mirror = (material.flags & 8u) != 0u;
            float hitT = query.CommittedRayT();
            float3 hitPosition = state.origin + state.direction * hitT;

            PathInteriorList interior;
            interior.slots = state.interiorSlots;
            uint currentMaterial = interior.TopMaterial();
            if (currentMaterial != NO_INTERIOR_MATERIAL)
            {
                // Closed media tint by traveled distance, not by repeatedly
                // multiplying the interface base color. This is the same
                // Beer-Lambert convention used by RTXPT-style nested media.
                state.throughput *= exp(
                    -max(Materials[currentMaterial].absorptionCoefficient, 0.0.xxx) * hitT);
            }
            if (dielectric && !MaterialIsThinSurface(material)
                && !interior.IsTrueIntersection(MaterialNestedPriority(material)))
            {
                InterlockedAdd(
                    StablePlaneGroupCounters[STABLE_COUNTER_FALSE_INTERSECTION],
                    1u);
                if (++rejectedIntersections > MAX_FALSE_INTERSECTIONS)
                    break;
                state.origin = hitPosition + state.direction * 0.002;
                state.sceneLength += hitT;
                continue;
            }

            if (!dielectric && !mirror)
            {
                uint address = StablePlaneAddress(pixel, planeCount, extent);
                StablePlaneRecord record;
                record.data0 = float4(state.origin, hitT);
                record.data1 = float4(state.direction, state.sceneLength);
                record.data2 = float4(state.throughput, asfloat(state.depth));
                record.data3 = float4(
                    asfloat(state.interiorSlots.x),
                    asfloat(state.interiorSlots.y),
                    EncodeStableDirection(StablePrimaryRayDirection(pixel, extent)));
                StablePlaneRecords[address] = record;
                StablePlaneHeaders[uint3(pixel, planeCount)] = state.branchId;
                float weight = AverageThroughput(state.throughput);
                planeAverageThroughput[planeCount] = weight;
                if (weight > dominantWeight)
                {
                    dominantWeight = weight;
                    dominantPlane = planeCount;
                }
                ++planeCount;
                break;
            }

            uint reflectedBranchId;
            if (!AdvanceStableBranch(state.branchId, 1u, reflectedBranchId))
                break;
            BuildBranchState reflected = state;
            reflected.origin = hitPosition + normal * 0.002;
            reflected.direction = normalize(reflect(state.direction, normal));
            reflected.sceneLength += hitT;
            reflected.branchId = reflectedBranchId;
            reflected.depth = state.depth + 1u;

            if (mirror)
            {
                reflected.throughput *= max(material.baseColorFactor.xyz, 0.0.xxx);
                // A mirror has no competing lobe. Continuing in the current
                // state avoids spending fork capacity on a path whose only
                // legal continuation is already known.
                state = reflected;
                continue;
            }

            float incidentIor = currentMaterial == NO_INTERIOR_MATERIAL
                ? 1.0
                : max(Materials[currentMaterial].ior, 1.0e-4);
            PathInteriorList transmittedInterior = interior;
            float transmittedIor = 1.0;
            bool mediumUpdateValid = true;
            if (MaterialIsThinSurface(material))
            {
                transmittedIor = max(material.ior, 1.0e-4);
            }
            else if (frontFace)
            {
                transmittedIor = max(material.ior, 1.0e-4);
                mediumUpdateValid = transmittedInterior.Enter(
                    materialIndex,
                    MaterialNestedPriority(material));
                if (!mediumUpdateValid)
                    InterlockedAdd(
                        StablePlaneGroupCounters[STABLE_COUNTER_INTERIOR_OVERFLOW],
                        1u);
            }
            else
            {
                mediumUpdateValid = transmittedInterior.Exit(materialIndex);
                if (!mediumUpdateValid)
                    InterlockedAdd(
                        StablePlaneGroupCounters[STABLE_COUNTER_INVALID_MEDIUM_EXIT],
                        1u);
                uint outerMaterial = transmittedInterior.TopMaterial();
                transmittedIor = outerMaterial == NO_INTERIOR_MATERIAL
                    ? 1.0
                    : max(Materials[outerMaterial].ior, 1.0e-4);
            }

            float cosine = saturate(dot(-state.direction, normal));
            float fresnel = StableSchlick(cosine, incidentIor, transmittedIor);
            float eta = incidentIor / transmittedIor;
            float3 refractionDirection = MaterialIsThinSurface(material)
                ? state.direction
                : refract(state.direction, normal, eta);
            bool totalInternalReflection = length(refractionDirection) < 1.0e-4;
            if (totalInternalReflection)
                InterlockedAdd(StablePlaneGroupCounters[STABLE_COUNTER_TIR], 1u);
            reflected.throughput *= totalInternalReflection ? 1.0 : fresnel;
            if (totalInternalReflection)
            {
                // TIR has one legal lobe. It is an in-place continuation, not
                // a fork, so nested glass cannot lose its only path to queue
                // pressure.
                state = reflected;
                continue;
            }

            uint transmittedBranchId;
            if (!mediumUpdateValid
                || !AdvanceStableBranch(state.branchId, 2u, transmittedBranchId))
            {
                // An invalid medium transition must never fabricate a
                // transmitted interior list. Reflection is the only legal
                // fallback and remains the current state rather than a fork.
                state = reflected;
                continue;
            }

            BuildBranchState transmitted = state;
            transmitted.origin = hitPosition + normalize(refractionDirection) * 0.002;
            transmitted.direction = normalize(refractionDirection);
            transmitted.throughput *= 1.0 - fresnel;
            transmitted.sceneLength += hitT;
            transmitted.branchId = transmittedBranchId;
            transmitted.depth = state.depth + 1u;
            transmitted.interiorSlots = transmittedInterior.slots;

            // Keep the lobe order fixed across pixels: reflection is the
            // bounded fork, while transmission preserves the current path's
            // branch identity. Throughput sorting would swap plane identities
            // around Fresnel crossings and create denoiser spatial seams.
            EnqueueBranch(queue, tail, reflected);
            state = transmitted;
            continue;
        }
    }

    if (head < tail && planeCount >= STABLE_PLANE_COUNT)
        InterlockedAdd(StablePlaneGroupCounters[STABLE_COUNTER_PLANE_OVERFLOW_PIXELS], 1u);

    // These are final per-pixel classifications. Updating the histogram while
    // discovering planes would make a three-plane pixel appear once in each
    // of the 0/1/2 buckets and would leave the 3 bucket permanently empty.
    InterlockedAdd(
        StablePlaneGroupCounters[STABLE_COUNTER_ACTIVE_PLANE_SLOTS],
        planeCount);
    InterlockedAdd(
        StablePlaneGroupCounters[STABLE_COUNTER_PLANE_COUNT_0 + planeCount],
        1u);

    // Match RTXPT's RR preparation principle: radiance from all stable planes
    // needs one deterministic, correspondingly mixed set of material guides.
    // Plane zero is the primary continuation; secondary lobe throughput is
    // subtracted from its residual share before equalization and the stable
    // dominant-plane bias are applied.
    float3 available = 0.0;
    float3 throughputWeights = float3(1.0, 0.0, 0.0);
    [unroll]
    for (uint plane = 0u; plane < STABLE_PLANE_COUNT; ++plane)
    {
        if (plane < planeCount)
            available[plane] = 1.0;
        if (plane > 0u && plane < planeCount)
        {
            float branchWeight = saturate(planeAverageThroughput[plane]);
            throughputWeights[plane] = branchWeight;
            throughputWeights[0] = saturate(throughputWeights[0] - branchWeight);
        }
    }
    float3 guideWeights = throughputWeights * 0.2 + available * 0.01;
    guideWeights[dominantPlane] += 0.05;
    guideWeights *= available;
    guideWeights /= max(guideWeights.x + guideWeights.y + guideWeights.z, 1.0e-6);
    [unroll]
    for (uint plane = 0u; plane < STABLE_PLANE_COUNT; ++plane)
    {
        if (plane >= planeCount)
            continue;
        uint address = StablePlaneAddress(pixel, plane, extent);
        StablePlaneRecord restart = StablePlaneRecords[address];
        restart.data2.w = guideWeights[plane];
        StablePlaneRecords[address] = restart;
    }

    // Alpha carries only an exact small integer and is not radiance. P3/P4 use
    // it to select the primary guide after all planes have been filled.
    StableRadiance[pixel] = float4(0.0, 0.0, 0.0, float(dominantPlane));
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchId : SV_DispatchThreadID, uint3 groupThreadId : SV_GroupThreadID)
{
    uint width;
    uint height;
    StableRadiance.GetDimensions(width, height);
    uint2 extent = uint2(width, height);
    uint linearThread = groupThreadId.y * 8u + groupThreadId.x;
    if (linearThread < 12u)
        StablePlaneGroupCounters[linearThread] = 0u;

    // Out-of-bounds threads participate in both barriers. Returning before
    // the first barrier would leave in-bounds lanes waiting forever on a
    // partially filled thread group at non-8-aligned render extents.
    GroupMemoryBarrierWithGroupSync();
    bool inBounds = all(dispatchId.xy < extent);
    if (inBounds)
        ProcessStablePlanePixel(dispatchId.xy, extent);
    GroupMemoryBarrierWithGroupSync();
    if (linearThread < 12u)
        InterlockedAdd(StablePlaneCounters[linearThread], StablePlaneGroupCounters[linearThread]);
}

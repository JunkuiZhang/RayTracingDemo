// Cornell Box DXR path tracer. This pass only traces one independent sample and
// writes first-hit attributes. RawDiffuse is albedo-demodulated diffuse only;
// RawSpecular is the unmodulated specular + emissive signal. Temporal
// reconstruction and spatial filtering are deliberately separate dispatches.
#include "stage11_camera.hlsli"
#include "stage11_material.hlsli"
#include "stage11_path_space.hlsli"
#include "stage11_scene.hlsli"
RaytracingAccelerationStructure Scene : register(t0);

StructuredBuffer<Vertex> Vertices : register(t1);
StructuredBuffer<uint> Indices : register(t2);
StructuredBuffer<Material> Materials : register(t3);
StructuredBuffer<InstanceGpu> Instances : register(t4);
Texture2D<float4> MaterialTextures[] : register(t5);
static const uint MAX_MATERIAL_SAMPLERS = 64u;
SamplerState MaterialSamplers[MAX_MATERIAL_SAMPLERS] : register(s0);

RWTexture2D<float4> RawDiffuse : register(u0);
RWTexture2D<float4> RawSpecular : register(u1);
RWTexture2D<float4> GBufferAlbedo : register(u2);
RWTexture2D<float4> GBufferNormalRoughness : register(u3);
RWTexture2D<float> GBufferDepth : register(u4);
RWTexture2D<float2> GBufferMotion : register(u5);
RWTexture2D<uint> GBufferId : register(u6);
RWTexture2D<float4> GBufferWorldPosition : register(u7);
RWTexture2D<float> GBufferHitDistance : register(u8);
// Stage 9 reconstruction guides. Existing G-buffer resources above retain
// their Stage 6 SVGF semantics; these resources use the explicit NRD/RR
// contract and are not consumed by SVGF.
RWTexture2D<float4> ReconstructionNoisyHdr : register(u9);
RWTexture2D<float4> ReconstructionDiffuseAlbedo : register(u10);
RWTexture2D<float4> ReconstructionSpecularAlbedo : register(u11);
RWTexture2D<float4> ReconstructionNormalRoughness : register(u12);
RWTexture2D<float> ReconstructionViewZ : register(u13);
// XY follows NRD's old = new + MV convention in pixel units. Z is the
// previous/current linear viewZ delta; W is reserved for future adapters.
RWTexture2D<float4> ReconstructionMotion : register(u14);
RWTexture2D<float> ReconstructionDiffuseHitDistance : register(u15);
RWTexture2D<float> ReconstructionSpecularHitDistance : register(u16);
RWTexture2D<float4> ReconstructionPrimaryEmissive : register(u17);
// DLSS owns separate dense depth/motion resources. GBufferMotion retains its
// existing Stage 6 direction for SVGF and NRD.
RWTexture2D<float> DlssDepth : register(u18);
RWTexture2D<float2> DlssMotion : register(u19);
// Camera-visible glass owns a second reconstruction layer. Reflection remains
// in the primary NRD signal, while the surface reached through refraction gets
// a complete, independent guide set. Combining the two before denoising would
// force two depths and motion fields through one temporal history.
RWTexture2D<float4> TransmissionRawDiffuse : register(u20);
RWTexture2D<float4> TransmissionRawSpecular : register(u21);
RWTexture2D<float4> TransmissionBaseColor : register(u22);
RWTexture2D<float4> TransmissionNormalRoughness : register(u23);
RWTexture2D<float> TransmissionViewZ : register(u24);
RWTexture2D<float4> TransmissionMotion : register(u25);
RWTexture2D<float> TransmissionDiffuseHitDistance : register(u26);
RWTexture2D<float> TransmissionSpecularHitDistance : register(u27);
RWTexture2D<float4> TransmissionPrimaryEmissive : register(u28);
RWTexture2D<float4> TransmissionDiffuseAlbedo : register(u29);
RWTexture2D<float4> TransmissionViewProxy : register(u30);
// RR can consume reflected-geometry motion directly. This is preferable to
// reconstructing it from a scalar hit distance because the latter inherits
// subpixel variation from the primary ray origin.
RWTexture2D<float2> DlssSpecularMotion : register(u31);
RWStructuredBuffer<StablePlaneRecord> StablePlaneRecords : register(u32);
RWTexture2DArray<uint> StablePlaneHeaders : register(u33);
RWTexture2DArray<float4> StablePlaneNoisyDiffuse : register(u34);
RWTexture2DArray<float4> StablePlaneNoisySpecular : register(u35);
RWTexture2D<float4> StableRadiance : register(u36);
RWTexture2D<float4> StableDiffuseAlbedo : register(u37);
RWTexture2D<float4> StableSpecularAlbedo : register(u38);

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
    // 0 = no Streamline guides, 1 = DLSS SR guides, 2 = DLSS RR guides.
    // RR needs a distinct value because it additionally writes an explicit
    // reflected-geometry motion field, while SR consumes only primary guides.
    uint DlssGuideMode;
    uint NrdEnabled;
    uint ResetHistory;
};

cbuffer PathSpaceConstants : register(b1)
{
    // 0 = ordinary legacy raygen, 1 = fill one previously built stable plane.
    uint PathSpacePass;
    uint StablePlaneIndex;
};

struct Payload
{
    float3 radiance;
    uint seed;
    uint depth;
    float lastPdf;
    uint firstKind;
    float hitDistance;
    float3 rawDiffuse;
    float3 rawSpecular;
    // NRD Primary Surface Replacement (PSR) state. A single planar mirror is
    // deliberately stored as a plane instead of a full affine transform to
    // keep the recursive DXR payload below 96 bytes on laptop GPUs. A second
    // consecutive mirror falls back to the ordinary specular signal.
    uint psrActive;
    uint sampleDimensionOffset;
    float3 psrThroughput;
    float4 psrMirrorPlane;
};

uint RandomUint(inout uint state)
{
    // PCG-inspired permutation. The frame/pixel seed remains reproducible while
    // avoiding the strong neighboring-pixel correlation of a bare xorshift.
    state = state * 747796405u + 2891336453u;
    uint word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

float RandomFloat(inout uint state)
{
    return (RandomUint(state) & 0x00FFFFFFu) / 16777216.0;
}

uint HashUint(uint value)
{
    value ^= value >> 16u;
    value *= 0x7FEB352Du;
    value ^= value >> 15u;
    value *= 0x846CA68Bu;
    value ^= value >> 16u;
    return value;
}

uint SobolDimensionOne(uint sampleIndex)
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

// Laine-Karras nested uniform scrambling applied to the first two Sobol
// dimensions. Each pixel/dimension receives a stable scramble while the frame
// index advances through a low-discrepancy temporal sequence.
uint OwenScramble(uint value, uint seed)
{
    value = reversebits(value);
    value ^= value * 0x3D20ADEAu;
    value += seed;
    value *= (seed >> 16u) | 1u;
    value ^= value * 0x05526C56u;
    value ^= value * 0x53A22864u;
    return reversebits(value);
}

float UintToUnitFloat(uint value)
{
    return (float(value >> 8u) + 0.5) / 16777216.0;
}

float2 SampleOwenSobol2D(uint2 pixel, uint dimension)
{
    uint sampleIndex = FrameIndex + 1u;
    uint pixelSeed = HashUint(pixel.x ^ HashUint(pixel.y + 0x9E3779B9u));
    uint dimensionSeed = HashUint(pixelSeed ^ (dimension * 0xA511E9B3u));
    uint x = OwenScramble(reversebits(sampleIndex), HashUint(dimensionSeed ^ 0x68BC21EBu));
    uint y = OwenScramble(SobolDimensionOne(sampleIndex), HashUint(dimensionSeed ^ 0x02E5BE93u));
    return float2(UintToUnitFloat(x), UintToUnitFloat(y));
}

float2 PrimaryRaySampleOffset(uint2 pixel)
{
    // DLSS SR and Ray Reconstruction must see exactly the global projection
    // jitter submitted through Streamline. Adding an independent per-pixel
    // offset here creates hidden primary-coverage motion at hard silhouettes.
    // Native rendering has no temporal-upscaler jitter contract, so it keeps
    // the existing Owen-Sobol subpixel sampling for stochastic anti-aliasing.
    return DlssGuideMode != 0u
        ? float2(0.5, 0.5)
        : SampleOwenSobol2D(pixel, 0u);
}

uint Bayer4x4Value(uint2 pixel)
{
    static const uint values[16] = {
        0u, 8u, 2u, 10u,
        12u, 4u, 14u, 6u,
        3u, 11u, 1u, 9u,
        15u, 7u, 13u, 5u,
    };
    return values[(pixel.y & 3u) * 4u + (pixel.x & 3u)];
}

float SampleStratifiedLobe(uint2 pixel, inout uint seed)
{
    uint stratum = (Bayer4x4Value(pixel) + (FrameIndex * 5u)) & 15u;
    return (float(stratum) + RandomFloat(seed)) / 16.0;
}

float3 SampleCosineHemisphere(float3 normal, float2 sample)
{
    float radius = sqrt(sample.x);
    float angle = 6.28318530718 * sample.y;
    float2 disk = radius * float2(cos(angle), sin(angle));
    float z = sqrt(max(0.0, 1.0 - dot(disk, disk)));
    float3 tangent = normalize(abs(normal.z) < 0.999
        ? cross(float3(0, 0, 1), normal)
        : cross(float3(0, 1, 0), normal));
    float3 bitangent = cross(normal, tangent);
    return normalize(tangent * disk.x + bitangent * disk.y + normal * z);
}

float Schlick(float cosine, float etaRatio)
{
    float r0 = (1.0 - etaRatio) / (1.0 + etaRatio);
    r0 *= r0;
    return r0 + (1.0 - r0) * pow(1.0 - cosine, 5.0);
}

void CameraBasis(float yaw, float pitch, out float3 forward, out float3 right, out float3 up)
{
    Stage11CameraBasis(yaw, pitch, forward, right, up);
}

float2 ProjectToPreviousUv(float3 worldPosition, uint2 size)
{
    return Stage11ProjectToUv(
        worldPosition,
        PreviousCameraPosition,
        PreviousCameraYaw,
        PreviousCameraPitch,
        size);
}

float2 ProjectToCurrentUv(float3 worldPosition, uint2 size)
{
    return Stage11ProjectToUv(
        worldPosition,
        CameraPosition,
        CameraYaw,
        CameraPitch,
        size);
}

float DlssDeviceDepth(float3 worldPosition)
{
    float3 forward;
    float3 right;
    float3 up;
    CameraBasis(CameraYaw, CameraPitch, forward, right, up);
    float viewZ = dot(worldPosition - CameraPosition, forward);
    return Stage11DeviceDepthFromViewZ(viewZ);
}

float3 PreviousWorldPosition(float3 localPosition, InstanceGpu instanceData)
{
    float3x4 previousObjectToWorld = float3x4(
        instanceData.previousObjectToWorldRow0,
        instanceData.previousObjectToWorldRow1,
        instanceData.previousObjectToWorldRow2);
    return mul(previousObjectToWorld, float4(localPosition, 1.0));
}

float3x3 Inverse3x3(float3x3 value)
{
    float a = value[0][0], b = value[0][1], c = value[0][2];
    float d = value[1][0], e = value[1][1], f = value[1][2];
    float g = value[2][0], h = value[2][1], i = value[2][2];
    float determinant = a * (e * i - f * h)
        - b * (d * i - f * g)
        + c * (d * h - e * g);
    if (abs(determinant) <= 1.0e-8)
        return float3x3(1, 0, 0, 0, 1, 0, 0, 0, 1);
    float reciprocal = rcp(determinant);
    return float3x3(
        e * i - f * h, c * h - b * i, b * f - c * e,
        f * g - d * i, a * i - c * g, c * d - a * f,
        d * h - e * g, b * g - a * h, a * e - b * d) * reciprocal;
}

float3 PreviousWorldNormal(float3 currentWorldNormal, InstanceGpu instanceData)
{
    // Recover the object-space shading normal from the current transform, then
    // apply the previous inverse-transpose. This covers rigid and non-uniform
    // instance animation without pretending the current normal is historical.
    float3 localNormal = mul(currentWorldNormal, (float3x3)ObjectToWorld3x4());
    float3x3 previousObjectToWorld = float3x3(
        instanceData.previousObjectToWorldRow0.xyz,
        instanceData.previousObjectToWorldRow1.xyz,
        instanceData.previousObjectToWorldRow2.xyz);
    float3 previousNormal = mul(localNormal, Inverse3x3(previousObjectToWorld));
    float lengthSquared = dot(previousNormal, previousNormal);
    return lengthSquared > 1.0e-10 && all(isfinite(previousNormal))
        ? previousNormal * rsqrt(lengthSquared)
        : currentWorldNormal;
}

struct RrSpecularGuide
{
    float hitDistance;
    float2 motion;
};

float4 MakeMirrorPlane(float3 normal, float3 planePoint);
float3 ReflectPointAcrossPlane(float4 plane, float3 position);

// Trace a deterministic dominant-reflection ray solely for RR guidance. The
// same committed triangle/barycentrics are transformed by current and previous
// instance transforms, then unfolded across the corresponding primary tangent
// planes. Projecting those virtual points yields explicit reflected-geometry
// motion. The query never contributes radiance, so it cannot bias the path
// estimator, and a static camera/scene produces zero motion even while sampling
// jitter moves the primary ray within the pixel.
RrSpecularGuide TraceRrSpecularGuide(
    float3 primaryHitPosition,
    float3 previousPrimaryHitPosition,
    float3 primaryNormal,
    float3 previousPrimaryNormal,
    float3 incomingDirection,
    float2 fallbackMotion,
    uint2 renderSize)
{
    RrSpecularGuide guide;
    guide.hitDistance = 0.0;
    guide.motion = fallbackMotion;
    float3 guideDirection = normalize(reflect(incomingDirection, primaryNormal));
    if (dot(primaryNormal, guideDirection) <= 0.0)
        return guide;

    RayDesc guideRay;
    guideRay.Origin = primaryHitPosition + primaryNormal * 0.002;
    guideRay.Direction = guideDirection;
    guideRay.TMin = 0.001;
    guideRay.TMax = 1000.0;

    RayQuery<RAY_FLAG_CULL_BACK_FACING_TRIANGLES | RAY_FLAG_FORCE_OPAQUE> query;
    query.TraceRayInline(Scene, RAY_FLAG_NONE, 0xFF, guideRay);
    while (query.Proceed())
    {
    }
    if (query.CommittedStatus() != COMMITTED_TRIANGLE_HIT)
        return guide;

    uint instanceId = query.CommittedInstanceID();
    InstanceGpu guideInstance = Instances[instanceId];
    uint primitive = query.CommittedPrimitiveIndex();
    uint3 triangleIndices = uint3(
        Indices[guideInstance.indexOffset + primitive * 3u],
        Indices[guideInstance.indexOffset + primitive * 3u + 1u],
        Indices[guideInstance.indexOffset + primitive * 3u + 2u]);
    float2 committedBarycentrics = query.CommittedTriangleBarycentrics();
    float3 barycentrics = float3(
        1.0 - committedBarycentrics.x - committedBarycentrics.y,
        committedBarycentrics.x,
        committedBarycentrics.y);
    float3 localGuideHit =
        Vertices[guideInstance.vertexOffset + triangleIndices.x].position * barycentrics.x
        + Vertices[guideInstance.vertexOffset + triangleIndices.y].position * barycentrics.y
        + Vertices[guideInstance.vertexOffset + triangleIndices.z].position * barycentrics.z;
    float3 currentGuideHit = mul(
        query.CommittedObjectToWorld3x4(),
        float4(localGuideHit, 1.0));
    float3 previousGuideHit = PreviousWorldPosition(localGuideHit, guideInstance);

    float3 currentVirtualHit = ReflectPointAcrossPlane(
        MakeMirrorPlane(primaryNormal, primaryHitPosition),
        currentGuideHit);
    float3 previousVirtualHit = ReflectPointAcrossPlane(
        MakeMirrorPlane(previousPrimaryNormal, previousPrimaryHitPosition),
        previousGuideHit);
    float2 currentUv = ProjectToCurrentUv(currentVirtualHit, renderSize);
    float2 previousUv = ProjectToPreviousUv(previousVirtualHit, renderSize);
    float2 reflectedMotion = (previousUv - currentUv) * float2(renderSize);
    if (all(isfinite(reflectedMotion)))
        guide.motion = reflectedMotion;
    guide.hitDistance = query.CommittedRayT();
    return guide;
}

float4 SampleMaterialTexture(uint textureAndSampler, float2 uv)
{
    const uint textureViewMask = (1u << 7u) - 1u;
    const uint samplerIndexMask = (1u << 6u) - 1u;
    uint textureView = textureAndSampler & textureViewMask;
    uint samplerIndex = (textureAndSampler >> 7u) & samplerIndexMask;
    return MaterialTextures[NonUniformResourceIndex(textureView)]
        .SampleLevel(MaterialSamplers[NonUniformResourceIndex(samplerIndex)], uv, 0.0);
}

static const float PI = 3.14159265359;

float3 FresnelSchlick(float cosine, float3 f0)
{
    return f0 + (1.0 - f0) * pow(1.0 - saturate(cosine), 5.0);
}

float D_GGX(float NoH, float roughness)
{
    float alpha = roughness * roughness;
    float alphaSquared = alpha * alpha;
    float denominator = NoH * NoH * (alphaSquared - 1.0) + 1.0;
    return alphaSquared / max(PI * denominator * denominator, 1.0e-7);
}

float G_SchlickGGX(float NoX, float roughness)
{
    float k = (roughness + 1.0) * (roughness + 1.0) / 8.0;
    return NoX / max(NoX * (1.0 - k) + k, 1.0e-6);
}

float G_Smith(float NoV, float NoL, float roughness)
{
    return G_SchlickGGX(NoV, roughness) * G_SchlickGGX(NoL, roughness);
}

// Isotropic bounded GGX VNDF v3 PDF. This follows the public bounded-VNDF
// formulation also used by the locked NVIDIA MathLib ML_VNDF_VERSION=3.
float PdfGgxVndfV3(float NoV, float roughness, float distribution)
{
    float alpha = roughness * roughness;
    float alphaSquared = alpha * alpha;
    float viewTangentLength = sqrt(max(0.0, 1.0 - NoV * NoV));
    float stretchedTangentSquared = alphaSquared * viewTangentLength * viewTangentLength;
    float stretchedLength = sqrt(stretchedTangentSquared + NoV * NoV);
    float scale = 1.0 + viewTangentLength;
    float scaleSquared = scale * scale;
    float bound = (1.0 - alphaSquared) * scaleSquared
        / max(scaleSquared + alphaSquared * NoV * NoV, 1.0e-6);
    return 0.5 * distribution / max(bound * NoV + stretchedLength, 1.0e-6);
}

struct BrdfEvaluation
{
    float3 diffuse;
    float3 specular;
    float diffusePdf;
    float specularPdf;
};

BrdfEvaluation EvaluateBrdf(
    float3 normal,
    float3 viewDirection,
    float3 lightDirection,
    float3 baseColor,
    float metallic,
    float roughness)
{
    BrdfEvaluation value;
    value.diffuse = 0;
    value.specular = 0;
    value.diffusePdf = 0;
    value.specularPdf = 0;
    float NoV = saturate(dot(normal, viewDirection));
    float NoL = saturate(dot(normal, lightDirection));
    if (NoV <= 0.0 || NoL <= 0.0)
        return value;

    float3 halfVector = normalize(viewDirection + lightDirection);
    float NoH = saturate(dot(normal, halfVector));
    float VoH = saturate(dot(viewDirection, halfVector));
    float3 f0 = lerp(0.04.xxx, baseColor, metallic);
    float3 fresnel = FresnelSchlick(VoH, f0);
    float distribution = D_GGX(NoH, roughness);
    float geometry = G_Smith(NoV, NoL, roughness);
    value.specular = distribution * geometry * fresnel
        / max(4.0 * NoV * NoL, 1.0e-6);
    value.diffuse = (1.0 - metallic) * (1.0 - fresnel) * baseColor / PI;
    value.diffusePdf = NoL / PI;
    value.specularPdf = PdfGgxVndfV3(NoV, roughness, distribution);
    return value;
}

float ComputeSpecularProbability(float3 baseColor, float metallic, bool useNrdProbabilisticLobe)
{
    float3 f0 = lerp(0.04.xxx, baseColor, metallic);
    float specularEnergy = max(max(f0.x, f0.y), f0.z);
    float diffuseEnergy = max(max(baseColor.x, baseColor.y), baseColor.z) * (1.0 - metallic);
    if (diffuseEnergy <= 1.0e-6)
        return specularEnergy > 1.0e-6 ? 1.0 : 0.0;
    if (specularEnergy <= 1.0e-6)
        return 0.0;
    float minimumProbability = useNrdProbabilisticLobe ? 0.25 : 0.05;
    return clamp(
        specularEnergy / (specularEnergy + diffuseEnergy),
        minimumProbability,
        1.0 - minimumProbability);
}

// This is the split-sum EnvBRDF approximation used for the reconstruction
// material factor, rather than a bare F0. It keeps the view-angle and
// roughness dependence required by the reconstruction contract. The exact
// backend will use the same material-factor convention when it unpacks.
float3 ComputeReconstructionSpecularAlbedo(float3 f0, float roughness, float NoV)
{
    float4 c0 = float4(-1.0, -0.0275, -0.572, 0.022);
    float4 c1 = float4(1.0, 0.0425, 1.04, -0.04);
    float4 r = roughness * c0 + c1;
    float a004 = min(r.x * r.x, exp2(-9.28 * NoV)) * r.x + r.y;
    float2 ab = float2(-1.04, 1.04) * a004 + r.zw;
    return max(f0 * ab.x + ab.y, 0.0.xxx);
}

void ComputeStableNrdMaterialFactors(
    float3 baseColor,
    float metallic,
    float roughness,
    float NoV,
    out float3 diffuseFactor,
    out float3 specularFactor)
{
    // Locked to NRD 4.17.3 NRD_MaterialFactors. The stable record must carry
    // exactly the factors used for front-end demodulation and back-end restore;
    // the older EnvBRDF approximation is intentionally not mixed into REBLUR.
    float m = saturate(roughness * roughness);
    float4 x = float4(1.0, NoV, NoV * NoV, NoV * NoV * NoV);
    float4 y = float4(1.0, m, m * m, m * m * m);
    const float2x2 m1 = float2x2(0.99044, -1.28514, 1.29678, -0.755907);
    const float3x3 m2 = float3x3(
        1.0, 2.92338, 59.4188,
        20.3225, -27.0302, 222.592,
        121.563, 626.13, 316.627);
    const float2x2 m3 = float2x2(0.0365463, 3.32707, 9.0632, -9.04756);
    const float3x3 m4 = float3x3(
        1.0, 3.59685, -1.36772,
        9.04401, -16.3174, 9.22949,
        5.56589, 19.7886, -20.2123);
    float bias = dot(mul(m1, x.xy), y.xy)
        / max(dot(mul(m2, x.xyw), y.xyw), 1.0e-6);
    float scale = dot(mul(m3, x.xy), y.xy)
        / max(dot(mul(m4, x.xzw), y.xyw), 1.0e-6);
    float3 f0 = lerp(0.04.xxx, baseColor, metallic);
    float3 environmentFresnel = saturate(f0 * scale + bias);
    float3 diffuseAlbedo = baseColor * (1.0 - metallic);
    diffuseFactor = lerp(0.02.xxx, 1.0.xxx, (1.0 - environmentFresnel) * diffuseAlbedo);
    specularFactor = environmentFresnel * lerp(0.1, 1.0, roughness);
    specularFactor = lerp(0.02.xxx, 1.0.xxx, specularFactor);
}

float3 FiniteNonNegative(float3 value)
{
    return all(isfinite(value)) ? max(value, 0.0.xxx) : 0.0.xxx;
}

void WriteStablePlaneGuides(
    Payload payload,
    float3 hitPosition,
    float3 previousHitPosition,
    float3 normal,
    float3 baseColor,
    float metallic,
    float roughness,
    uint kind,
    float3 emissive)
{
    uint2 pixel = DispatchRaysIndex().xy;
    uint2 extent = DispatchRaysDimensions().xy;
    uint address = StablePlaneAddress(pixel, StablePlaneIndex, extent);
    StablePlaneRecord restart = StablePlaneRecords[address];
    float sceneLength = restart.data1.w + RayTCurrent();
    float3 primaryDirection = DecodeStableDirection(restart.data3.zw);
    float3 virtualPosition = CameraPosition + primaryDirection * sceneLength;
    float3 previousVirtualPosition = virtualPosition + previousHitPosition - hitPosition;
    float2 currentUv = ProjectToCurrentUv(virtualPosition, extent);
    float2 previousUv = ProjectToPreviousUv(previousVirtualPosition, extent);

    float3 cameraForward;
    float3 cameraRight;
    float3 cameraUp;
    CameraBasis(CameraYaw, CameraPitch, cameraForward, cameraRight, cameraUp);
    float3 previousCameraForward;
    float3 previousCameraRight;
    float3 previousCameraUp;
    CameraBasis(
        PreviousCameraYaw,
        PreviousCameraPitch,
        previousCameraForward,
        previousCameraRight,
        previousCameraUp);
    float viewZ = dot(virtualPosition - CameraPosition, cameraForward);
    float previousViewZ = dot(
        previousVirtualPosition - PreviousCameraPosition,
        previousCameraForward);
    float3 motion = ResetHistory != 0u
        ? 0.0.xxx
        : float3(
            (previousUv - currentUv) * float2(extent),
            previousViewZ - viewZ);
    float3 viewDirection = normalize(-WorldRayDirection());
    float NoV = saturate(dot(normal, viewDirection));
    float3 diffuseFactor;
    float3 specularFactor;
    ComputeStableNrdMaterialFactors(
        baseColor,
        metallic,
        roughness,
        NoV,
        diffuseFactor,
        specularFactor);

    StablePlaneRecord guide;
    guide.data0 = float4(normal, roughness);
    guide.data1 = float4(FiniteNonNegative(diffuseFactor), viewZ);
    guide.data2 = float4(FiniteNonNegative(specularFactor), float(kind));
    guide.data3 = float4(motion, asfloat(PackStableHdr(
        payload.psrThroughput * FiniteNonNegative(emissive))));
    StablePlaneRecords[address] = guide;
    uint dominantPlane = min(
        uint(round(StableRadiance[pixel].w)),
        STABLE_PLANE_COUNT - 1u);
    if (StablePlaneIndex == dominantPlane)
    {
        float3 f0 = lerp(0.04.xxx, baseColor, metallic);
        StableDiffuseAlbedo[pixel] = float4(
            FiniteNonNegative(baseColor * (1.0 - metallic)), 1.0);
        StableSpecularAlbedo[pixel] = float4(
            FiniteNonNegative(ComputeReconstructionSpecularAlbedo(
                f0, roughness, NoV)),
            1.0);
    }
}

float4 MakeMirrorPlane(float3 normal, float3 planePoint)
{
    // Reflecting geometry across the mirror plane unfolds the reflected ray
    // into a straight camera ray. XYZ is the unit plane normal and W is the
    // signed plane distance in the n dot x = d convention.
    float3 n = normalize(normal);
    return float4(n, dot(n, planePoint));
}

float3 ReflectPointAcrossPlane(float4 plane, float3 position)
{
    return position - 2.0 * (dot(plane.xyz, position) - plane.w) * plane.xyz;
}

float3 ReflectVectorAcrossPlane(float4 plane, float3 vector)
{
    return vector - 2.0 * dot(plane.xyz, vector) * plane.xyz;
}

void WritePsrSurfaceGuides(
    Payload payload,
    float3 physicalPosition,
    float3 previousPhysicalPosition,
    float3 physicalNormal,
    float3 baseColor,
    float metallic,
    float roughness,
    float3 emissive,
    uint kind,
    uint stableSurfaceId)
{
    uint2 pixel = DispatchRaysIndex().xy;
    uint2 size = DispatchRaysDimensions().xy;
    float3 virtualPosition = ReflectPointAcrossPlane(payload.psrMirrorPlane, physicalPosition);
    // The current mirror transform is also applied to the previous physical
    // hit. Animated/rotating mirrors must reset history until a previous-frame
    // reflection transform is carried separately; static Cornell mirrors are
    // exact under this representation.
    float3 previousVirtualPosition = ReflectPointAcrossPlane(
        payload.psrMirrorPlane,
        previousPhysicalPosition);
    float3 virtualNormal = normalize(ReflectVectorAcrossPlane(
        payload.psrMirrorPlane,
        physicalNormal));
    if (dot(virtualNormal, CameraPosition - virtualPosition) < 0.0)
        virtualNormal = -virtualNormal;

    float2 currentUv = ProjectToCurrentUv(virtualPosition, size);
    float2 previousUv = ProjectToPreviousUv(previousVirtualPosition, size);
    float2 svgfMotion = ResetHistory != 0u
        ? 0
        : (currentUv - previousUv) * float2(size);

    float3 cameraForward;
    float3 cameraRight;
    float3 cameraUp;
    CameraBasis(CameraYaw, CameraPitch, cameraForward, cameraRight, cameraUp);
    float3 previousCameraForward;
    float3 previousCameraRight;
    float3 previousCameraUp;
    CameraBasis(
        PreviousCameraYaw,
        PreviousCameraPitch,
        previousCameraForward,
        previousCameraRight,
        previousCameraUp);
    float viewZ = dot(virtualPosition - CameraPosition, cameraForward);
    float previousViewZ = dot(
        previousVirtualPosition - PreviousCameraPosition,
        previousCameraForward);
    float3 viewDirection = normalize(CameraPosition - virtualPosition);
    float NoV = saturate(dot(virtualNormal, viewDirection));
    float3 f0 = lerp(0.04.xxx, baseColor, metallic);

    // Replace the complete application G-buffer only while NRD is active.
    // This keeps validation views coherent with the virtual reconstruction
    // surface without changing the feature-off/SVGF shading path.
    GBufferAlbedo[pixel] = float4(baseColor, float(kind));
    GBufferNormalRoughness[pixel] = float4(virtualNormal * 0.5 + 0.5, roughness);
    GBufferDepth[pixel] = length(virtualPosition - CameraPosition);
    GBufferMotion[pixel] = svgfMotion;
    GBufferId[pixel] = stableSurfaceId;
    GBufferWorldPosition[pixel] = float4(virtualPosition, 1.0);
    GBufferHitDistance[pixel] = 0.0;

    ReconstructionDiffuseAlbedo[pixel] = float4(
        FiniteNonNegative(baseColor * (1.0 - metallic)),
        metallic);
    ReconstructionSpecularAlbedo[pixel] = float4(
        FiniteNonNegative(ComputeReconstructionSpecularAlbedo(f0, roughness, NoV)),
        float(kind));
    ReconstructionNormalRoughness[pixel] = float4(
        virtualNormal * 0.5 + 0.5,
        roughness);
    ReconstructionViewZ[pixel] = viewZ > 0.0 && isfinite(viewZ) ? viewZ : 1001.0;
    ReconstructionMotion[pixel] = ResetHistory != 0u
        ? 0
        : float4(
            (previousUv - currentUv) * float2(size),
            previousViewZ - viewZ,
            0.0);
    ReconstructionPrimaryEmissive[pixel] = float4(
        FiniteNonNegative(payload.psrThroughput * emissive),
        1.0);
}

void WriteTransmissionSurfaceGuides(
    float3 hitPosition,
    float3 previousHitPosition,
    float3 normal,
    float3 viewDirection,
    float3 baseColor,
    float metallic,
    float roughness,
    uint kind)
{
    uint2 pixel = DispatchRaysIndex().xy;
    uint2 size = DispatchRaysDimensions().xy;
    float2 currentUv = ProjectToCurrentUv(hitPosition, size);
    float2 previousUv = ProjectToPreviousUv(previousHitPosition, size);

    float3 cameraForward;
    float3 cameraRight;
    float3 cameraUp;
    CameraBasis(CameraYaw, CameraPitch, cameraForward, cameraRight, cameraUp);
    float3 previousCameraForward;
    float3 previousCameraRight;
    float3 previousCameraUp;
    CameraBasis(
        PreviousCameraYaw,
        PreviousCameraPitch,
        previousCameraForward,
        previousCameraRight,
        previousCameraUp);
    float viewZ = dot(hitPosition - CameraPosition, cameraForward);
    float previousViewZ = dot(
        previousHitPosition - PreviousCameraPosition,
        previousCameraForward);

    TransmissionBaseColor[pixel] = float4(baseColor, float(kind));
    TransmissionNormalRoughness[pixel] = float4(normal * 0.5 + 0.5, roughness);
    TransmissionViewZ[pixel] = viewZ > 0.0 && isfinite(viewZ) ? viewZ : 1001.0;
    TransmissionMotion[pixel] = ResetHistory != 0u
        ? 0
        : float4(
            (previousUv - currentUv) * float2(size),
            previousViewZ - viewZ,
            0.0);
    TransmissionDiffuseAlbedo[pixel] = float4(
        FiniteNonNegative(baseColor * (1.0 - metallic)),
        metallic);
    // The shared NRD prep derives V from camera-position minus this value.
    // Refraction changes V, so a synthetic point one unit down the actual
    // incoming ray preserves the correct BSDF material factor without adding
    // another full-resolution guide solely for view direction.
    TransmissionViewProxy[pixel] = float4(
        CameraPosition - normalize(viewDirection),
        1.0);
}

void InitializeDeterministicGlassChild(
    Payload parent,
    uint firstKind,
    uint branchOffset,
    out Payload child)
{
    child.radiance = 0;
    child.seed = HashUint(parent.seed ^ (0x9E3779B9u + branchOffset));
    child.depth = parent.depth + 1u;
    child.lastPdf = 0;
    child.firstKind = firstKind;
    child.hitDistance = 0;
    child.rawDiffuse = 0;
    child.rawSpecular = 0;
    child.psrActive = 0;
    // Reflection and refraction must not replay identical secondary samples.
    // A disjoint Sobol dimension range preserves temporal low discrepancy
    // without introducing a second random-number implementation.
    child.sampleDimensionOffset = parent.sampleDimensionOffset + branchOffset;
    child.psrThroughput = 0;
    child.psrMirrorPlane = 0;
}

float MinimumValidSpecularHitDistance(float first, float second)
{
    // This mirrors NRD_FrontEnd_SpecHitDistAveraging: for multiple specular
    // paths, tracking uses the nearest non-zero in-lobe hit rather than a
    // radiance/PDF-weighted distance that would move independently of geometry.
    if (first <= 0.0)
        return max(second, 0.0);
    if (second <= 0.0)
        return max(first, 0.0);
    return min(first, second);
}

float3 SampleGgxVndfDirection(
    float3 normal,
    float3 viewDirection,
    float roughness,
    float2 sample)
{
    // Trim the lowest-probability 5% tail as recommended by the NRD sample;
    // this reduces denoiser-hostile grazing fireflies. The existing geometric
    // normal check still rejects any remaining below-surface direction.
    sample.y *= 0.95;
    float alpha = roughness * roughness;
    float3 tangent = normalize(abs(normal.z) < 0.999
        ? cross(float3(0, 0, 1), normal)
        : cross(float3(0, 1, 0), normal));
    float3 bitangent = cross(normal, tangent);
    float3 viewLocal = float3(
        dot(viewDirection, tangent),
        dot(viewDirection, bitangent),
        max(dot(viewDirection, normal), 1.0e-6));
    float3 stretchedView = normalize(float3(alpha * viewLocal.xy, viewLocal.z));
    float phi = 2.0 * PI * sample.x;
    float viewTangentLength = length(viewLocal.xy);
    float scale = 1.0 + viewTangentLength;
    float alphaSquared = alpha * alpha;
    float scaleSquared = scale * scale;
    float bound = (1.0 - alphaSquared) * scaleSquared
        / max(scaleSquared + alphaSquared * viewLocal.z * viewLocal.z, 1.0e-6);
    float lowerBound = viewLocal.z > 0.0 ? bound * stretchedView.z : stretchedView.z;
    float diskZ = 1.0 - sample.y * (1.0 + lowerBound);
    float diskRadius = sqrt(max(0.0, 1.0 - diskZ * diskZ));
    float3 visibleNormal = float3(diskRadius * cos(phi), diskRadius * sin(phi), diskZ)
        + stretchedView;
    float3 halfLocal = normalize(float3(alpha * visibleNormal.xy, max(visibleNormal.z, 1.0e-6)));
    float3 halfVector = normalize(
        tangent * halfLocal.x + bitangent * halfLocal.y + normal * halfLocal.z);
    return normalize(reflect(-viewDirection, halfVector));
}

[shader("raygeneration")]
void RayGen()
{
    uint2 pixel = DispatchRaysIndex().xy;
    uint2 size = DispatchRaysDimensions().xy;
    uint seed = pixel.x * 1973u + pixel.y * 9277u + FrameIndex * 26699u + 89173u;
    float2 primarySampleOffset = PrimaryRaySampleOffset(pixel);
    float2 uv = (float2(pixel) + primarySampleOffset + CameraJitterPx) / float2(size);
    float2 screen = uv * 2.0 - 1.0;
    screen.x *= float(size.x) / float(size.y);

    float3 forward;
    float3 right;
    float3 up;
    CameraBasis(CameraYaw, CameraPitch, forward, right, up);

    RayDesc ray;
    ray.Origin = CameraPosition;
    ray.Direction = normalize(
        forward * STAGE11_CAMERA_FOCAL_LENGTH + right * screen.x - up * screen.y);
    ray.TMin = 0.001;
    ray.TMax = 1000.0;

    Payload payload;
    payload.radiance = 0;
    payload.seed = seed;
    payload.depth = 0;
    payload.lastPdf = 0;
    payload.firstKind = 0;
    payload.hitDistance = 0;
    payload.rawDiffuse = 0;
    payload.rawSpecular = 0;
    payload.psrActive = 0;
    payload.sampleDimensionOffset = 0;
    payload.psrThroughput = 0;
    payload.psrMirrorPlane = 0;

    GBufferAlbedo[pixel] = 0;
    GBufferNormalRoughness[pixel] = 0;
    GBufferDepth[pixel] = 0;
    GBufferMotion[pixel] = 0;
    GBufferId[pixel] = 0xFFFFFFFFu;
    GBufferWorldPosition[pixel] = 0;
    GBufferHitDistance[pixel] = 0;
    if (DlssGuideMode != 0u)
    {
        DlssDepth[pixel] = 1.0;
        DlssMotion[pixel] = 0;
        if (DlssGuideMode == 2u)
            DlssSpecularMotion[pixel] = 0;
    }
    ReconstructionNoisyHdr[pixel] = 0;
    ReconstructionDiffuseAlbedo[pixel] = 0;
    ReconstructionSpecularAlbedo[pixel] = 0;
    ReconstructionNormalRoughness[pixel] = 0;
    ReconstructionViewZ[pixel] = 1001.0;
    ReconstructionMotion[pixel] = 0;
    ReconstructionSpecularHitDistance[pixel] = 0;
    ReconstructionPrimaryEmissive[pixel] = 0;
    ReconstructionDiffuseHitDistance[pixel] = 0;
    if (NrdEnabled != 0u)
    {
        TransmissionRawDiffuse[pixel] = 0;
        TransmissionRawSpecular[pixel] = 0;
        // Alpha -1 is an explicit invalid-layer marker. Opaque material class
        // zero is valid, so a zero-cleared alpha cannot double as presence.
        TransmissionBaseColor[pixel] = float4(0, 0, 0, -1);
        TransmissionNormalRoughness[pixel] = float4(0.5, 0.5, 1.0, 1.0);
        TransmissionViewZ[pixel] = 1001.0;
        TransmissionMotion[pixel] = 0;
        TransmissionDiffuseHitDistance[pixel] = 0;
        TransmissionSpecularHitDistance[pixel] = 0;
        TransmissionPrimaryEmissive[pixel] = 0;
        TransmissionDiffuseAlbedo[pixel] = 0;
        TransmissionViewProxy[pixel] = float4(CameraPosition, 1.0);
    }
    TraceRay(
        Scene,
        RAY_FLAG_CULL_BACK_FACING_TRIANGLES,
        0xFF,
        0,
        1,
        0,
        ray,
        payload);

    RawDiffuse[pixel] = float4(payload.rawDiffuse, 1.0);
    RawSpecular[pixel] = float4(payload.rawSpecular, 1.0);
}

[shader("raygeneration")]
void StableFillRayGen()
{
    uint2 pixel = DispatchRaysIndex().xy;
    uint2 extent = DispatchRaysDimensions().xy;
    // Plane fills run in reverse order. Clear the one-per-pixel dominant
    // guides in the first dispatch so any later valid dominant plane replaces
    // them without an extra full-screen clear pass.
    if (StablePlaneIndex + 1u == STABLE_PLANE_COUNT)
    {
        StableDiffuseAlbedo[pixel] = 0.0;
        StableSpecularAlbedo[pixel] = 0.0;
    }
    uint branchId = StablePlaneHeaders[uint3(pixel, StablePlaneIndex)];
    if (branchId == STABLE_BRANCH_INVALID)
        return;

    uint address = StablePlaneAddress(pixel, StablePlaneIndex, extent);
    StablePlaneRecord restart = StablePlaneRecords[address];
    float3 planeThroughput = restart.data2.xyz;
    RayDesc ray;
    ray.Origin = restart.data0.xyz;
    ray.Direction = normalize(restart.data1.xyz);
    ray.TMin = 0.001;
    ray.TMax = max(restart.data0.w + 0.01, 0.002);

    Payload payload;
    payload.radiance = 0.0;
    payload.seed = HashUint(
        pixel.x * 1973u
        + pixel.y * 9277u
        + FrameIndex * 26699u
        + branchId * 104729u);
    payload.depth = 0u;
    payload.lastPdf = 0.0;
    payload.firstKind = 0u;
    payload.hitDistance = 0.0;
    payload.rawDiffuse = 0.0;
    payload.rawSpecular = 0.0;
    payload.psrActive = 0u;
    payload.sampleDimensionOffset = (branchId & 0xffu) * 64u;
    // The closest-hit shader publishes the plane guide before recursive
    // shading mutates the legacy PSR fields.
    payload.psrThroughput = planeThroughput;
    payload.psrMirrorPlane = 0.0;

    TraceRay(
        Scene,
        RAY_FLAG_CULL_BACK_FACING_TRIANGLES,
        0xff,
        0,
        1,
        0,
        ray,
        payload);

    StablePlaneNoisyDiffuse[uint3(pixel, StablePlaneIndex)] = float4(
        FiniteNonNegative(planeThroughput * payload.rawDiffuse),
        payload.lastPdf);
    StablePlaneNoisySpecular[uint3(pixel, StablePlaneIndex)] = float4(
        FiniteNonNegative(planeThroughput * payload.rawSpecular),
        payload.hitDistance);
}

[shader("miss")]
void Miss(inout Payload payload)
{
    payload.radiance = 0;
    payload.hitDistance = 0;
}

[shader("miss")]
void ShadowMiss(inout Payload payload)
{
    payload.radiance = 1.0;
}

[shader("closesthit")]
void ClosestHit(inout Payload payload, in BuiltInTriangleIntersectionAttributes attributes)
{
    uint primitive = PrimitiveIndex();
    InstanceGpu instanceData = Instances[InstanceID()];
    uint3 triIndices = uint3(
        Indices[instanceData.indexOffset + primitive * 3],
        Indices[instanceData.indexOffset + primitive * 3 + 1],
        Indices[instanceData.indexOffset + primitive * 3 + 2]);
    float3 barycentric = float3(
        1.0 - attributes.barycentrics.x - attributes.barycentrics.y,
        attributes.barycentrics.x,
        attributes.barycentrics.y);
    uint vertex0 = instanceData.vertexOffset + triIndices.x;
    uint vertex1 = instanceData.vertexOffset + triIndices.y;
    uint vertex2 = instanceData.vertexOffset + triIndices.z;
    float3 localPosition = Vertices[vertex0].position * barycentric.x
        + Vertices[vertex1].position * barycentric.y
        + Vertices[vertex2].position * barycentric.z;
    float3 localNormal = normalize(
        Vertices[vertex0].normal * barycentric.x
        + Vertices[vertex1].normal * barycentric.y
        + Vertices[vertex2].normal * barycentric.z);
    float4 localTangent = Vertices[vertex0].tangent * barycentric.x
        + Vertices[vertex1].tangent * barycentric.y
        + Vertices[vertex2].tangent * barycentric.z;
    float2 texcoord0 = Vertices[vertex0].texcoord0 * barycentric.x
        + Vertices[vertex1].texcoord0 * barycentric.y
        + Vertices[vertex2].texcoord0 * barycentric.z;

    uint materialIndex = instanceData.materialIndex;
    Material material = Materials[materialIndex];
    float3 geometricNormal = normalize(mul(localNormal, (float3x3)WorldToObject3x4()));
    bool frontFace = HitKind() == HIT_KIND_TRIANGLE_FRONT_FACE;
    bool doubleSided = (material.flags & 1u) != 0u
        || (material.flags & 4u) != 0u;
    float3 normal = frontFace ? geometricNormal : -geometricNormal;
    if (!frontFace && !doubleSided)
        return;
    float4 baseColor = material.baseColorFactor
        * SampleMaterialTexture(material.baseColorTextureAndSampler, texcoord0);
    float4 metallicRoughness = SampleMaterialTexture(
        material.metallicRoughnessTextureAndSampler,
        texcoord0);
    float3 emissive = material.emissiveFactor
        * SampleMaterialTexture(material.emissiveTextureAndSampler, texcoord0).xyz;
    float metallic = saturate(material.metallicFactor * metallicRoughness.b);
    float roughness = clamp(material.roughnessFactor * metallicRoughness.g, 0.045, 1.0);

    if ((material.flags & 2u) != 0u)
    {
        float3 tangent = normalize(mul(localTangent.xyz, (float3x3)ObjectToWorld3x4()));
        tangent = normalize(tangent - normal * dot(normal, tangent));
        float3 bitangent = normalize(cross(normal, tangent)) * localTangent.w;
        float3 tangentNormal = SampleMaterialTexture(
            material.normalTextureAndSampler,
            texcoord0).xyz * 2.0 - 1.0;
        tangentNormal.xy *= material.normalScale;
        normal = normalize(
            tangent * tangentNormal.x + bitangent * tangentNormal.y + normal * tangentNormal.z);
        if (dot(normal, -WorldRayDirection()) < 0.0)
            normal = -normal;
    }

    bool legacyDielectric = (material.flags & 4u) != 0u;
    bool legacyMetal = (material.flags & 8u) != 0u;
    bool legacyEmissive = (material.flags & 16u) != 0u;
    uint kind = legacyDielectric ? 2u : (legacyMetal ? 1u : (legacyEmissive ? 3u : 0u));
    float3 hitPosition = mul(ObjectToWorld3x4(), float4(localPosition, 1.0));
    float3 previousHitPosition = PreviousWorldPosition(localPosition, instanceData);
    payload.hitDistance = RayTCurrent();
    bool isPsrSurface = NrdEnabled != 0u
        && payload.psrActive == 1u
        && kind != 1u;
    // Both reconstruction backends need a deterministic principal transmitted
    // path. NRD publishes a second REBLUR layer; RR keeps the complete glass
    // signal and pairs it with a stable virtual-transmission guide downstream.
    bool layeredTransmissionEnabled = NrdEnabled != 0u || DlssGuideMode == 2u;
    bool isTransmissionSurface = layeredTransmissionEnabled
        && payload.psrActive == 3u
        && kind != 2u;

    if (PathSpacePass == 1u && payload.depth == 0u)
        WriteStablePlaneGuides(
            payload,
            hitPosition,
            previousHitPosition,
            normal,
            baseColor.xyz,
            metallic,
            roughness,
            kind,
            emissive);

    if (isPsrSurface)
    {
        WritePsrSurfaceGuides(
            payload,
            hitPosition,
            previousHitPosition,
            normal,
            baseColor.xyz,
            metallic,
            roughness,
            emissive,
            kind,
            instanceData.stableSurfaceId);
    }
    if (NrdEnabled != 0u && isTransmissionSurface)
    {
        WriteTransmissionSurfaceGuides(
            hitPosition,
            previousHitPosition,
            normal,
            normalize(-WorldRayDirection()),
            baseColor.xyz,
            metallic,
            roughness,
            kind);
    }

    if (payload.depth == 0)
    {
        uint2 pixel = DispatchRaysIndex().xy;
        uint2 size = DispatchRaysDimensions().xy;
        payload.firstKind = kind;
        GBufferAlbedo[pixel] = float4(baseColor.xyz, float(kind));
        GBufferNormalRoughness[pixel] = float4(normal * 0.5 + 0.5, roughness);
        GBufferDepth[pixel] = RayTCurrent();
        GBufferId[pixel] = instanceData.stableSurfaceId;
        GBufferWorldPosition[pixel] = float4(hitPosition, 1.0);
        // Project the actual jittered primary hit through both non-jittered
        // cameras. Using the pixel center here would turn per-pixel ray jitter
        // into false motion even for a fully static scene.
        float2 currentUv = ProjectToCurrentUv(hitPosition, size);
        float2 previousUv = ProjectToPreviousUv(previousHitPosition, size);
        GBufferMotion[pixel] = ResetHistory != 0u
            ? 0
            : (currentUv - previousUv) * float2(size);
        if (DlssGuideMode != 0u)
        {
            DlssDepth[pixel] = DlssDeviceDepth(hitPosition);
            DlssMotion[pixel] = ResetHistory != 0u
                ? 0
                : (previousUv - currentUv) * float2(size);
            if (DlssGuideMode == 2u)
                DlssSpecularMotion[pixel] = DlssMotion[pixel];
        }

        float3 cameraForward;
        float3 cameraRight;
        float3 cameraUp;
        CameraBasis(CameraYaw, CameraPitch, cameraForward, cameraRight, cameraUp);
        float3 previousCameraForward;
        float3 previousCameraRight;
        float3 previousCameraUp;
        CameraBasis(
            PreviousCameraYaw,
            PreviousCameraPitch,
            previousCameraForward,
            previousCameraRight,
            previousCameraUp);
        float viewZ = dot(hitPosition - CameraPosition, cameraForward);
        float previousViewZ = dot(
            previousHitPosition - PreviousCameraPosition,
            previousCameraForward);
        float3 firstViewDirection = normalize(-WorldRayDirection());
        float NoV = saturate(dot(normal, firstViewDirection));
        float3 f0 = lerp(0.04.xxx, baseColor.xyz, metallic);
        ReconstructionNoisyHdr[pixel] = 0;
        // RGB is the shared diffuse albedo guide. Alpha has one explicit
        // consumer in the NRD adapter and stores metallic for reconstructing
        // the true dielectric/metal F0 without overloading GBufferAlbedo.a.
        ReconstructionDiffuseAlbedo[pixel] = float4(
            FiniteNonNegative(baseColor.xyz * (1.0 - metallic)),
            metallic);
        ReconstructionSpecularAlbedo[pixel] = float4(
            FiniteNonNegative(ComputeReconstructionSpecularAlbedo(f0, roughness, NoV)),
            1.0);
        ReconstructionNormalRoughness[pixel] = float4(normal * 0.5 + 0.5, roughness);
        ReconstructionViewZ[pixel] = viewZ > 0.0 && isfinite(viewZ) ? viewZ : 1001.0;
        ReconstructionMotion[pixel] = ResetHistory != 0u
            ? 0
            : float4(
                (previousUv - currentUv) * float2(size),
                previousViewZ - viewZ,
                0.0);
        ReconstructionPrimaryEmissive[pixel] = float4(FiniteNonNegative(emissive), 1.0);

        if (NrdEnabled != 0u && kind == 2u)
        {
            // Publish a stable full-glass coverage layer before tracing the
            // principal transmitted ray. A successful replacement hit will
            // overwrite these fallback guides. Keeping the layer present for
            // TIR/unresolved pixels prevents camera jitter from toggling the
            // post-denoise mask into a salt-and-pepper band.
            WriteTransmissionSurfaceGuides(
                hitPosition,
                previousHitPosition,
                normal,
                firstViewDirection,
                baseColor.xyz,
                metallic,
                roughness,
                kind);
        }
    }

    if (kind == 3u)
    {
        float weight = 1.0;
        if (payload.depth > 0 && payload.lastPdf > 0.0)
        {
            const float lightArea = 0.25;
            float lightPdf = RayTCurrent() * RayTCurrent()
                / max(0.0001, abs(dot(normal, -WorldRayDirection())) * lightArea);
            float bsdfSquared = payload.lastPdf * payload.lastPdf;
            weight = bsdfSquared / max(bsdfSquared + lightPdf * lightPdf, 1.0e-7);
        }
        payload.radiance = emissive * weight;
        if (isPsrSurface)
        {
            payload.rawDiffuse = 0;
            payload.rawSpecular = payload.psrThroughput * payload.radiance;
            payload.psrActive = 2u;
            ReconstructionNoisyHdr[DispatchRaysIndex().xy] = float4(
                FiniteNonNegative(payload.rawSpecular),
                1.0);
        }
        else if (isTransmissionSurface)
        {
            payload.rawDiffuse = 0;
            payload.rawSpecular = payload.psrThroughput * payload.radiance;
            payload.psrActive = 4u;
            if (NrdEnabled != 0u)
            {
                uint2 transmissionPixel = DispatchRaysIndex().xy;
                TransmissionRawDiffuse[transmissionPixel] = 0;
                TransmissionRawSpecular[transmissionPixel] = float4(
                    FiniteNonNegative(payload.rawSpecular),
                    1.0);
                TransmissionDiffuseHitDistance[transmissionPixel] = 0;
                TransmissionSpecularHitDistance[transmissionPixel] = 0;
                TransmissionPrimaryEmissive[transmissionPixel] = float4(
                    FiniteNonNegative(payload.rawSpecular),
                    1.0);
            }
        }
        else if (payload.depth == 0)
        {
            payload.rawSpecular = payload.radiance;
            ReconstructionNoisyHdr[DispatchRaysIndex().xy] = float4(
                FiniteNonNegative(payload.rawSpecular),
                1.0);
        }
        return;
    }
    if (payload.depth >= 3)
    {
        payload.radiance = kind == 0u ? emissive : 0;
        return;
    }

    float3 viewDirection = normalize(-WorldRayDirection());
    float3 directDiffuse = 0;
    float3 directSpecular = 0;
    float diffuseHitDistance = 0.0;
    float specularHitDistance = 0.0;
    // A PSR hit is the primary surface from NRD's point of view, even though it
    // appears at a later physical bounce. Apply the same lobe stratification
    // and in-lobe hit-distance rules there rather than at the hidden mirror.
    // NRD requires a single explicitly selected lobe because REBLUR consumes
    // split signals and reconstructs missing hit distances. RR consumes one
    // complete noisy HDR signal, so it retains the lower-variance mixture
    // estimator and receives reflection tracking through a separate guide.
    bool useNrdProbabilisticLobe = NrdEnabled != 0u
        && (payload.depth == 0u || isPsrSurface);
    if (kind == 0u)
    {
        // Spend extra visibility rays only after a path starts in the
        // specular/transmission lobe. This targets mirror/refraction noise
        // without multiplying the full-screen primary path budget.
        uint lightSampleCount = payload.depth > 0u && payload.firstKind != 0u ? 4u : 1u;
        for (uint lightSampleIndex = 0u; lightSampleIndex < lightSampleCount; ++lightSampleIndex)
        {
            float2 lightRandom = SampleOwenSobol2D(
                DispatchRaysIndex().xy,
                payload.sampleDimensionOffset
                    + 2u + payload.depth * 8u + lightSampleIndex * 32u);
            float3 lightPoint = float3(
                -0.25 + lightRandom.x * 0.5,
                0.9966667,
                0.6666667 + lightRandom.y * 0.5);
            float3 toLight = lightPoint - hitPosition;
            float lightDistance = length(toLight);
            float3 lightDirection = toLight / max(lightDistance, 1.0e-6);
            float NoL = max(0.0, dot(normal, lightDirection));
            float lightCosine = max(0.0, dot(float3(0, -1, 0), -lightDirection));
            if (NoL > 0.0 && lightCosine > 0.0)
            {
                Payload shadow;
                shadow.radiance = 0;
                shadow.seed = payload.seed;
                shadow.depth = payload.depth;
                shadow.lastPdf = 0;
                shadow.firstKind = 0;
                shadow.hitDistance = 0;
                shadow.rawDiffuse = 0;
                shadow.rawSpecular = 0;
                RayDesc shadowRay;
                shadowRay.Origin = hitPosition + normal * 0.002;
                shadowRay.Direction = lightDirection;
                shadowRay.TMin = 0.001;
                shadowRay.TMax = lightDistance - 0.004;
                TraceRay(
                    Scene,
                    RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH
                        | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER
                        | RAY_FLAG_CULL_BACK_FACING_TRIANGLES,
                    0xFF,
                    0,
                    1,
                    1,
                    shadowRay,
                    shadow);
                BrdfEvaluation brdf = EvaluateBrdf(
                    normal,
                    viewDirection,
                    lightDirection,
                    baseColor.xyz,
                    metallic,
                    roughness);
                float specularProbability = ComputeSpecularProbability(
                    baseColor.xyz,
                    metallic,
                    useNrdProbabilisticLobe);
                float diffuseBsdfPdf = (1.0 - specularProbability) * brdf.diffusePdf;
                float specularBsdfPdf = specularProbability * brdf.specularPdf;
                float bsdfPdf = diffuseBsdfPdf + specularBsdfPdf;
                float lightPdf = lightDistance * lightDistance
                    / max(lightCosine * 0.25, 1.0e-6);
                float lightSquared = lightPdf * lightPdf;
                float bsdfSquared = bsdfPdf * bsdfPdf;
                float misWeight = lightSquared / max(lightSquared + bsdfSquared, 1.0e-7);
                float diffuseMisWeight = useNrdProbabilisticLobe
                    ? lightSquared / max(lightSquared + diffuseBsdfPdf * diffuseBsdfPdf, 1.0e-7)
                    : misWeight;
                float specularMisWeight = useNrdProbabilisticLobe
                    ? lightSquared / max(lightSquared + specularBsdfPdf * specularBsdfPdf, 1.0e-7)
                    : misWeight;
                float3 lightRadiance = Materials[3].emissiveFactor;
                float visibility = shadow.radiance;
                float3 diffuseSampleRadiance = visibility * lightRadiance * brdf.diffuse * NoL
                    * diffuseMisWeight / max(lightPdf, 1.0e-6);
                float3 specularSampleRadiance = visibility * lightRadiance * brdf.specular * NoL
                    * specularMisWeight / max(lightPdf, 1.0e-6);
                directDiffuse += diffuseSampleRadiance / float(lightSampleCount);
                directSpecular += specularSampleRadiance / float(lightSampleCount);
                if (any(diffuseSampleRadiance > 0.0))
                    diffuseHitDistance = lightDistance;
                // A sampled light is not necessarily inside the specular
                // lobe.  Feeding its changing distance to NRD/RR breaks
                // reflected-geometry reprojection.  Reconstruction paths use
                // an indirect in-lobe distance below; legacy paths retain the
                // old direct-light distance because they do not expose it to
                // a reflection tracker.
                if (any(specularSampleRadiance > 0.0)
                    && NrdEnabled == 0u
                    && DlssGuideMode != 2u)
                    specularHitDistance = lightDistance;
            }
        }
    }

    float3 direction = 0;
    float3 bounceWeight = 0;
    float3 diffuseBounceWeight = 0;
    float3 specularBounceWeight = 0;
    float samplePdf = 1.0;
    bool sampledSpecular = kind == 1u || kind == 2u;
    bool sampledTransmission = false;
    // The first entry/exit pair of a camera-visible dielectric is a tiny part
    // of the screen in the target scene, so tracing both Fresnel branches is a
    // better quality/cost trade than feeding Bernoulli noise into REBLUR. The
    // depth bound prevents exponential branching in nested glass geometry.
    bool deterministicGlass = layeredTransmissionEnabled
        && kind == 2u
        && (payload.depth == 0u
            || isPsrSurface
            || (payload.firstKind == 2u && payload.depth <= 1u));
    if (deterministicGlass)
    {
        float etaRatio = frontFace
            ? (1.0 / max(material.ior, 1.0001))
            : max(material.ior, 1.0001);
        float cosine = saturate(dot(-WorldRayDirection(), normal));
        float fresnel = Schlick(cosine, etaRatio);
        float3 reflectionDirection = normalize(reflect(WorldRayDirection(), normal));
        float3 refractionDirection = refract(WorldRayDirection(), normal, etaRatio);
        bool totalInternalReflection = length(refractionDirection) < 0.001;

        uint firstPathKind = payload.depth == 0u ? 2u : payload.firstKind;
        float reflectionWeight = totalInternalReflection ? 1.0 : fresnel;
        float refractionWeight = 1.0 - reflectionWeight;
        bool transmissionExit = payload.psrActive == 3u;

        Payload reflectionChild;
        InitializeDeterministicGlassChild(payload, firstPathKind, 0u, reflectionChild);
        if (!transmissionExit)
        {
            // Glass reflection needs the same virtual replacement surface as a
            // metal mirror. Without it, the ceiling/light reflection is still
            // filtered using the glass interface depth and remains grainy.
            if (NrdEnabled != 0u && payload.depth == 0u && payload.psrActive == 0u)
            {
                reflectionChild.psrActive = 1u;
                reflectionChild.psrThroughput = baseColor.xyz * reflectionWeight;
                reflectionChild.psrMirrorPlane = MakeMirrorPlane(normal, hitPosition);
            }
            RayDesc reflectionRay;
            reflectionRay.Origin = hitPosition + reflectionDirection * 0.002;
            reflectionRay.Direction = reflectionDirection;
            reflectionRay.TMin = 0.001;
            reflectionRay.TMax = 1000.0;
            TraceRay(
                Scene,
                RAY_FLAG_CULL_BACK_FACING_TRIANGLES,
                0xFF,
                0,
                1,
                0,
                reflectionRay,
                reflectionChild);
        }

        Payload refractionChild;
        InitializeDeterministicGlassChild(payload, firstPathKind, 64u, refractionChild);
        if (!totalInternalReflection)
        {
            // Only the geometrically transmitted branch owns the replacement
            // layer. Internal reflection remains in the primary residual; this
            // prevents two internal paths from racing to publish one guide set.
            if (payload.depth == 0u && payload.psrActive == 0u)
            {
                refractionChild.psrActive = 3u;
                refractionChild.psrThroughput = baseColor.xyz * refractionWeight;
            }
            else if (payload.psrActive == 3u)
            {
                refractionChild.psrActive = 3u;
                refractionChild.psrThroughput = payload.psrThroughput
                    * baseColor.xyz
                    * refractionWeight;
            }
            refractionDirection = normalize(refractionDirection);
            RayDesc refractionRay;
            refractionRay.Origin = hitPosition + refractionDirection * 0.002;
            refractionRay.Direction = refractionDirection;
            refractionRay.TMin = 0.001;
            refractionRay.TMax = 1000.0;
            TraceRay(
                Scene,
                RAY_FLAG_CULL_BACK_FACING_TRIANGLES,
                0xFF,
                0,
                1,
                0,
                refractionRay,
                refractionChild);
        }

        // This real-time glass model intentionally keeps two reconstructable
        // layers: first-interface reflection and the principal transmitted ray.
        // Higher-order internal reflections need additional depth/motion layers;
        // mixing them back into either layer recreates the very snow this split
        // is designed to remove. Their omitted energy is preferable to an
        // unstable, guide-incompatible estimator in the current renderer.
        float3 glassRadiance = transmissionExit
            ? baseColor.xyz * refractionWeight * refractionChild.radiance
            : baseColor.xyz * (
                reflectionWeight * reflectionChild.radiance
                + refractionWeight * refractionChild.radiance);
        float glassHitDistance = totalInternalReflection || transmissionExit
            ? reflectionChild.hitDistance
            : MinimumValidSpecularHitDistance(
                reflectionChild.hitDistance,
                refractionChild.hitDistance);
        if (transmissionExit)
            glassHitDistance = refractionChild.hitDistance;
        payload.seed = HashUint(reflectionChild.seed ^ refractionChild.seed);
        payload.radiance = FiniteNonNegative(glassRadiance);
        bool resolvedTransmission = refractionChild.psrActive == 4u;
        if (payload.psrActive == 3u && resolvedTransmission)
        {
            payload.rawDiffuse = refractionChild.rawDiffuse;
            payload.rawSpecular = refractionChild.rawSpecular;
            payload.psrActive = 4u;
        }
        else
        {
            bool resolvedReflection = reflectionChild.psrActive == 2u;
            float3 layeredTransmission = payload.depth == 0u && resolvedTransmission
                ? refractionChild.rawDiffuse + refractionChild.rawSpecular
                : 0;
            float3 layeredReflection = resolvedReflection
                ? reflectionChild.rawDiffuse + reflectionChild.rawSpecular
                : 0;
            float3 primaryResidual = max(
                payload.radiance - layeredTransmission - layeredReflection,
                0.0.xxx);
            payload.rawDiffuse = resolvedReflection ? reflectionChild.rawDiffuse : 0;
            payload.rawSpecular = isPsrSurface
                ? payload.psrThroughput * payload.radiance
                : (resolvedReflection
                    ? reflectionChild.rawSpecular + primaryResidual
                    : primaryResidual);
        }
        if (isPsrSurface)
            payload.psrActive = 2u;

        // Recursive entry/exit interfaces must not overwrite the primary
        // reconstruction UAVs. They return radiance and layered raw signals to
        // their parent; only the camera-visible or mirror-replacement interface
        // publishes the primary layer.
        if (payload.depth == 0u || isPsrSurface)
        {
            uint2 glassPixel = DispatchRaysIndex().xy;
            if (DlssGuideMode == 2u && payload.depth == 0u)
            {
                // RR still reconstructs the complete camera-visible glass
                // signal. Its optional transparency overlay is composited but
                // not a substitute for denoising raw path-traced refraction.
                ReconstructionNoisyHdr[glassPixel] = float4(payload.radiance, 1.0);
            }
            else
            {
                ReconstructionNoisyHdr[glassPixel] = float4(
                    payload.rawDiffuse + payload.rawSpecular,
                    1.0);
            }
            if (reflectionChild.psrActive != 2u)
            {
                ReconstructionDiffuseHitDistance[glassPixel] = 0.0;
                ReconstructionSpecularHitDistance[glassPixel] = glassHitDistance;
                if (payload.depth == 0u)
                    GBufferHitDistance[glassPixel] = glassHitDistance;
            }
        }
        return;
    }
    if (kind == 1u)
    {
        direction = reflect(WorldRayDirection(), normal);
        bounceWeight = baseColor.xyz;
        specularBounceWeight = bounceWeight;
    }
    else if (kind == 2u)
    {
        float etaRatio = frontFace ? (1.0 / max(material.ior, 1.0001)) : max(material.ior, 1.0001);
        float cosine = saturate(dot(-WorldRayDirection(), normal));
        float3 refracted = refract(WorldRayDirection(), normal, etaRatio);
        float fresnelSample = SampleOwenSobol2D(
            DispatchRaysIndex().xy,
            payload.sampleDimensionOffset + 6u + payload.depth * 8u).x;
        bool reflectRay = length(refracted) < 0.001
            || Schlick(cosine, etaRatio) > fresnelSample;
        direction = reflectRay ? reflect(WorldRayDirection(), normal) : refracted;
        sampledTransmission = !reflectRay;
        bounceWeight = baseColor.xyz;
        specularBounceWeight = bounceWeight;
    }
    else
    {
        float specularProbability = ComputeSpecularProbability(
            baseColor.xyz,
            metallic,
            useNrdProbabilisticLobe);
        float lobeSample = useNrdProbabilisticLobe
            ? SampleStratifiedLobe(DispatchRaysIndex().xy, payload.seed)
            : RandomFloat(payload.seed);
        sampledSpecular = specularProbability >= 1.0
            || (specularProbability > 0.0 && lobeSample < specularProbability);
        float2 directionSample = SampleOwenSobol2D(
            DispatchRaysIndex().xy,
            payload.sampleDimensionOffset + 4u + payload.depth * 8u);
        if (sampledSpecular)
        {
            direction = SampleGgxVndfDirection(normal, viewDirection, roughness, directionSample);
            float NoL = max(0.0, dot(normal, direction));
            BrdfEvaluation brdf = EvaluateBrdf(
                normal,
                viewDirection,
                direction,
                baseColor.xyz,
                metallic,
                roughness);
            if (useNrdProbabilisticLobe)
            {
                samplePdf = specularProbability * brdf.specularPdf;
                specularBounceWeight = brdf.specular * NoL / max(samplePdf, 1.0e-6);
            }
            else
            {
                samplePdf = specularProbability * brdf.specularPdf
                    + (1.0 - specularProbability) * brdf.diffusePdf;
                diffuseBounceWeight = brdf.diffuse * NoL / max(samplePdf, 1.0e-6);
                specularBounceWeight = brdf.specular * NoL / max(samplePdf, 1.0e-6);
            }
            bounceWeight = diffuseBounceWeight + specularBounceWeight;
        }
        else
        {
            direction = SampleCosineHemisphere(normal, directionSample);
            float NoL = max(0.0, dot(normal, direction));
            BrdfEvaluation brdf = EvaluateBrdf(
                normal,
                viewDirection,
                direction,
                baseColor.xyz,
                metallic,
                roughness);
            if (useNrdProbabilisticLobe)
            {
                samplePdf = (1.0 - specularProbability) * brdf.diffusePdf;
                diffuseBounceWeight = brdf.diffuse * NoL / max(samplePdf, 1.0e-6);
            }
            else
            {
                samplePdf = (1.0 - specularProbability) * brdf.diffusePdf
                    + specularProbability * brdf.specularPdf;
                diffuseBounceWeight = brdf.diffuse * NoL / max(samplePdf, 1.0e-6);
                specularBounceWeight = brdf.specular * NoL / max(samplePdf, 1.0e-6);
            }
            bounceWeight = diffuseBounceWeight + specularBounceWeight;
        }
    }

    if (!sampledTransmission && dot(normal, direction) <= 0.0)
    {
        bounceWeight = 0;
        diffuseBounceWeight = 0;
        specularBounceWeight = 0;
    }
    RayDesc bounce;
    bounce.Origin = hitPosition + (kind == 2u ? direction : normal) * 0.002;
    bounce.Direction = normalize(direction);
    bounce.TMin = 0.001;
    bounce.TMax = 1000.0;
    Payload child;
    child.radiance = 0;
    child.seed = payload.seed;
    child.depth = payload.depth + 1;
    child.lastPdf = kind == 0u ? max(samplePdf, 1.0e-6) : 0.0;
    child.firstKind = payload.depth == 0u
        ? (sampledSpecular ? max(kind, 1u) : 0u)
        : payload.firstKind;
    child.hitDistance = 0;
    child.rawDiffuse = 0;
    child.rawSpecular = 0;
    child.psrActive = 0;
    child.sampleDimensionOffset = payload.sampleDimensionOffset;
    child.psrThroughput = 0;
    child.psrMirrorPlane = 0;

    // The compact payload carries only the current reflection plane. Until a
    // previous-frame plane is added, moving mirrors must use the conventional
    // specular path so PSR never fabricates a plausible but wrong motion vector.
    bool psrMirrorIsStatic = length(previousHitPosition - hitPosition) <= 1.0e-5;
    if (NrdEnabled != 0u
        && kind == 1u
        && payload.depth == 0u
        && payload.psrActive == 0u
        && psrMirrorIsStatic)
    {
        child.psrActive = 1u;
        child.psrThroughput = baseColor.xyz;
        child.psrMirrorPlane = MakeMirrorPlane(normal, hitPosition);
    }
    TraceRay(
        Scene,
        RAY_FLAG_CULL_BACK_FACING_TRIANGLES,
        0xFF,
        0,
        1,
        0,
        bounce,
        child);
    payload.seed = child.seed;
    float3 bouncedRadiance = bounceWeight * child.radiance;
    float3 localRawDiffuse = directDiffuse + diffuseBounceWeight * child.radiance;
    float3 localRawSpecular = emissive
        + directSpecular
        + specularBounceWeight * child.radiance;
    bool primaryUsesPsr = NrdEnabled != 0u
        && payload.depth == 0u
        && kind == 1u
        && child.psrActive == 2u;
    if (payload.depth == 0)
    {
        payload.rawDiffuse = primaryUsesPsr ? child.rawDiffuse : localRawDiffuse;
        payload.rawSpecular = primaryUsesPsr ? child.rawSpecular : localRawSpecular;
        // SVGF and RR keep the low-variance mixture estimator and therefore
        // share the continuation hitT across both non-zero lobe estimates. NRD
        // uses a Bayer-stratified probabilistic lobe estimator: the skipped
        // lobe is zero, so only the selected in-lobe hitT is exported.
        if (any(diffuseBounceWeight > 0.0))
            diffuseHitDistance = child.hitDistance;
        if (any(specularBounceWeight > 0.0))
            specularHitDistance = child.hitDistance;
        if (PathSpacePass == 1u)
        {
            // A stable fill root has no parent MIS consumer after it returns.
            // Reuse these two output slots instead of increasing the recursive
            // 96-byte payload paid by every path vertex.
            payload.lastPdf = diffuseHitDistance;
            payload.hitDistance = specularHitDistance;
        }
        // Trace reflection tracking independently from the noisy radiance
        // sample. Explicit reflected-geometry motion avoids the residual
        // subpixel wobble produced when RR derives it from scalar hitT.
        if (DlssGuideMode == 2u && (kind == 0u || kind == 1u))
        {
            RrSpecularGuide rrGuide = TraceRrSpecularGuide(
                hitPosition,
                previousHitPosition,
                normal,
                PreviousWorldNormal(normal, instanceData),
                WorldRayDirection(),
                DlssMotion[DispatchRaysIndex().xy],
                DispatchRaysDimensions().xy);
            specularHitDistance = rrGuide.hitDistance;
            DlssSpecularMotion[DispatchRaysIndex().xy] = ResetHistory != 0u
                ? 0
                : rrGuide.motion;
        }
        ReconstructionNoisyHdr[DispatchRaysIndex().xy] = float4(
            FiniteNonNegative(payload.rawDiffuse + payload.rawSpecular),
            1.0);
    }
    else if (isPsrSurface)
    {
        // Split radiance at the virtual primary surface, not at the mirror.
        // Mirror tint is path throughput and therefore stays in the signal;
        // the PSR material factors describe only the visible replacement hit.
        payload.rawDiffuse = payload.psrThroughput * localRawDiffuse;
        payload.rawSpecular = payload.psrThroughput * localRawSpecular;
        payload.psrActive = 2u;
        ReconstructionNoisyHdr[DispatchRaysIndex().xy] = float4(
            FiniteNonNegative(payload.rawDiffuse + payload.rawSpecular),
            1.0);
    }
    else if (isTransmissionSurface)
    {
        // The throughput already contains entry/exit Fresnel and glass tint.
        // Keep it in the signal while the replacement material factors remain
        // those of the surface actually visible through the glass.
        payload.rawDiffuse = payload.psrThroughput * localRawDiffuse;
        payload.rawSpecular = payload.psrThroughput * localRawSpecular;
        payload.psrActive = 4u;
        if (NrdEnabled != 0u)
        {
            uint2 transmissionPixel = DispatchRaysIndex().xy;
            TransmissionRawDiffuse[transmissionPixel] = float4(
                FiniteNonNegative(payload.rawDiffuse),
                1.0);
            TransmissionRawSpecular[transmissionPixel] = float4(
                FiniteNonNegative(payload.rawSpecular),
                1.0);
            TransmissionDiffuseHitDistance[transmissionPixel] = diffuseHitDistance;
            TransmissionSpecularHitDistance[transmissionPixel] = specularHitDistance;
            TransmissionPrimaryEmissive[transmissionPixel] = float4(
                FiniteNonNegative(payload.psrThroughput * emissive),
                1.0);
        }
    }
    if (payload.depth == 0)
    {
        if (sampledSpecular)
            GBufferHitDistance[DispatchRaysIndex().xy] = child.hitDistance;
        if (!primaryUsesPsr)
        {
            ReconstructionDiffuseHitDistance[DispatchRaysIndex().xy] = diffuseHitDistance;
            ReconstructionSpecularHitDistance[DispatchRaysIndex().xy] = specularHitDistance;
        }
    }
    else if (isPsrSurface)
    {
        ReconstructionDiffuseHitDistance[DispatchRaysIndex().xy] = diffuseHitDistance;
        ReconstructionSpecularHitDistance[DispatchRaysIndex().xy] = specularHitDistance;
    }
    payload.radiance = emissive + directDiffuse + directSpecular + bouncedRadiance;
}

// Kept as a reference while validating the GGX replacement; it is not an
// exported shader and therefore cannot be selected by the DXR hit group.
void LegacyClosestHit(inout Payload payload, in BuiltInTriangleIntersectionAttributes attributes)
{
    uint primitive = PrimitiveIndex();
    InstanceGpu instanceData = Instances[InstanceID()];
    uint3 triIndices = uint3(
        Indices[instanceData.indexOffset + primitive * 3],
        Indices[instanceData.indexOffset + primitive * 3 + 1],
        Indices[instanceData.indexOffset + primitive * 3 + 2]);
    float3 barycentric = float3(
        1.0 - attributes.barycentrics.x - attributes.barycentrics.y,
        attributes.barycentrics.x,
        attributes.barycentrics.y);
    float3 localPosition = Vertices[instanceData.vertexOffset + triIndices.x].position * barycentric.x
        + Vertices[instanceData.vertexOffset + triIndices.y].position * barycentric.y
        + Vertices[instanceData.vertexOffset + triIndices.z].position * barycentric.z;
    float3 localNormal = normalize(
        Vertices[instanceData.vertexOffset + triIndices.x].normal * barycentric.x
        + Vertices[instanceData.vertexOffset + triIndices.y].normal * barycentric.y
        + Vertices[instanceData.vertexOffset + triIndices.z].normal * barycentric.z);
    float2 texcoord0 = Vertices[instanceData.vertexOffset + triIndices.x].texcoord0 * barycentric.x
        + Vertices[instanceData.vertexOffset + triIndices.y].texcoord0 * barycentric.y
        + Vertices[instanceData.vertexOffset + triIndices.z].texcoord0 * barycentric.z;
    uint materialIndex = instanceData.materialIndex;
    Material material = Materials[materialIndex];
    float3 geometricNormal = normalize(mul(localNormal, (float3x3)WorldToObject3x4()));
    bool frontFace = HitKind() == HIT_KIND_TRIANGLE_FRONT_FACE;
    float3 normal = frontFace ? geometricNormal : -geometricNormal;
    float4 baseColor = material.baseColorFactor
        * SampleMaterialTexture(material.baseColorTextureAndSampler, texcoord0);
    float4 metallicRoughness = SampleMaterialTexture(
        material.metallicRoughnessTextureAndSampler,
        texcoord0);
    float3 emissive = material.emissiveFactor
        * SampleMaterialTexture(material.emissiveTextureAndSampler, texcoord0).xyz;
    float metallic = saturate(material.metallicFactor * metallicRoughness.b);
    float roughness = saturate(material.roughnessFactor * metallicRoughness.g);
    uint kind = (material.flags & 4u) != 0u
        ? 2u
        : (any(emissive > 0.0) ? 3u : (metallic > 0.5 ? 1u : 0u));
    float3 hitPosition = mul(ObjectToWorld3x4(), float4(localPosition, 1.0));
    float3 previousHitPosition = PreviousWorldPosition(localPosition, instanceData);
    payload.hitDistance = RayTCurrent();

    if (payload.depth == 0)
    {
        uint2 pixel = DispatchRaysIndex().xy;
        uint2 size = DispatchRaysDimensions().xy;
        payload.firstKind = kind;
        GBufferAlbedo[pixel] = float4(baseColor.xyz, float(kind));
        GBufferNormalRoughness[pixel] = float4(normal * 0.5 + 0.5, roughness);
        GBufferDepth[pixel] = RayTCurrent();
        GBufferId[pixel] = instanceData.stableSurfaceId;
        GBufferWorldPosition[pixel] = float4(hitPosition, 1.0);
        float2 currentUv = (float2(pixel) + 0.5) / float2(size);
        float2 previousUv = ProjectToPreviousUv(previousHitPosition, size);
        GBufferMotion[pixel] = ResetHistory != 0u
            ? 0
            : (currentUv - previousUv) * float2(size);
        if (DlssGuideMode != 0u)
        {
            DlssDepth[pixel] = DlssDeviceDepth(hitPosition);
            DlssMotion[pixel] = ResetHistory != 0u
                ? 0
                : (previousUv - currentUv) * float2(size);
            if (DlssGuideMode == 2u)
                DlssSpecularMotion[pixel] = DlssMotion[pixel];
        }
    }

    if (kind == 3u)
    {
        float weight = 1.0;
        if (payload.depth > 0 && payload.lastPdf > 0.0)
        {
            const float lightArea = 0.25;
            float lightPdf = RayTCurrent() * RayTCurrent()
                / max(0.0001, abs(dot(normal, -WorldRayDirection())) * lightArea);
            float bsdfSquared = payload.lastPdf * payload.lastPdf;
            weight = bsdfSquared / (bsdfSquared + lightPdf * lightPdf);
        }
        payload.radiance = emissive * weight;
        return;
    }
    if (payload.depth >= 3)
    {
        payload.radiance = 0;
        return;
    }

    float3 directLighting = 0;
    float3 direction;
    if (kind == 1u)
    {
        direction = reflect(WorldRayDirection(), normal);
    }
    else if (kind == 2u)
    {
        float etaRatio = frontFace ? (1.0 / 1.5) : 1.5;
        float cosine = saturate(dot(-WorldRayDirection(), normal));
        float3 refracted = refract(WorldRayDirection(), normal, etaRatio);
        float fresnelSample = SampleOwenSobol2D(
            DispatchRaysIndex().xy,
            6u + payload.depth * 8u).x;
        direction = length(refracted) < 0.001 || Schlick(cosine, etaRatio) > fresnelSample
            ? reflect(WorldRayDirection(), normal)
            : refracted;
    }
    else
    {
        float2 lightRandom = SampleOwenSobol2D(
            DispatchRaysIndex().xy,
            2u + payload.depth * 8u);
        float3 lightPoint = float3(-0.25 + lightRandom.x * 0.5, 0.9966667, 0.6666667 + lightRandom.y * 0.5);
        float3 toLight = lightPoint - hitPosition;
        float lightDistance = length(toLight);
        float3 lightDirection = toLight / lightDistance;
        float surfaceCosine = max(0.0, dot(normal, lightDirection));
        float lightCosine = max(0.0, dot(float3(0, -1, 0), -lightDirection));
        if (surfaceCosine > 0.0 && lightCosine > 0.0)
        {
            Payload shadow;
            shadow.radiance = 0;
            shadow.seed = payload.seed;
            shadow.depth = payload.depth;
            shadow.lastPdf = 0;
            shadow.firstKind = payload.firstKind;
            shadow.hitDistance = 0;
            RayDesc shadowRay;
            shadowRay.Origin = hitPosition + normal * 0.002;
            shadowRay.Direction = lightDirection;
            shadowRay.TMin = 0.001;
            shadowRay.TMax = lightDistance - 0.004;
            TraceRay(
                Scene,
                RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH
                    | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER
                    | RAY_FLAG_CULL_BACK_FACING_TRIANGLES,
                0xFF,
                0,
                1,
                1,
                shadowRay,
                shadow);
            const float lightArea = 0.25;
            float lightPdf = lightDistance * lightDistance / (lightCosine * lightArea);
            float bsdfPdf = surfaceCosine / 3.14159265359;
            float lightSquared = lightPdf * lightPdf;
            float misWeight = lightSquared / (lightSquared + bsdfPdf * bsdfPdf);
            directLighting = shadow.radiance * baseColor.xyz * Materials[3].emissiveFactor
                * (surfaceCosine / 3.14159265359) * misWeight / lightPdf;
        }
        direction = SampleCosineHemisphere(
            normal,
            SampleOwenSobol2D(DispatchRaysIndex().xy, 4u + payload.depth * 8u));
    }

    RayDesc bounce;
    bounce.Origin = hitPosition + direction * 0.002;
    bounce.Direction = normalize(direction);
    bounce.TMin = 0.001;
    bounce.TMax = 1000.0;
    Payload child;
    child.radiance = 0;
    child.seed = payload.seed;
    child.depth = payload.depth + 1;
    child.lastPdf = kind == 0u
        ? max(0.0, dot(normal, bounce.Direction)) / 3.14159265359
        : 0.0;
    child.firstKind = payload.firstKind;
    child.hitDistance = 0;
    TraceRay(
        Scene,
        RAY_FLAG_CULL_BACK_FACING_TRIANGLES,
        0xFF,
        0,
        1,
        0,
        bounce,
        child);
    payload.seed = child.seed;
    if (payload.depth == 0 && (kind == 1u || kind == 2u))
        GBufferHitDistance[DispatchRaysIndex().xy] = child.hitDistance;
    payload.radiance = directLighting + baseColor.xyz * child.radiance;
}

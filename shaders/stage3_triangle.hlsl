// 阶段 4 的 Cornell Box 路径追踪 Shader 库。
RaytracingAccelerationStructure Scene : register(t0);

struct Vertex
{
    float3 position;
    float3 normal;
};

struct Material
{
    float4 albedo;
    float4 emissionAndKind;
};

StructuredBuffer<Vertex> Vertices : register(t1);
StructuredBuffer<uint> Indices : register(t2);
StructuredBuffer<uint> MaterialIndices : register(t3);
StructuredBuffer<Material> Materials : register(t4);
RWTexture2D<float4> Output : register(u0);
RWTexture2D<float4> GBufferAlbedo : register(u1);
RWTexture2D<float4> GBufferNormal : register(u2);
RWTexture2D<float> GBufferDepth : register(u3);
RWTexture2D<float4> Accumulation : register(u4);
RWTexture2D<float2> HistoryMoments : register(u5);
RWTexture2D<float2> MotionVectors : register(u6);
RWTexture2D<float4> PreviousNormal : register(u7);
RWTexture2D<float> PreviousDepth : register(u8);
RWTexture2D<float4> PreviousAccumulation : register(u9);

cbuffer FrameConstants : register(b0)
{
    uint FrameIndex;
    float3 CameraPosition;
    float CameraYaw;
    float CameraPitch;
    float2 CameraPadding;
    float3 PreviousCameraPosition;
    float PreviousCameraYaw;
    float PreviousCameraPitch;
};

struct Payload
{
    float3 radiance;
    uint seed;
    uint depth;
    float lastPdf;
};

uint RandomUint(inout uint state)
{
    state ^= state << 13;
    state ^= state >> 17;
    state ^= state << 5;
    return state;
}

float RandomFloat(inout uint state)
{
    return (RandomUint(state) & 0x00FFFFFF) / 16777216.0;
}

float3 SampleHemisphere(float3 normal, inout uint seed)
{
    float z = RandomFloat(seed);
    float angle = 6.2831853 * RandomFloat(seed);
    float radius = sqrt(max(0.0, 1.0 - z * z));
    float3 local = float3(radius * cos(angle), radius * sin(angle), z);
    float3 tangent = normalize(abs(normal.z) < 0.999 ? cross(float3(0, 0, 1), normal) : cross(float3(0, 1, 0), normal));
    float3 bitangent = cross(normal, tangent);
    return normalize(tangent * local.x + bitangent * local.y + normal * local.z);
}

float Schlick(float cosine, float refractionRatio)
{
    float r0 = (1.0 - refractionRatio) / (1.0 + refractionRatio);
    r0 *= r0;
    return r0 + (1.0 - r0) * pow(1.0 - cosine, 5.0);
}

[shader("raygeneration")]
void RayGen()
{
    uint2 pixel = DispatchRaysIndex().xy;
    uint2 size = DispatchRaysDimensions().xy;
    uint seed = pixel.x * 1973 + pixel.y * 9277 + FrameIndex * 26699 + 89173;
    float2 jitter = float2(RandomFloat(seed), RandomFloat(seed));
    float2 uv = (float2(pixel) + jitter) / float2(size);
    float2 screen = uv * 2.0 - 1.0;
    screen.x *= float(size.x) / float(size.y);

    RayDesc ray;
    float3 forward = normalize(float3(sin(CameraYaw) * cos(CameraPitch), sin(CameraPitch), cos(CameraYaw) * cos(CameraPitch)));
    float3 right = normalize(cross(float3(0, 1, 0), forward));
    float3 up = cross(forward, right);
    ray.Origin = CameraPosition;
    ray.Direction = normalize(forward * 1.65 + right * screen.x - up * screen.y * 0.9);
    ray.TMin = 0.001;
    ray.TMax = 1000.0;

    Payload payload;
    payload.radiance = 0;
    payload.seed = seed;
    payload.depth = 0;
    payload.lastPdf = 0;
    GBufferAlbedo[pixel] = 0;
    GBufferNormal[pixel] = 0;
    GBufferDepth[pixel] = 0;
    TraceRay(Scene, RAY_FLAG_NONE, 0xFF, 0, 1, 0, ray, payload);
    float4 sample = float4(payload.radiance, 1.0);
    float2 camera_delta = float2(CameraPosition.x - PreviousCameraPosition.x, CameraPosition.z - PreviousCameraPosition.z);
    camera_delta.x += (CameraYaw - PreviousCameraYaw) * 0.5;
    camera_delta.y += (CameraPitch - PreviousCameraPitch) * 0.5;
    int2 reprojection_offset = int2(camera_delta * float2(size) * 0.08);
    int2 history_pixel = clamp(int2(pixel) + reprojection_offset, int2(0, 0), int2(size) - 1);
    float4 history_sample = PreviousAccumulation[history_pixel];
    float4 history = FrameIndex == 0 ? sample : history_sample;
    float4 accumulated = (history * FrameIndex + sample) / (FrameIndex + 1.0);
    Accumulation[pixel] = accumulated;
    MotionVectors[pixel] = float2(CameraPosition.x - PreviousCameraPosition.x, CameraPosition.z - PreviousCameraPosition.z);

    // 使用历史矩估计当前像素的亮度方差，方差越大时越谨慎地融合邻域。
    float center_luminance = dot(accumulated.xyz, float3(0.2126, 0.7152, 0.0722));
    float history_mean = HistoryMoments[pixel].x;
    float history_variance = max(0.0001, HistoryMoments[pixel].y - history_mean * history_mean);
    float3 filtered = accumulated.xyz;
    float currentDepth = GBufferDepth[pixel];
    float3 currentNormal = GBufferNormal[pixel].xyz * 2.0 - 1.0;
    float previousDepth = PreviousDepth[pixel];
    float3 previousNormal = PreviousNormal[pixel].xyz * 2.0 - 1.0;
    bool historyValid = FrameIndex > 0 && previousDepth > 0.0
        && abs(currentDepth - previousDepth) < max(0.02, currentDepth * 0.05)
        && dot(currentNormal, previousNormal) > 0.85;
    if (historyValid)
    {
        float3 spatial_sum = accumulated.xyz;
        float spatial_weight = 1.0;
        [unroll]
        for (int pass = 0; pass < 2; ++pass)
        {
            int step = 1 << pass;
            [unroll]
            for (int y = -1; y <= 1; ++y)
            {
                [unroll]
                for (int x = -1; x <= 1; ++x)
                {
                    if (x == 0 && y == 0)
                        continue;
                    int2 neighbor = clamp(int2(pixel) + int2(x * step, y * step), int2(0, 0), int2(size) - 1);
                    float3 neighborColor = Accumulation[neighbor].xyz;
                    float neighborDepth = GBufferDepth[neighbor];
                    float3 neighborNormal = GBufferNormal[neighbor].xyz * 2.0 - 1.0;
                    float depth_weight = exp(-abs(neighborDepth - currentDepth) / max(0.01, currentDepth * 0.05));
                    float normal_weight = pow(saturate(dot(currentNormal, neighborNormal)), 8.0);
                    float neighbor_luminance = dot(neighborColor, float3(0.2126, 0.7152, 0.0722));
                    float color_weight = exp(-abs(neighbor_luminance - center_luminance) / (0.25 + history_variance * 4.0));
                    float weight = depth_weight * normal_weight * color_weight;
                    spatial_sum += neighborColor * weight;
                    spatial_weight += weight;
                }
            }
        }
        filtered = spatial_sum / spatial_weight;
    }
    float luminance = dot(filtered, float3(0.2126, 0.7152, 0.0722));
    float previousLuminance = historyValid ? HistoryMoments[pixel].x : luminance;
    float previousSecondMoment = historyValid ? HistoryMoments[pixel].y : luminance * luminance;
    float secondMoment = (previousSecondMoment * FrameIndex + luminance * luminance) / (FrameIndex + 1.0);
    float meanMoment = (previousLuminance * FrameIndex + luminance) / (FrameIndex + 1.0);
    HistoryMoments[pixel] = float2(meanMoment, secondMoment);
    Output[pixel] = float4(filtered, 1.0);
}

[shader("miss")]
void Miss(inout Payload payload)
{
    payload.radiance = float3(0.003, 0.005, 0.012);
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
    uint3 tri_indices = uint3(Indices[primitive * 3], Indices[primitive * 3 + 1], Indices[primitive * 3 + 2]);
    float3 barycentric = float3(1.0 - attributes.barycentrics.x - attributes.barycentrics.y, attributes.barycentrics.x, attributes.barycentrics.y);
    float3 normal = normalize(Vertices[tri_indices.x].normal * barycentric.x + Vertices[tri_indices.y].normal * barycentric.y + Vertices[tri_indices.z].normal * barycentric.z);
    if (dot(normal, WorldRayDirection()) > 0.0)
        normal = -normal;

    Material material = Materials[MaterialIndices[primitive]];
    if (payload.depth == 0)
    {
        uint2 pixel = DispatchRaysIndex().xy;
        GBufferAlbedo[pixel] = float4(material.albedo.xyz, 1.0);
        GBufferNormal[pixel] = float4(normal * 0.5 + 0.5, 1.0);
        GBufferDepth[pixel] = RayTCurrent();
    }
    if (material.emissionAndKind.w > 2.5)
    {
        float weight = 1.0;
        if (payload.depth > 0 && payload.lastPdf > 0.0)
        {
            const float lightArea = 0.42;
            float lightPdf = RayTCurrent() * RayTCurrent() / max(0.0001, abs(dot(normal, -WorldRayDirection())) * lightArea);
            float bsdfSquared = payload.lastPdf * payload.lastPdf;
            weight = bsdfSquared / (bsdfSquared + lightPdf * lightPdf);
        }
        payload.radiance = material.emissionAndKind.xyz * weight;
        return;
    }
    if (payload.depth >= 3)
    {
        payload.radiance = 0;
        return;
    }

    float3 hitPosition = WorldRayOrigin() + RayTCurrent() * WorldRayDirection();
    float3 directLighting = 0;
    float3 direction;
    uint kind = uint(material.emissionAndKind.w + 0.5);
    if (kind == 1)
    {
        direction = reflect(WorldRayDirection(), normal);
    }
    else if (kind == 2)
    {
        float ratio = dot(WorldRayDirection(), normal) < 0.0 ? (1.0 / 1.5) : 1.5;
        float cosine = min(dot(-WorldRayDirection(), normal), 1.0);
        float3 refracted = refract(WorldRayDirection(), normal, ratio);
        direction = length(refracted) < 0.001 || Schlick(cosine, ratio) > RandomFloat(payload.seed)
            ? reflect(WorldRayDirection(), normal)
            : refracted;
    }
    else
    {
        float2 lightRandom = float2(RandomFloat(payload.seed), RandomFloat(payload.seed));
        float3 lightPoint = float3(-0.35 + lightRandom.x * 0.7, 1.98, 0.75 + lightRandom.y * 0.6);
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
            RayDesc shadowRay;
            shadowRay.Origin = hitPosition + normal * 0.002;
            shadowRay.Direction = lightDirection;
            shadowRay.TMin = 0.001;
            shadowRay.TMax = lightDistance - 0.004;
            TraceRay(Scene, RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER, 0xFF, 0, 1, 1, shadowRay, shadow);
            const float lightArea = 0.42;
            float lightPdf = lightDistance * lightDistance / (lightCosine * lightArea);
            float bsdfPdf = surfaceCosine / 3.14159265;
            float lightSquared = lightPdf * lightPdf;
            float misWeight = lightSquared / (lightSquared + bsdfPdf * bsdfPdf);
            directLighting = shadow.radiance * material.albedo.xyz * float3(14.0, 12.0, 9.0)
                * (surfaceCosine / 3.14159265) * misWeight / lightPdf;
        }
        direction = SampleHemisphere(normal, payload.seed);
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
    child.lastPdf = kind == 0 ? max(0.0, dot(normal, bounce.Direction)) / 3.14159265 : 0.0;
    TraceRay(Scene, RAY_FLAG_NONE, 0xFF, 0, 1, 0, bounce, child);
    payload.seed = child.seed;
    payload.radiance = directLighting + material.albedo.xyz * child.radiance;
}

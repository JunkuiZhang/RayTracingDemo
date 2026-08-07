// 阶段 3 的最小 DXR Shader 库：射线生成、未命中和三角形命中。
RaytracingAccelerationStructure Scene : register(t0);
RWTexture2D<float4> Output : register(u0);

struct Payload
{
    float4 color;
};

[shader("raygeneration")]
void RayGen()
{
    uint2 pixel = DispatchRaysIndex().xy;
    uint2 size = DispatchRaysDimensions().xy;
    float2 uv = (float2(pixel) + 0.5) / float2(size);
    float2 screen = uv * 2.0 - 1.0;
    RayDesc ray;
    ray.Origin = float3(0.0, 0.0, -2.0);
    ray.Direction = normalize(float3(screen.x, -screen.y, 1.5));
    ray.TMin = 0.001;
    ray.TMax = 1000.0;
    Payload payload;
    payload.color = float4(0, 0, 0, 1);
    TraceRay(Scene, RAY_FLAG_NONE, 0xFF, 0, 1, 0, ray, payload);
}

[shader("miss")]
void Miss(inout Payload payload)
{
    payload.color = float4(0.015, 0.025, 0.08, 1.0);
    Output[DispatchRaysIndex().xy] = payload.color;
}

[shader("closesthit")]
void ClosestHit(inout Payload payload, in BuiltInTriangleIntersectionAttributes attributes)
{
    float3 normal = normalize(cross(float3(1, 0, 0), float3(0, 1, 0)));
    payload.color = float4(abs(normal), 1.0);
    Output[DispatchRaysIndex().xy] = payload.color;
}

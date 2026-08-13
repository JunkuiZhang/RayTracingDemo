// Shared camera math for the jittered path tracer and the unjittered RR
// visibility pass. Keeping the focal length and basis construction here makes
// the two passes agree on viewZ and pixel motion without sharing jitter.
static const float STAGE11_CAMERA_FOCAL_LENGTH = 2.747477419;

void Stage11CameraBasis(
    float yaw,
    float pitch,
    out float3 forward,
    out float3 right,
    out float3 up)
{
    forward = normalize(float3(sin(yaw) * cos(pitch), sin(pitch), cos(yaw) * cos(pitch)));
    right = normalize(cross(float3(0, 1, 0), forward));
    up = cross(forward, right);
}

float2 Stage11ProjectToUv(
    float3 worldPosition,
    float3 cameraPosition,
    float yaw,
    float pitch,
    uint2 size)
{
    float3 forward;
    float3 right;
    float3 up;
    Stage11CameraBasis(yaw, pitch, forward, right, up);
    float3 relative = worldPosition - cameraPosition;
    float forwardDistance = dot(relative, forward);
    if (forwardDistance <= 0.0001)
        return float2(-2.0, -2.0);

    float aspect = float(size.x) / float(size.y);
    float2 screen;
    screen.x = STAGE11_CAMERA_FOCAL_LENGTH * dot(relative, right) / forwardDistance;
    screen.y = -STAGE11_CAMERA_FOCAL_LENGTH * dot(relative, up) / forwardDistance;
    return float2(screen.x / aspect, screen.y) * 0.5 + 0.5;
}

float Stage11ViewZ(float3 worldPosition, float3 cameraPosition, float yaw, float pitch)
{
    float3 forward;
    float3 right;
    float3 up;
    Stage11CameraBasis(yaw, pitch, forward, right, up);
    return dot(worldPosition - cameraPosition, forward);
}

float3 Stage11PrimaryRayDirection(
    uint2 pixel,
    uint2 size,
    float3 cameraPosition,
    float yaw,
    float pitch)
{
    float2 uv = (float2(pixel) + 0.5) / float2(size);
    float2 screen = uv * 2.0 - 1.0;
    screen.x *= float(size.x) / float(size.y);
    float3 forward;
    float3 right;
    float3 up;
    Stage11CameraBasis(yaw, pitch, forward, right, up);
    return normalize(forward * STAGE11_CAMERA_FOCAL_LENGTH + right * screen.x - up * screen.y);
}

float2 Stage11OctEncode(float3 normal)
{
    normal /= max(abs(normal.x) + abs(normal.y) + abs(normal.z), 1.0e-6);
    if (normal.z < 0.0)
    {
        normal.xy = (1.0 - abs(normal.yx)) * sign(normal.xy);
    }
    return normal.xy * 0.5 + 0.5;
}

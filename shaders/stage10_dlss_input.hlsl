// Stage 10C: compose the denoised linear HDR signal before Streamline DLSS.
// This shader intentionally has no sampling, filtering, or exposure logic;
// those remain owned by the existing denoiser and later DLSS pass.
Texture2D<float4> DenoisedDiffuse : register(t0);
Texture2D<float4> DenoisedSpecular : register(t1);
RWTexture2D<float4> DlssInputHdr : register(u0);
RWTexture2D<float4> Exposure : register(u1);

cbuffer DlssInputConstants : register(b0)
{
    uint ResetHistory;
};

[numthreads(8, 8, 1)]
void main(uint3 dispatch_id : SV_DispatchThreadID)
{
    uint width;
    uint height;
    DlssInputHdr.GetDimensions(width, height);
    if (dispatch_id.x >= width || dispatch_id.y >= height)
    {
        return;
    }

    float4 diffuse = DenoisedDiffuse.Load(int3(dispatch_id.xy, 0));
    float4 specular = DenoisedSpecular.Load(int3(dispatch_id.xy, 0));
    // Keep the reset value in the root contract even though a reset does not
    // alter the HDR signal; history invalidation belongs to the DLSS options.
    uint unused_reset = ResetHistory;
    DlssInputHdr[dispatch_id.xy] = float4(max(diffuse.rgb + specular.rgb, 0.0), unused_reset == 0u ? 1.0 : 1.0);
    if (all(dispatch_id.xy == uint2(0, 0)))
    {
        Exposure[uint2(0, 0)] = float4(1.0, 1.0, 1.0, 1.0);
    }
}

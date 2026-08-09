# NVIDIA NRD v4.17.3 lock

The NRD SDK is an optional local build input and is not redistributed in this
repository. Run `scripts/fetch_nrd.ps1` explicitly on a machine with network
access. The script verifies the official tag, complete commits, dependency
commits (including NRI's fixed D3D12MemoryAllocator), archive hashes, and
license hashes before the source is placed under the ignored `external/`
directory.

The application uses NRD only as an optional `REBLUR_DIFFUSE_SPECULAR`
comparison backend. SVGF remains the default and this repository does not
include Streamline, DLSS, Ray Reconstruction, RELAX, SIGMA, SH, or ReSTIR.

The 9A–9F integration keeps the C ABI bridge, NRD resources, and descriptor
state inside a fence-retired render generation. `F3` switches between SVGF and
NRD only in an `--features nrd` build; it does not make NRD the default. The
bridge and all dependencies are built from the commits in
`version.lock.json` with local, disconnected FetchContent. No network access is
performed by `build.rs`, and no NRD binary or SDK is redistributed by this
repository.

The fetched SDK's NVIDIA RTX SDK License and third-party notices must remain
available for any local build or distribution review. For an `nrd` feature
build, `build.rs` copies the verified NRD, NRI, MathLib, ShaderMake and
D3D12MemoryAllocator license/notice files beside the executable. This notice is
not a replacement for those license files.

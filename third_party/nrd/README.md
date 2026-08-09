# NVIDIA NRD v4.17.3 lock

The NRD SDK is an optional local build input and is not redistributed in this
repository. Run `scripts/fetch_nrd.ps1` explicitly on a machine with network
access. The script verifies the official tag, complete commits, dependency
commits, archive hashes, and license hashes before the source is placed under
the ignored `external/` directory.

The application uses NRD only as an optional `REBLUR_DIFFUSE_SPECULAR`
comparison backend. SVGF remains the default and this repository does not
include Streamline, DLSS, Ray Reconstruction, RELAX, SIGMA, SH, or ReSTIR.

The fetched SDK's NVIDIA RTX SDK License and third-party notices must remain
available for any local build or distribution review. This notice is not a
replacement for those license files.

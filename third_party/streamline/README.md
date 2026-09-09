# NVIDIA Streamline v2.14.1 lock

Streamline is an optional local SDK input. The repository does not redistribute
the release ZIP, SDK tree, DLLs, or generated binaries. Run
`scripts/fetch_streamline.ps1` explicitly on a machine with network access; the
script verifies the exact official release URL and archive hash, then checks the
required headers/import library, production/development binaries, NVIDIA code
signatures, and licenses before moving the SDK into ignored
`external/streamline-v2.14.1/`. The lock separately records the resolved tag
commit for provenance.

The lock records the archive and file hashes observed from the official
`v2.14.1` release on 2026-09-08. A changed or incomplete local SDK is never
overwritten automatically. `build.rs` only reads this directory when the
`streamline` feature is enabled and never performs network access.

Release builds use the SDK's production `bin/x64` files. Debug builds use only
the matching `bin/x64/development` files. The Streamline, DLSS, Reflex, PCL,
and NVIDIA DLSS plugin licenses remain beside the executable for local review.

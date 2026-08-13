use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::debug_view::DebugView;

pub const CAPTURE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct CaptureMetadata {
    pub png_path: String,
    pub gpu_name: String,
    pub output_width: u32,
    pub output_height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub requested_scale: f32,
    pub resolution_mode: String,
    pub debug_view: DebugView,
    pub actual_spp: u32,
    pub frame_index: u32,
    pub generation_id: u64,
    pub atrous_mode: String,
    pub command_recording_mode: String,
    pub acceleration_structure_mode: String,
    pub path_space_mode: String,
    pub path_space_consumer: String,
    pub stable_plane_allocated_bytes: u64,
    pub denoiser_backend: String,
    pub upscaler_mode: String,
    pub reflex_mode: String,
    pub streamline_sdk_version: Option<String>,
    pub viewport_id: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureLayoutError {
    WidthOverflow,
    HeightOverflow,
    OffsetOutOfBounds,
    RowPitchTooSmall,
    BufferTooSmall,
}

impl std::fmt::Display for CaptureLayoutError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::WidthOverflow => "capture width overflows RGBA8 row size",
            Self::HeightOverflow => "capture height overflows row offset",
            Self::OffsetOutOfBounds => "capture footprint offset is outside the buffer",
            Self::RowPitchTooSmall => "capture row pitch is smaller than RGBA8 row size",
            Self::BufferTooSmall => "capture readback buffer is shorter than the footprint",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CaptureLayoutError {}

pub fn unpack_rgba8_rows(
    mapped: &[u8],
    footprint_offset: usize,
    row_pitch: usize,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, CaptureLayoutError> {
    let row_bytes = (width as usize)
        .checked_mul(4)
        .ok_or(CaptureLayoutError::WidthOverflow)?;
    if row_pitch < row_bytes {
        return Err(CaptureLayoutError::RowPitchTooSmall);
    }
    let rows = height as usize;
    let source_bytes = row_pitch
        .checked_mul(rows)
        .ok_or(CaptureLayoutError::HeightOverflow)?;
    let end = footprint_offset
        .checked_add(source_bytes)
        .ok_or(CaptureLayoutError::OffsetOutOfBounds)?;
    if end > mapped.len() {
        return Err(CaptureLayoutError::BufferTooSmall);
    }
    let destination_bytes = row_bytes
        .checked_mul(rows)
        .ok_or(CaptureLayoutError::HeightOverflow)?;
    let mut rgba = Vec::with_capacity(destination_bytes);
    for row in 0..rows {
        let start = footprint_offset + row * row_pitch;
        rgba.extend_from_slice(&mapped[start..start + row_bytes]);
    }
    Ok(rgba)
}

pub fn write_png_atomic(
    path: &Path,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<u64, Box<dyn std::error::Error>> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "PNG size overflows"))?;
    if rgba.len() != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "RGBA byte length {} does not match {}",
                rgba.len(),
                expected
            ),
        )
        .into());
    }
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("capture target already exists: {}", path.display()),
        )
        .into());
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = PathBuf::from(format!(
        "{}.tmp-{}-{}",
        path.display(),
        std::process::id(),
        nonce
    ));
    let result = image::save_buffer_with_format(
        &temporary,
        rgba,
        width,
        height,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    );
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    let size = fs::metadata(&temporary)?.len();
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(size)
}

pub fn capture_json_line(metadata: &CaptureMetadata, png_bytes: u64) -> String {
    serde_json::json!({
        "schema_version": CAPTURE_SCHEMA_VERSION,
        "png_path": metadata.png_path,
        "gpu_name": metadata.gpu_name,
        "output_width": metadata.output_width,
        "output_height": metadata.output_height,
        "render_width": metadata.render_width,
        "render_height": metadata.render_height,
        "requested_scale": metadata.requested_scale,
        "resolution_mode": metadata.resolution_mode,
        "debug_view": {
            "name": metadata.debug_view.name(),
            "index": metadata.debug_view.index(),
            "hlsl_value": metadata.debug_view.hlsl_value(),
        },
        "actual_spp": metadata.actual_spp,
        "frame_index": metadata.frame_index,
        "generation_id": metadata.generation_id,
        "modes": {
            "atrous": metadata.atrous_mode,
            "command_recording": metadata.command_recording_mode,
            "acceleration_structure": metadata.acceleration_structure_mode,
            "denoiser": metadata.denoiser_backend,
            "upscaler": metadata.upscaler_mode,
            "reflex": metadata.reflex_mode,
        },
        "path_space": {
            "requested": metadata.path_space_mode,
            "active": metadata.path_space_mode,
            "plane_count": if metadata.path_space_mode == "stable-planes" { 3 } else { 0 },
            // P2 deliberately leaves the displayed reconstruction on the
            // legacy raygen until NRD/RR can consume every plane coherently.
            "consumer": metadata.path_space_consumer,
            "allocated_bytes": metadata.stable_plane_allocated_bytes,
        },
        "streamline": {
            "sdk_version": metadata.streamline_sdk_version,
            "viewport_id": metadata.viewport_id,
        },
        "png_bytes": png_bytes,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_pitch_padding_is_removed_without_reordering_pixels() {
        let mut mapped = [9_u8; 28];
        mapped[4..12].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        mapped[16..24].copy_from_slice(&[11, 12, 13, 14, 15, 16, 17, 18]);
        assert_eq!(
            unpack_rgba8_rows(&mapped, 4, 12, 2, 2).unwrap(),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 15, 16, 17, 18]
        );
    }

    #[test]
    fn row_pitch_without_padding_and_invalid_lengths_are_checked() {
        assert_eq!(
            unpack_rgba8_rows(&[1, 2, 3, 4], 0, 4, 1, 1).unwrap(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            unpack_rgba8_rows(&[1, 2, 3], 0, 4, 1, 1),
            Err(CaptureLayoutError::BufferTooSmall)
        );
        assert_eq!(
            unpack_rgba8_rows(&[0; 8], 0, 3, 1, 1),
            Err(CaptureLayoutError::RowPitchTooSmall)
        );
    }

    #[test]
    fn capture_json_preserves_fixed_and_dynamic_metadata() {
        let metadata = CaptureMetadata {
            png_path: "capture.png".to_string(),
            gpu_name: "RTX 4060".to_string(),
            output_width: 1280,
            output_height: 720,
            render_width: 1280,
            render_height: 720,
            requested_scale: 1.0,
            resolution_mode: "fixed".to_string(),
            debug_view: DebugView::HistoryLength,
            actual_spp: 128,
            frame_index: 130,
            generation_id: 1,
            atrous_mode: "baseline".to_string(),
            command_recording_mode: "optimized".to_string(),
            acceleration_structure_mode: "baseline".to_string(),
            path_space_mode: "stable-planes".to_string(),
            path_space_consumer: "nrd-stable-planes".to_string(),
            stable_plane_allocated_bytes: 123_456,
            denoiser_backend: "svgf".to_string(),
            upscaler_mode: "native".to_string(),
            reflex_mode: "unavailable".to_string(),
            streamline_sdk_version: None,
            viewport_id: None,
        };
        let value: serde_json::Value =
            serde_json::from_str(&capture_json_line(&metadata, 256)).unwrap();
        assert_eq!(value["schema_version"], CAPTURE_SCHEMA_VERSION);
        assert_eq!(value["debug_view"]["index"], 8);
        assert_eq!(value["modes"]["command_recording"], "optimized");
        assert_eq!(value["modes"]["denoiser"], "svgf");
        assert_eq!(value["path_space"]["consumer"], "nrd-stable-planes");
        assert_eq!(value["path_space"]["allocated_bytes"], 123_456);
        assert_eq!(value["png_bytes"], 256);
    }
}

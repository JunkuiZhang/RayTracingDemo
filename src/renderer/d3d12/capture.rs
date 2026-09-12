use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::debug_view::DebugView;

pub const CAPTURE_SCHEMA_VERSION: u32 = 3;

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
    pub requested_path_space: String,
    pub active_path_space: String,
    pub path_space_consumer: String,
    pub stable_plane_allocated_bytes: u64,
    pub denoiser_backend: String,
    pub upscaler_mode: String,
    pub reflex_mode: String,
    pub frame_generation_requested: String,
    pub frame_generation_active: String,
    pub display_mode: String,
    pub hdr_paper_white_nits: Option<u32>,
    pub hdr_peak_nits: Option<u32>,
    pub display_peak_nits: Option<u32>,
    pub bits_per_color: Option<u32>,
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
            Self::WidthOverflow => "capture width overflows source row size",
            Self::HeightOverflow => "capture height overflows row offset",
            Self::OffsetOutOfBounds => "capture footprint offset is outside the buffer",
            Self::RowPitchTooSmall => "capture row pitch is smaller than the source row size",
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

/// Converts a packed RGB10 HDR10/BT.2100 presentation surface to an SDR PNG
/// preview. The live HDR signal is left untouched; capture decoding removes
/// ST.2084, converts BT.2020 to Rec.709, and maps paper white back to 1.0.
pub fn unpack_hdr10_rows_to_rgba8(
    mapped: &[u8],
    footprint_offset: usize,
    row_pitch: usize,
    width: u32,
    height: u32,
    paper_white_nits: u32,
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

    let pixel_count = (width as usize)
        .checked_mul(rows)
        .ok_or(CaptureLayoutError::HeightOverflow)?;
    let mut rgba = Vec::with_capacity(
        pixel_count
            .checked_mul(4)
            .ok_or(CaptureLayoutError::HeightOverflow)?,
    );
    let paper_white_nits = paper_white_nits.max(1) as f32;
    for row in 0..rows {
        let start = footprint_offset + row * row_pitch;
        for pixel in mapped[start..start + row_bytes].chunks_exact(4) {
            let packed = u32::from_le_bytes(pixel.try_into().expect("chunks_exact yields 4 bytes"));
            let pq_rec2020 = [
                (packed & 0x3ff) as f32 / 1023.0,
                ((packed >> 10) & 0x3ff) as f32 / 1023.0,
                ((packed >> 20) & 0x3ff) as f32 / 1023.0,
            ];
            let rec2020_nits = pq_rec2020.map(decode_st2084);
            let rec709_nits = [
                1.660_491 * rec2020_nits[0]
                    - 0.587_641 * rec2020_nits[1]
                    - 0.072_850 * rec2020_nits[2],
                -0.124_550 * rec2020_nits[0]
                    + 1.132_900 * rec2020_nits[1]
                    - 0.008_349 * rec2020_nits[2],
                -0.018_151 * rec2020_nits[0] - 0.100_579 * rec2020_nits[1]
                    + 1.118_730 * rec2020_nits[2],
            ];
            for linear_nits in rec709_nits {
                let normalized = (linear_nits / paper_white_nits).clamp(0.0, 1.0);
                let srgb = if normalized <= 0.003_130_8 {
                    normalized * 12.92
                } else {
                    1.055 * normalized.powf(1.0 / 2.4) - 0.055
                };
                rgba.push((srgb * 255.0).round() as u8);
            }
            rgba.push(255);
        }
    }
    Ok(rgba)
}

fn decode_st2084(encoded: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 32.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 128.0;
    const C3: f32 = 2392.0 / 128.0;
    let powered = encoded.clamp(0.0, 1.0).powf(1.0 / M2);
    let denominator = (C2 - C3 * powered).max(f32::EPSILON);
    ((powered - C1).max(0.0) / denominator).powf(1.0 / M1) * 10_000.0
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
            "requested": metadata.requested_path_space,
            "active": metadata.active_path_space,
            "plane_count": if metadata.active_path_space == "stable-planes" { 3 } else { 0 },
            "consumer": metadata.path_space_consumer,
            "allocated_bytes": metadata.stable_plane_allocated_bytes,
        },
        "streamline": {
            "sdk_version": metadata.streamline_sdk_version,
            "viewport_id": metadata.viewport_id,
        },
        "capture_source": "application_display_output",
        "frame_generation": {
            "requested": metadata.frame_generation_requested,
            "active": metadata.frame_generation_active,
        },
        "display": {
            "mode": metadata.display_mode,
            "paper_white_nits": metadata.hdr_paper_white_nits,
            "peak_nits": metadata.hdr_peak_nits,
            "display_peak_nits": metadata.display_peak_nits,
            "bits_per_color": metadata.bits_per_color,
            "capture_encoding": if metadata.display_mode == "hdr10" { "sdr-png-preview" } else { "sdr-rgba8" },
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
    fn hdr10_capture_maps_reference_white_to_sdr_and_rejects_short_rows() {
        // Encode a neutral 200-nit HDR10 pixel. Quantization may land one SDR
        // code below white, but all channels must remain neutral and opaque.
        const M1: f32 = 2610.0 / 16384.0;
        const M2: f32 = 2523.0 / 32.0;
        const C1: f32 = 3424.0 / 4096.0;
        const C2: f32 = 2413.0 / 128.0;
        const C3: f32 = 2392.0 / 128.0;
        let powered = (200.0_f32 / 10_000.0).powf(M1);
        let pq = ((C1 + C2 * powered) / (1.0 + C3 * powered)).powf(M2);
        let code = (pq * 1023.0).round() as u32;
        let pixel = (code | (code << 10) | (code << 20) | (3 << 30)).to_le_bytes();
        let rgba = unpack_hdr10_rows_to_rgba8(&pixel, 0, 4, 1, 1, 200).unwrap();
        assert!(rgba[0] >= 254);
        assert_eq!(rgba[0], rgba[1]);
        assert_eq!(rgba[1], rgba[2]);
        assert_eq!(rgba[3], 255);
        assert_eq!(
            unpack_hdr10_rows_to_rgba8(&pixel[..3], 0, 4, 1, 1, 200),
            Err(CaptureLayoutError::BufferTooSmall)
        );
        assert_eq!(
            unpack_hdr10_rows_to_rgba8(&pixel, 0, 3, 1, 1, 200),
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
            requested_path_space: "auto".to_string(),
            active_path_space: "stable-planes".to_string(),
            path_space_consumer: "nrd-stable-planes".to_string(),
            stable_plane_allocated_bytes: 123_456,
            denoiser_backend: "svgf".to_string(),
            upscaler_mode: "native".to_string(),
            reflex_mode: "unavailable".to_string(),
            frame_generation_requested: "off".to_string(),
            frame_generation_active: "unavailable".to_string(),
            display_mode: "sdr".to_string(),
            hdr_paper_white_nits: None,
            hdr_peak_nits: None,
            display_peak_nits: None,
            bits_per_color: None,
            streamline_sdk_version: None,
            viewport_id: None,
        };
        let value: serde_json::Value =
            serde_json::from_str(&capture_json_line(&metadata, 256)).unwrap();
        assert_eq!(value["schema_version"], CAPTURE_SCHEMA_VERSION);
        assert_eq!(value["path_space"]["requested"], "auto");
        assert_eq!(value["path_space"]["active"], "stable-planes");
        assert_eq!(value["debug_view"]["index"], 8);
        assert_eq!(value["modes"]["command_recording"], "optimized");
        assert_eq!(value["modes"]["denoiser"], "svgf");
        assert_eq!(value["path_space"]["consumer"], "nrd-stable-planes");
        assert_eq!(value["path_space"]["allocated_bytes"], 123_456);
        assert_eq!(value["display"]["mode"], "sdr");
        assert_eq!(value["display"]["capture_encoding"], "sdr-rgba8");
        assert_eq!(value["capture_source"], "application_display_output");
        assert_eq!(value["frame_generation"]["requested"], "off");
        assert_eq!(value["frame_generation"]["active"], "unavailable");
        assert_eq!(value["png_bytes"], 256);

        let hdr_metadata = CaptureMetadata {
            display_mode: "hdr10".to_string(),
            hdr_paper_white_nits: Some(200),
            hdr_peak_nits: Some(1_000),
            display_peak_nits: Some(1_200),
            bits_per_color: Some(10),
            ..metadata
        };
        let hdr: serde_json::Value =
            serde_json::from_str(&capture_json_line(&hdr_metadata, 512)).unwrap();
        assert_eq!(hdr["display"]["capture_encoding"], "sdr-png-preview");
        assert_eq!(hdr["display"]["paper_white_nits"], 200);
        assert_eq!(hdr["display"]["peak_nits"], 1_000);
        assert_eq!(hdr["display"]["display_peak_nits"], 1_200);
        assert_eq!(hdr["display"]["bits_per_color"], 10);
    }
}

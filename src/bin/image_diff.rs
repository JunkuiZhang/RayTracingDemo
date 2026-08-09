use std::{env, process::ExitCode};

use image::GenericImageView;

#[derive(Clone, Copy, Debug, PartialEq)]
struct DiffSummary {
    width: u32,
    height: u32,
    changed_rgb_pixels: u64,
    alpha_mismatch_count: u64,
    max_channel_abs_diff: u8,
    mae: f64,
    rmse: f64,
}

fn compare_rgba8(
    width: u32,
    height: u32,
    left: &[u8],
    right: &[u8],
) -> Result<DiffSummary, String> {
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "image dimensions overflow".to_string())?;
    let byte_count = pixel_count
        .checked_mul(4)
        .ok_or_else(|| "image byte count overflows".to_string())?;
    if left.len() != byte_count || right.len() != byte_count {
        return Err(format!(
            "RGBA byte lengths differ from dimensions: left={} right={} expected={byte_count}",
            left.len(),
            right.len()
        ));
    }

    let mut changed_rgb_pixels = 0_u64;
    let mut alpha_mismatch_count = 0_u64;
    let mut max_channel_abs_diff = 0_u8;
    let mut absolute_sum = 0_u64;
    let mut square_sum = 0_u64;
    for (left_pixel, right_pixel) in left.chunks_exact(4).zip(right.chunks_exact(4)) {
        let mut rgb_changed = false;
        for channel in 0..3 {
            let difference = left_pixel[channel].abs_diff(right_pixel[channel]);
            rgb_changed |= difference != 0;
            max_channel_abs_diff = max_channel_abs_diff.max(difference);
            absolute_sum += u64::from(difference);
            square_sum += u64::from(difference) * u64::from(difference);
        }
        changed_rgb_pixels += u64::from(rgb_changed);
        alpha_mismatch_count += u64::from(left_pixel[3] != right_pixel[3]);
    }

    let channel_count = (pixel_count * 3) as f64;
    Ok(DiffSummary {
        width,
        height,
        changed_rgb_pixels,
        alpha_mismatch_count,
        max_channel_abs_diff,
        mae: if channel_count == 0.0 {
            0.0
        } else {
            absolute_sum as f64 / channel_count
        },
        rmse: if channel_count == 0.0 {
            0.0
        } else {
            (square_sum as f64 / channel_count).sqrt()
        },
    })
}

fn compare_files(left_path: &str, right_path: &str) -> Result<DiffSummary, String> {
    let left = image::open(left_path).map_err(|error| format!("read {left_path}: {error}"))?;
    let right = image::open(right_path).map_err(|error| format!("read {right_path}: {error}"))?;
    if left.dimensions() != right.dimensions() {
        return Err(format!(
            "incompatible image dimensions: left={}x{} right={}x{}",
            left.width(),
            left.height(),
            right.width(),
            right.height()
        ));
    }
    let (width, height) = left.dimensions();
    compare_rgba8(width, height, &left.to_rgba8(), &right.to_rgba8())
}

fn summary_json(summary: DiffSummary) -> String {
    serde_json::json!({
        "width": summary.width,
        "height": summary.height,
        "changed_rgb_pixels": summary.changed_rgb_pixels,
        "alpha_mismatch_count": summary.alpha_mismatch_count,
        "max_channel_abs_diff": summary.max_channel_abs_diff,
        "mae": summary.mae,
        "rmse": summary.rmse,
    })
    .to_string()
}

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments.len() != 2 {
        eprintln!("usage: image_diff <left.png> <right.png>");
        return ExitCode::from(2);
    }
    match compare_files(&arguments[0], &arguments[1]) {
        Ok(summary) => {
            println!("{}", summary_json(summary));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("image_diff: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_images_have_zero_metrics() {
        let summary = compare_rgba8(1, 1, &[1, 2, 3, 4], &[1, 2, 3, 4]).unwrap();
        assert_eq!(summary.changed_rgb_pixels, 0);
        assert_eq!(summary.alpha_mismatch_count, 0);
        assert_eq!(summary.max_channel_abs_diff, 0);
        assert_eq!(summary.mae, 0.0);
        assert_eq!(summary.rmse, 0.0);
    }

    #[test]
    fn rgb_and_alpha_metrics_are_reported_separately() {
        let summary = compare_rgba8(
            2,
            1,
            &[0, 0, 0, 1, 10, 20, 30, 40],
            &[3, 0, 0, 2, 10, 25, 30, 40],
        )
        .unwrap();
        assert_eq!(summary.changed_rgb_pixels, 2);
        assert_eq!(summary.alpha_mismatch_count, 1);
        assert_eq!(summary.max_channel_abs_diff, 5);
        assert!((summary.mae - 8.0 / 6.0).abs() < f64::EPSILON);
        assert!((summary.rmse - (34.0_f64 / 6.0).sqrt()).abs() < f64::EPSILON);
    }

    #[test]
    fn dimensions_and_lengths_are_rejected() {
        assert!(compare_rgba8(1, 1, &[0; 4], &[0; 8]).is_err());
        assert!(compare_rgba8(0, 1, &[], &[]).is_ok());
    }

    #[test]
    fn json_is_single_line_and_parseable() {
        let summary = compare_rgba8(1, 1, &[0; 4], &[1, 0, 0, 0]).unwrap();
        let json = summary_json(summary);
        assert!(!json.contains('\n'));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["width"], 1);
        assert_eq!(value["changed_rgb_pixels"], 1);
    }
}

use std::{env, process::ExitCode};

use image::GenericImageView;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Roi {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DiffSummary {
    width: u32,
    height: u32,
    roi: Roi,
    changed_rgb_pixels: u64,
    rgb_pixels_over_2: u64,
    alpha_mismatch_count: u64,
    max_channel_abs_diff: u8,
    mean_max_channel_abs_diff: f64,
    mae: f64,
    rmse: f64,
}

fn compare_rgba8(
    width: u32,
    height: u32,
    left: &[u8],
    right: &[u8],
) -> Result<DiffSummary, String> {
    compare_rgba8_roi(
        width,
        height,
        left,
        right,
        Roi {
            x: 0,
            y: 0,
            width,
            height,
        },
    )
}

fn compare_rgba8_roi(
    width: u32,
    height: u32,
    left: &[u8],
    right: &[u8],
    roi: Roi,
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
    let roi_right = roi
        .x
        .checked_add(roi.width)
        .ok_or_else(|| "ROI horizontal extent overflows".to_string())?;
    let roi_bottom = roi
        .y
        .checked_add(roi.height)
        .ok_or_else(|| "ROI vertical extent overflows".to_string())?;
    if roi.width == 0 || roi.height == 0 || roi_right > width || roi_bottom > height {
        return Err(format!(
            "ROI x={},y={},width={},height={} is outside {}x{} image",
            roi.x, roi.y, roi.width, roi.height, width, height
        ));
    }

    let mut changed_rgb_pixels = 0_u64;
    let mut rgb_pixels_over_2 = 0_u64;
    let mut alpha_mismatch_count = 0_u64;
    let mut max_channel_abs_diff = 0_u8;
    let mut max_channel_sum = 0_u64;
    let mut absolute_sum = 0_u64;
    let mut square_sum = 0_u64;
    for y in roi.y..roi_bottom {
        for x in roi.x..roi_right {
            let offset = ((y as usize * width as usize) + x as usize) * 4;
            let left_pixel = &left[offset..offset + 4];
            let right_pixel = &right[offset..offset + 4];
            let mut rgb_changed = false;
            let mut pixel_max = 0_u8;
            for channel in 0..3 {
                let difference = left_pixel[channel].abs_diff(right_pixel[channel]);
                rgb_changed |= difference != 0;
                pixel_max = pixel_max.max(difference);
                max_channel_abs_diff = max_channel_abs_diff.max(difference);
                absolute_sum += u64::from(difference);
                square_sum += u64::from(difference) * u64::from(difference);
            }
            changed_rgb_pixels += u64::from(rgb_changed);
            rgb_pixels_over_2 += u64::from(pixel_max > 2);
            max_channel_sum += u64::from(pixel_max);
            alpha_mismatch_count += u64::from(left_pixel[3] != right_pixel[3]);
        }
    }

    let roi_pixel_count = roi.width as usize * roi.height as usize;
    let channel_count = (roi_pixel_count * 3) as f64;
    Ok(DiffSummary {
        width,
        height,
        roi,
        changed_rgb_pixels,
        rgb_pixels_over_2,
        alpha_mismatch_count,
        max_channel_abs_diff,
        mean_max_channel_abs_diff: max_channel_sum as f64 / roi_pixel_count as f64,
        mae: absolute_sum as f64 / channel_count,
        rmse: (square_sum as f64 / channel_count).sqrt(),
    })
}

fn compare_files(
    left_path: &str,
    right_path: &str,
    roi: Option<Roi>,
) -> Result<DiffSummary, String> {
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
    let left = left.to_rgba8();
    let right = right.to_rgba8();
    match roi {
        Some(roi) => compare_rgba8_roi(width, height, &left, &right, roi),
        None => compare_rgba8(width, height, &left, &right),
    }
}

fn summary_json(summary: DiffSummary) -> String {
    serde_json::json!({
        "width": summary.width,
        "height": summary.height,
        "roi": {
            "x": summary.roi.x,
            "y": summary.roi.y,
            "width": summary.roi.width,
            "height": summary.roi.height,
        },
        "changed_rgb_pixels": summary.changed_rgb_pixels,
        "rgb_pixels_over_2": summary.rgb_pixels_over_2,
        "alpha_mismatch_count": summary.alpha_mismatch_count,
        "max_channel_abs_diff": summary.max_channel_abs_diff,
        "mean_max_channel_abs_diff": summary.mean_max_channel_abs_diff,
        "mae": summary.mae,
        "rmse": summary.rmse,
    })
    .to_string()
}

fn parse_roi(value: &str) -> Result<Roi, String> {
    let fields = value
        .split(',')
        .map(|field| {
            field
                .parse::<u32>()
                .map_err(|error| format!("invalid ROI component {field:?}: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if fields.len() != 4 {
        return Err("ROI must use x,y,width,height".to_string());
    }
    Ok(Roi {
        x: fields[0],
        y: fields[1],
        width: fields[2],
        height: fields[3],
    })
}

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments.len() != 2 && arguments.len() != 4 {
        eprintln!("usage: image_diff <left.png> <right.png> [--roi x,y,width,height]");
        return ExitCode::from(2);
    }
    let roi = if arguments.len() == 4 {
        if arguments[2] != "--roi" {
            eprintln!("image_diff: expected --roi before ROI coordinates");
            return ExitCode::from(2);
        }
        match parse_roi(&arguments[3]) {
            Ok(roi) => Some(roi),
            Err(error) => {
                eprintln!("image_diff: {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        None
    };
    match compare_files(&arguments[0], &arguments[1], roi) {
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
        assert_eq!(summary.mean_max_channel_abs_diff, 0.0);
        assert_eq!(summary.rgb_pixels_over_2, 0);
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
        assert_eq!(summary.rgb_pixels_over_2, 2);
        assert_eq!(summary.mean_max_channel_abs_diff, 4.0);
        assert!((summary.mae - 8.0 / 6.0).abs() < f64::EPSILON);
        assert!((summary.rmse - (34.0_f64 / 6.0).sqrt()).abs() < f64::EPSILON);
    }

    #[test]
    fn dimensions_and_lengths_are_rejected() {
        assert!(compare_rgba8(1, 1, &[0; 4], &[0; 8]).is_err());
        assert!(compare_rgba8(0, 1, &[], &[]).is_err());
        assert!(
            compare_rgba8_roi(
                2,
                2,
                &[0; 16],
                &[0; 16],
                Roi {
                    x: 1,
                    y: 1,
                    width: 2,
                    height: 1,
                }
            )
            .is_err()
        );
    }

    #[test]
    fn roi_metrics_exclude_pixels_outside_the_region() {
        let left = [0, 0, 0, 0, 10, 10, 10, 0];
        let right = [9, 9, 9, 0, 11, 12, 13, 0];
        let summary = compare_rgba8_roi(
            2,
            1,
            &left,
            &right,
            Roi {
                x: 1,
                y: 0,
                width: 1,
                height: 1,
            },
        )
        .unwrap();
        assert_eq!(summary.changed_rgb_pixels, 1);
        assert_eq!(summary.rgb_pixels_over_2, 1);
        assert_eq!(summary.max_channel_abs_diff, 3);
        assert_eq!(summary.mean_max_channel_abs_diff, 3.0);
    }

    #[test]
    fn roi_parser_requires_four_unsigned_components() {
        assert_eq!(
            parse_roi("1,2,3,4").unwrap(),
            Roi {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            }
        );
        assert!(parse_roi("1,2,3").is_err());
        assert!(parse_roi("1,-2,3,4").is_err());
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

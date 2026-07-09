//! 烈度计算工具
//!
//! 基于震级和距离估算日本震度（JMA Seismic Intensity Scale）
//!
//! 震度等级：
//! 0: 无感
//! 1: 微震
//! 2: 轻震
//! 3: 弱震 (开始有感)
//! 4: 中震
//! 5弱/5强: 强震
//! 6弱/6强: 烈震
//! 7: 剧震

/// 估算震度（基于震级和距离）
///
/// 优化后的公式，基于日本气象厅的经验公式和实际观测数据
/// 使用改进的衰减模型：I = a * M - b * log10(D + c) + d
///
/// 其中：
/// - I: 震度（JMA Scale 0-7）
/// - M: 震级（Magnitude）
/// - D: 距离 (km)
/// - a, b, c, d: 经验系数（根据震级调整）
pub fn estimate_intensity(magnitude: f64, distance_km: f64) -> u8 {
    // 边界检查
    if magnitude <= 0.0 || distance_km < 0.0 {
        return 0;
    }

    // 极近距离特殊处理（< 1km）
    if distance_km < 1.0 {
        let intensity = (magnitude * 1.5 - 2.5).clamp(0.0, 7.0);
        return intensity.round() as u8;
    }

    let (a, b, c, d) = intensity_coefficients(magnitude);

    // 计算震度
    let intensity = a * magnitude - b * (distance_km + c).log10() + d;

    // 限制在 0-7 范围内并四舍五入
    intensity.clamp(0.0, 7.0).round() as u8
}

fn intensity_coefficients(magnitude: f64) -> (f64, f64, f64, f64) {
    let small = (2.5, 3.8, 12.0, -1.2);
    let medium = (2.5, 3.6, 10.0, -1.3);
    let strong = (2.3, 3.7, 10.0, -1.0);
    let major = (2.0, 3.8, 10.0, -0.8);

    if magnitude < 4.8 {
        small
    } else if magnitude < 5.2 {
        blend_coefficients(small, medium, (magnitude - 4.8) / 0.4)
    } else if magnitude < 5.8 {
        medium
    } else if magnitude < 6.2 {
        blend_coefficients(medium, strong, (magnitude - 5.8) / 0.4)
    } else if magnitude < 6.8 {
        strong
    } else if magnitude < 7.2 {
        blend_coefficients(strong, major, (magnitude - 6.8) / 0.4)
    } else {
        major
    }
}

fn blend_coefficients(
    left: (f64, f64, f64, f64),
    right: (f64, f64, f64, f64),
    t: f64,
) -> (f64, f64, f64, f64) {
    (
        lerp(left.0, right.0, t),
        lerp(left.1, right.1, t),
        lerp(left.2, right.2, t),
        lerp(left.3, right.3, t),
    )
}

fn lerp(left: f64, right: f64, t: f64) -> f64 {
    left + (right - left) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_intensity() {
        // M7.0 震级，10km 距离，应该是高烈度
        let i1 = estimate_intensity(7.0, 10.0);
        assert!(i1 >= 5);

        // M7.0 震级，100km 距离，烈度应该降低
        let i2 = estimate_intensity(7.0, 100.0);
        assert!(i2 < i1);

        // M5.0 震级，50km 距离
        let i3 = estimate_intensity(5.0, 50.0);
        assert!((1..=5).contains(&i3));

        // M4.0 震级，10km 距离
        let i4 = estimate_intensity(4.0, 10.0);
        assert!(i4 <= i3);
    }

    #[test]
    fn test_magnitude_boundary_smoothing() {
        let before = estimate_intensity(4.9, 240.0);
        let after = estimate_intensity(5.0, 240.0);
        assert!(after.saturating_sub(before) <= 1);

        assert_eq!(estimate_intensity(5.1, 280.0), 2);
        assert_eq!(estimate_intensity(4.8, 280.0), 1);
    }
}

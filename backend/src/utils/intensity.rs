/// 烈度计算工具
///
/// 基于震级和距离估算日本震度（JMA Seismic Intensity Scale）
///
/// 震度等级：
/// 0: 无感
/// 1: 微震
/// 2: 轻震
/// 3: 弱震 (开始有感)
/// 4: 中震
/// 5弱/5强: 强震
/// 6弱/6强: 烈震
/// 7: 剧震

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
        let intensity = (magnitude * 1.5 - 2.5).max(0.0).min(7.0);
        return intensity.round() as u8;
    }

    // 优化后的系数：根据震级范围动态调整（更加保守的系数避免过度估算）
    let (a, b, c, d) = if magnitude >= 7.0 {
        // 大地震：M ≥ 7.0（更保守的系数确保不会过度估算）
        (2.0, 3.8, 10.0, -0.8)
    } else if magnitude >= 6.0 {
        // 强震：6.0 ≤ M < 7.0
        (2.3, 3.7, 10.0, -1.0)
    } else if magnitude >= 5.0 {
        // 中震：5.0 ≤ M < 6.0
        (2.5, 3.6, 10.0, -1.3)
    } else {
        // 小震：M < 5.0
        (2.5, 3.8, 12.0, -1.2)
    };

    // 计算震度
    let intensity = a * magnitude - b * (distance_km + c).log10() + d;

    // 限制在 0-7 范围内并四舍五入
    intensity.max(0.0).min(7.0).round() as u8
}

/// 验证烈度阈值是否有效
pub fn validate_intensity(intensity: u8) -> bool {
    intensity <= 7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_intensity() {
        // M7.0 震级，10km 距离，应该是高烈度
        let i1 = estimate_intensity(7.0, 10.0);
        assert!(i1 >= 5);
        println!("M7.0, 10km: 震度 {}", i1);

        // M7.0 震级，100km 距离，烈度应该降低
        let i2 = estimate_intensity(7.0, 100.0);
        assert!(i2 < i1);
        println!("M7.0, 100km: 震度 {}", i2);

        // M5.0 震级，50km 距离
        let i3 = estimate_intensity(5.0, 50.0);
        println!("M5.0, 50km: 震度 {}", i3);

        // M4.0 震级，10km 距离
        let i4 = estimate_intensity(4.0, 10.0);
        println!("M4.0, 10km: 震度 {}", i4);
    }

    #[test]
    fn test_validate_intensity() {
        assert!(validate_intensity(0));
        assert!(validate_intensity(7));
        assert!(!validate_intensity(8));
    }
}

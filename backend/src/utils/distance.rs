/// Vincenty 反解公式 - 高精度地理距离计算（优化版）
///
/// 基于 WGS84 椭球体模型
/// 精度：约 0.5mm（对于大多数点对）
///
/// # 参数
/// * `lat1`, `lon1` - 起点纬度和经度（度）
/// * `lat2`, `lon2` - 终点纬度和经度（度）
///
/// # 返回值
/// * `Some(distance)` - 距离（千米）
/// * `None` - 计算失败（对跖点或无效输入）
#[inline]
pub fn vincenty_distance(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> Option<f64> {
    // WGS84 椭球体参数
    const A: f64 = 6378137.0; // 赤道半径 (m)
    const B: f64 = 6356752.314245; // 极地半径 (m)
    const F: f64 = 1.0 / 298.257223563; // 扁率
    const A_SQ: f64 = A * A;
    const B_SQ: f64 = B * B;
    const TOLERANCE: f64 = 1e-12; // 收敛阈值
    const EPSILON: f64 = 1e-24; // 重合点判定阈值

    // 输入验证
    if !(-90.0..=90.0).contains(&lat1) || !(-90.0..=90.0).contains(&lat2) {
        return None;
    }
    if !(-180.0..=180.0).contains(&lon1) || !(-180.0..=180.0).contains(&lon2) {
        return None;
    }

    // 快速路径：相同点检查（同时检查纬度和经度）
    let lat_diff = (lat1 - lat2).abs();
    let mut lon_diff = (lon1 - lon2).abs();

    // 处理跨越 180° 的情况
    if lon_diff > 180.0 {
        lon_diff = 360.0 - lon_diff;
    }

    if lat_diff < 1e-9 && lon_diff < 1e-9 {
        return Some(0.0);
    }

    // 转换为弧度
    let lat1_rad = lat1.to_radians();
    let lat2_rad = lat2.to_radians();
    let lon1_rad = lon1.to_radians();
    let lon2_rad = lon2.to_radians();

    let l = lon2_rad - lon1_rad;

    // 计算归化纬度
    let tan_u1 = (1.0 - F) * lat1_rad.tan();
    let tan_u2 = (1.0 - F) * lat2_rad.tan();
    let u1 = tan_u1.atan();
    let u2 = tan_u2.atan();

    // 预计算三角函数值
    let sin_u1 = u1.sin();
    let cos_u1 = u1.cos();
    let sin_u2 = u2.sin();
    let cos_u2 = u2.cos();

    // 迭代计算
    let mut lambda = l;
    let mut iter_limit = 100;

    let (sin_sigma, cos_sigma, sigma, cos_sq_alpha, cos2_sigma_m) = loop {
        let sin_lambda = lambda.sin();
        let cos_lambda = lambda.cos();

        // 计算 sin(σ)
        let term1 = cos_u2 * sin_lambda;
        let term2 = cos_u1 * sin_u2 - sin_u1 * cos_u2 * cos_lambda;
        let sin_sq_sigma = term1 * term1 + term2 * term2;

        // 只有当经度差也很小时才判定为重合点
        if sin_sq_sigma < EPSILON && lon_diff < 1e-6 {
            return Some(0.0); // 真正的重合点
        }

        if sin_sq_sigma < EPSILON {
            // 对跖点情况：sin_sq_sigma 接近 0 但不是重合点
            return None;
        }

        let sin_sigma = sin_sq_sigma.sqrt();
        let cos_sigma = sin_u1 * sin_u2 + cos_u1 * cos_u2 * cos_lambda;
        let sigma = sin_sigma.atan2(cos_sigma);

        // 计算 sin(α)
        let sin_alpha = cos_u1 * cos_u2 * sin_lambda / sin_sigma;
        let cos_sq_alpha = 1.0 - sin_alpha * sin_alpha;

        // 处理赤道线情况
        let cos2_sigma_m = if cos_sq_alpha != 0.0 {
            cos_sigma - 2.0 * sin_u1 * sin_u2 / cos_sq_alpha
        } else {
            0.0
        };

        // 计算 λ
        let c = F / 16.0 * cos_sq_alpha * (4.0 + F * (4.0 - 3.0 * cos_sq_alpha));
        let cos2_sigma_m_sq = cos2_sigma_m * cos2_sigma_m;

        let lambda_new = l
            + (1.0 - c)
                * F
                * sin_alpha
                * (sigma
                    + c * sin_sigma
                        * (cos2_sigma_m + c * cos_sigma * (-1.0 + 2.0 * cos2_sigma_m_sq)));

        // 收敛检查
        if (lambda_new - lambda).abs() < TOLERANCE {
            break (sin_sigma, cos_sigma, sigma, cos_sq_alpha, cos2_sigma_m);
        }

        lambda = lambda_new;
        iter_limit -= 1;

        if iter_limit == 0 {
            return None; // 未收敛（对跖点情况）
        }
    };

    // 计算距离
    let u_sq = cos_sq_alpha * (A_SQ - B_SQ) / B_SQ;

    // 优化：减少重复计算
    let u_sq_div_16384 = u_sq / 16384.0;
    let u_sq_div_1024 = u_sq / 1024.0;

    let k1 = u_sq * (-768.0 + u_sq * (320.0 - 175.0 * u_sq));
    let big_a = 1.0 + u_sq_div_16384 * (4096.0 + k1);

    let k2 = u_sq * (-128.0 + u_sq * (74.0 - 47.0 * u_sq));
    let big_b = u_sq_div_1024 * (256.0 + k2);

    let cos2_sigma_m_sq = cos2_sigma_m * cos2_sigma_m;
    let sin_sigma_sq = sin_sigma * sin_sigma;

    let delta_sigma = big_b
        * sin_sigma
        * (cos2_sigma_m
            + 0.25
                * big_b
                * (cos_sigma * (-1.0 + 2.0 * cos2_sigma_m_sq)
                    - big_b / 6.0
                        * cos2_sigma_m
                        * (-3.0 + 4.0 * sin_sigma_sq)
                        * (-3.0 + 4.0 * cos2_sigma_m_sq)));

    let s = B * big_a * (sigma - delta_sigma);

    Some(s / 1000.0) // 转换为 km
}

/// 验证经纬度是否有效
pub fn validate_coordinates(lat: f64, lon: f64) -> bool {
    lat >= -90.0 && lat <= 90.0 && lon >= -180.0 && lon <= 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_point() {
        let dist = vincenty_distance(0.0, 0.0, 0.0, 0.0);
        assert_eq!(dist, Some(0.0));
    }

    #[test]
    fn test_short_distance() {
        // 北京到上海 约 1067 km
        let dist = vincenty_distance(39.9042, 116.4074, 31.2304, 121.4737).unwrap();
        assert!((dist - 1067.0).abs() < 2.0);
    }

    #[test]
    fn test_long_distance() {
        // 纽约 (JFK) 到伦敦 (LHR) 约 5555 km (使用 Vincenty 公式)
        let dist = vincenty_distance(40.6413, -73.7781, 51.4700, -0.4543).unwrap();
        assert!(
            (dist - 5555.0).abs() < 2.0,
            "Expected ~5555 km, got {} km",
            dist
        );
    }

    #[test]
    fn test_antipodal_points() {
        // 对跖点：赤道上相距 180° 的两点
        // 距离应约为 20037 km（地球赤道半周长）
        let dist = vincenty_distance(0.0, 0.0, 0.0, 180.0);

        // Vincenty 算法在对跖点附近可能不收敛
        // 如果收敛，距离应接近 20000 km
        match dist {
            Some(d) => {
                assert!(
                    d > 19900.0 && d < 20100.0,
                    "Antipodal distance should be ~20000 km, got {} km",
                    d
                );
            }
            None => {
                // 对跖点可能不收敛，这也是可接受的
                println!("Vincenty did not converge for antipodal points (expected behavior)");
            }
        }
    }

    #[test]
    fn test_near_antipodal() {
        // 测试接近对跖点但不完全对跖的情况
        // 马德里 (40.4168, -3.7038) 的对跖点约在新西兰附近
        // 新西兰惠灵顿 (-41.2865, 174.7762)
        let dist = vincenty_distance(40.4168, -3.7038, -41.2865, 174.7762);

        if let Some(d) = dist {
            // 这应该是一个非常长的距离，接近但小于 20000 km
            assert!(
                d > 19000.0 && d < 20000.0,
                "Near-antipodal distance should be 19000-20000 km, got {} km",
                d
            );
        }
    }

    #[test]
    fn test_invalid_coordinates() {
        assert_eq!(vincenty_distance(91.0, 0.0, 0.0, 0.0), None);
        assert_eq!(vincenty_distance(0.0, 181.0, 0.0, 0.0), None);
        assert_eq!(vincenty_distance(0.0, 0.0, -91.0, 0.0), None);
        assert_eq!(vincenty_distance(0.0, 0.0, 0.0, -181.0), None);
    }

    #[test]
    fn test_across_prime_meridian() {
        // 伦敦到巴黎
        let dist = vincenty_distance(51.5074, -0.1278, 48.8566, 2.3522).unwrap();
        assert!((dist - 344.0).abs() < 2.0);
    }

    #[test]
    fn test_across_date_line() {
        // 跨越国际日期变更线
        let dist = vincenty_distance(0.0, 179.0, 0.0, -179.0).unwrap();
        assert!(dist < 250.0); // 应该是短距离，不是绕地球
    }

    #[test]
    fn test_validate_coordinates() {
        assert!(validate_coordinates(35.6762, 139.6503));
        assert!(!validate_coordinates(91.0, 0.0));
        assert!(!validate_coordinates(0.0, 181.0));
    }
}

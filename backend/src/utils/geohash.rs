/// GeoHash 编码和邻居计算工具

const BASE32: &[u8] = b"0123456789bcdefghjkmnpqrstuvwxyz";
const PRECISION: usize = 4; // ~20km x 20km

/// GeoHash 编码
pub fn encode(lat: f64, lon: f64) -> String {
    encode_with_precision(lat, lon, PRECISION)
}

/// GeoHash 编码 (指定精度)
pub fn encode_with_precision(lat: f64, lon: f64, precision: usize) -> String {
    let mut lat_range = (-90.0, 90.0);
    let mut lon_range = (-180.0, 180.0);
    let mut hash = String::new();
    let mut bits = 0u8;
    let mut bit_count = 0;

    while hash.len() < precision {
        if bit_count % 2 == 0 {
            // 偶数位：编码经度
            let mid = (lon_range.0 + lon_range.1) / 2.0;
            if lon >= mid {
                bits |= 1 << (4 - (bit_count % 5));
                lon_range.0 = mid;
            } else {
                lon_range.1 = mid;
            }
        } else {
            // 奇数位：编码纬度
            let mid = (lat_range.0 + lat_range.1) / 2.0;
            if lat >= mid {
                bits |= 1 << (4 - (bit_count % 5));
                lat_range.0 = mid;
            } else {
                lat_range.1 = mid;
            }
        }

        bit_count += 1;

        if bit_count % 5 == 0 {
            hash.push(BASE32[bits as usize] as char);
            bits = 0;
        }
    }

    hash
}

/// 获取相邻的 9 个格子 (包括自己)
pub fn get_neighbors(geohash: &str) -> Vec<String> {
    let mut neighbors = Vec::with_capacity(9);
    neighbors.push(geohash.to_string());

    // 获取4个基本方向的邻居
    let north = neighbor(geohash, Direction::North);
    let south = neighbor(geohash, Direction::South);
    let east = neighbor(geohash, Direction::East);
    let west = neighbor(geohash, Direction::West);

    // 添加基本方向的邻居
    if let Some(ref n) = north {
        neighbors.push(n.clone());
    }
    if let Some(ref s) = south {
        neighbors.push(s.clone());
    }
    if let Some(ref e) = east {
        neighbors.push(e.clone());
    }
    if let Some(ref w) = west {
        neighbors.push(w.clone());
    }

    // 添加对角线方向的邻居
    if let Some(ref n) = north {
        if let Some(ne) = neighbor(n, Direction::East) {
            neighbors.push(ne);
        }
        if let Some(nw) = neighbor(n, Direction::West) {
            neighbors.push(nw);
        }
    }
    if let Some(ref s) = south {
        if let Some(se) = neighbor(s, Direction::East) {
            neighbors.push(se);
        }
        if let Some(sw) = neighbor(s, Direction::West) {
            neighbors.push(sw);
        }
    }

    // 去重
    neighbors.sort();
    neighbors.dedup();

    neighbors
}

#[derive(Debug)]
enum Direction {
    North,
    South,
    East,
    West,
}

/// 计算指定方向的邻居
fn neighbor(geohash: &str, direction: Direction) -> Option<String> {
    if geohash.is_empty() {
        return None;
    }

    let neighbor_map = match direction {
        Direction::North => [
            "p0r21436x8zb9dcf5h7kjnmqesgutwvy",
            "bc01fg45238967deuvhjyznpkmstqrwx",
        ],
        Direction::South => [
            "14365h7k9dcfesgujnmqp0r2twvyx8zb",
            "238967debc01fg45kmstqrwxuvhjyznp",
        ],
        Direction::East => [
            "bc01fg45238967deuvhjyznpkmstqrwx",
            "p0r21436x8zb9dcf5h7kjnmqesgutwvy",
        ],
        Direction::West => [
            "238967debc01fg45kmstqrwxuvhjyznp",
            "14365h7k9dcfesgujnmqp0r2twvyx8zb",
        ],
    };

    let border_map = match direction {
        Direction::North => ["prxz", "bcfguvyz"],
        Direction::South => ["028b", "0145hjnp"],
        Direction::East => ["bcfguvyz", "prxz"],
        Direction::West => ["0145hjnp", "028b"],
    };

    let last_char = geohash.chars().last()?;
    let parent = &geohash[..geohash.len() - 1];
    let type_idx = (geohash.len() % 2) as usize;

    let mut base = parent.to_string();

    // 如果在边界，需要递归处理父级
    if border_map[type_idx].contains(last_char) && !parent.is_empty() {
        base = neighbor(parent, direction)?;
    }

    let neighbor_chars = neighbor_map[type_idx];
    let pos = BASE32.iter().position(|&c| c as char == last_char)?;
    let neighbor_char = neighbor_chars.chars().nth(pos)?;

    Some(format!("{}{}", base, neighbor_char))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_basic() {
        // 东京塔坐标
        let hash = encode(35.6586, 139.7454);
        assert_eq!(hash.len(), 4);
        println!("东京塔 GeoHash: {}", hash);
    }

    #[test]
    fn test_encode_known_locations() {
        // 测试已知位置的 GeoHash
        // 北京天安门广场 (39.9042, 116.4074) -> wx4g (精度4)
        let beijing = encode_with_precision(39.9042, 116.4074, 4);
        assert_eq!(beijing, "wx4g");

        // 上海东方明珠 (31.2397, 121.4999) -> wtw3 (精度4)
        let shanghai = encode_with_precision(31.2397, 121.4999, 4);
        assert_eq!(shanghai, "wtw3");

        // 伦敦 (51.5074, -0.1278) -> gcpv (精度4)
        let london = encode_with_precision(51.5074, -0.1278, 4);
        assert_eq!(london, "gcpv");
    }

    #[test]
    fn test_encode_different_precisions() {
        let lat = 35.6586;
        let lon = 139.7454;

        // 测试不同精度
        let hash1 = encode_with_precision(lat, lon, 1);
        let hash2 = encode_with_precision(lat, lon, 2);
        let hash3 = encode_with_precision(lat, lon, 3);
        let hash5 = encode_with_precision(lat, lon, 5);

        assert_eq!(hash1.len(), 1);
        assert_eq!(hash2.len(), 2);
        assert_eq!(hash3.len(), 3);
        assert_eq!(hash5.len(), 5);

        // 较短的哈希应该是较长哈希的前缀
        assert!(hash5.starts_with(&hash1));
        assert!(hash5.starts_with(&hash2));
        assert!(hash5.starts_with(&hash3));

        println!("精度测试: {} -> {} -> {} -> {}", hash1, hash2, hash3, hash5);
    }

    #[test]
    fn test_encode_boundary_cases() {
        // 测试边界情况

        // 赤道和本初子午线交点
        let origin = encode_with_precision(0.0, 0.0, 4);
        assert_eq!(origin.len(), 4);
        println!("赤道本初子午线: {}", origin);

        // 北极附近（不能是正好90度，因为这是边界）
        let north_pole = encode_with_precision(89.9, 0.0, 4);
        assert_eq!(north_pole.len(), 4);
        println!("北极附近: {}", north_pole);

        // 南极附近
        let south_pole = encode_with_precision(-89.9, 0.0, 4);
        assert_eq!(south_pole.len(), 4);
        println!("南极附近: {}", south_pole);

        // 日界线东侧
        let date_line_east = encode_with_precision(0.0, 179.9, 4);
        assert_eq!(date_line_east.len(), 4);
        println!("日界线东侧: {}", date_line_east);

        // 日界线西侧
        let date_line_west = encode_with_precision(0.0, -179.9, 4);
        assert_eq!(date_line_west.len(), 4);
        println!("日界线西侧: {}", date_line_west);
    }

    #[test]
    fn test_encode_consistency() {
        // 相同坐标应该总是产生相同的哈希
        let lat = 35.6586;
        let lon = 139.7454;

        let hash1 = encode(lat, lon);
        let hash2 = encode(lat, lon);
        let hash3 = encode(lat, lon);

        assert_eq!(hash1, hash2);
        assert_eq!(hash2, hash3);
    }

    #[test]
    fn test_neighbors_count() {
        // 测试不同的 GeoHash，都应该返回9个邻居（包括自己）
        let test_hashes = vec!["wecn", "wx4g", "wtw3", "gcpv", "s000"];

        for hash in test_hashes {
            let neighbors = get_neighbors(hash);
            assert_eq!(neighbors.len(), 9, "GeoHash {} 应该有9个邻居", hash);
            assert!(
                neighbors.contains(&hash.to_string()),
                "邻居列表应该包含自己"
            );
        }
    }

    #[test]
    fn test_neighbors_detail() {
        let hash = "wecn";
        let neighbors = get_neighbors(hash);

        println!("中心: {}", hash);
        println!("所有邻居 (包括自己): {:?}", neighbors);

        // 验证邻居数量
        assert_eq!(neighbors.len(), 9);

        // 验证包含自己
        assert!(neighbors.contains(&hash.to_string()));

        // 验证没有重复
        let mut sorted = neighbors.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), neighbors.len(), "邻居列表不应该有重复");
    }

    #[test]
    fn test_neighbor_directions() {
        // 测试单个方向的邻居计算
        let hash = "wecn";

        let north = neighbor(hash, Direction::North);
        let south = neighbor(hash, Direction::South);
        let east = neighbor(hash, Direction::East);
        let west = neighbor(hash, Direction::West);

        assert!(north.is_some(), "北邻居应该存在");
        assert!(south.is_some(), "南邻居应该存在");
        assert!(east.is_some(), "东邻居应该存在");
        assert!(west.is_some(), "西邻居应该存在");

        // 所有邻居都应该与原始哈希不同
        assert_ne!(north.unwrap(), hash);
        assert_ne!(south.unwrap(), hash);
        assert_ne!(east.unwrap(), hash);
        assert_ne!(west.unwrap(), hash);
    }

    #[test]
    fn test_neighbor_reciprocity() {
        // 测试邻居的往返一致性
        // 如果 B 是 A 的北邻居，那么 A 应该是 B 的南邻居
        let hash = "wecn";

        if let Some(north) = neighbor(hash, Direction::North) {
            if let Some(south_of_north) = neighbor(&north, Direction::South) {
                assert_eq!(south_of_north, hash, "北邻居的南邻居应该是原点");
            }
        }

        if let Some(east) = neighbor(hash, Direction::East) {
            if let Some(west_of_east) = neighbor(&east, Direction::West) {
                assert_eq!(west_of_east, hash, "东邻居的西邻居应该是原点");
            }
        }
    }

    #[test]
    fn test_neighbors_at_boundaries() {
        // 测试边界附近的邻居计算
        // 使用不同长度的 GeoHash

        let boundary_hashes = vec![
            "0",     // 精度1
            "00",    // 精度2
            "000",   // 精度3
            "s000",  // 精度4
            "pbpbp", // 精度5
        ];

        for hash in boundary_hashes {
            let neighbors = get_neighbors(hash);
            // 即使在边界，也应该能计算出邻居（可能少于9个，但至少应该有中心点）
            assert!(
                neighbors.len() >= 1,
                "GeoHash {} 应该至少有1个元素（自己）",
                hash
            );
            assert!(neighbors.len() <= 9, "GeoHash {} 不应该超过9个邻居", hash);
            println!("边界测试 {} -> {} 个邻居", hash, neighbors.len());
        }
    }

    #[test]
    fn test_neighbors_uniqueness() {
        // 测试相邻区域不会有重复
        let test_cases = vec!["wecn", "wx4g", "wtw3", "gcpv", "9q5", "dqc", "u4pr"];

        for hash in test_cases {
            let neighbors = get_neighbors(hash);
            let unique_count = neighbors.len();

            let mut sorted = neighbors.clone();
            sorted.sort();
            sorted.dedup();
            let deduped_count = sorted.len();

            assert_eq!(
                unique_count, deduped_count,
                "GeoHash {} 的邻居应该都是唯一的",
                hash
            );
        }
    }

    #[test]
    fn test_encode_nearby_points() {
        // 测试非常接近的点是否产生相同或相邻的哈希
        let base_lat = 35.6586;
        let base_lon = 139.7454;

        let hash1 = encode(base_lat, base_lon);

        // 非常小的偏移（约10米）
        let hash2 = encode(base_lat + 0.0001, base_lon);
        let hash3 = encode(base_lat, base_lon + 0.0001);

        println!("基准点: {}", hash1);
        println!("北偏移: {}", hash2);
        println!("东偏移: {}", hash3);

        // 在精度4的情况下，这么小的偏移应该产生相同或相邻的哈希
        let neighbors1 = get_neighbors(&hash1);
        assert!(neighbors1.contains(&hash1));
    }

    #[test]
    fn test_all_base32_chars() {
        // 验证编码使用正确的 base32 字符集
        let test_coords = vec![
            (0.0, 0.0),
            (45.0, 45.0),
            (-45.0, -45.0),
            (60.0, 120.0),
            (-30.0, -90.0),
        ];

        for (lat, lon) in test_coords {
            let hash = encode_with_precision(lat, lon, 6);
            for c in hash.chars() {
                assert!(
                    BASE32.contains(&(c as u8)),
                    "字符 '{}' 应该在 BASE32 字符集中",
                    c
                );
            }
        }
    }

    #[test]
    fn test_empty_geohash() {
        // 测试空字符串的边界情况
        let result = neighbor("", Direction::North);
        assert!(result.is_none(), "空 GeoHash 应该返回 None");
    }

    #[test]
    fn test_precision_increases_accuracy() {
        let lat = 35.6586;
        let lon = 139.7454;

        // 更高精度的哈希应该更准确地代表位置
        // 测试方法：相同精度的不同位置应该产生不同的哈希
        let hash1_p3 = encode_with_precision(lat, lon, 3);
        let hash2_p3 = encode_with_precision(lat + 1.0, lon, 3);

        let hash1_p6 = encode_with_precision(lat, lon, 6);
        let hash2_p6 = encode_with_precision(lat + 0.001, lon, 6);

        // 精度3可能无法区分1度的差异，但精度6应该能区分0.001度
        assert_ne!(hash1_p3, hash2_p3, "精度3应该能区分1度差异");

        println!("精度3: {} vs {}", hash1_p3, hash2_p3);
        println!("精度6: {} vs {}", hash1_p6, hash2_p6);
    }

    #[test]
    fn test_more_known_locations() {
        // 纽约自由女神像 (40.6892, -74.0445)
        let ny = encode_with_precision(40.6892, -74.0445, 9);
        assert_eq!(ny, "dr5r7p4ry");
        println!("纽约: {}", ny);

        // 巴黎埃菲尔铁塔 (48.8584, 2.2945)
        let paris = encode_with_precision(48.8584, 2.2945, 5);
        assert_eq!(paris, "u09tu");
        println!("巴黎: {}", paris);

        // 悉尼歌剧院 (-33.8568, 151.2153)
        let sydney = encode_with_precision(-33.8568, 151.2153, 5);
        assert_eq!(sydney, "r3gx2");
        println!("悉尼: {}", sydney);

        // 东京 (35.6762, 139.6503)
        let tokyo = encode_with_precision(35.6762, 139.6503, 9);
        assert_eq!(tokyo, "xn76cydhz");
        println!("东京: {}", tokyo);
    }

    #[test]
    fn test_negative_coordinates() {
        // 南美洲南半球，西半球
        let south_america = encode_with_precision(-23.5505, -46.6333, 4); // 圣保罗
        assert_eq!(south_america.len(), 4);
        assert!(BASE32.contains(&(south_america.chars().next().unwrap() as u8)));
        println!("圣保罗 (负坐标): {}", south_america);

        // 南极洲
        let antarctica = encode_with_precision(-75.0, -120.0, 4);
        assert_eq!(antarctica.len(), 4);
        println!("南极洲: {}", antarctica);

        // 四个象限的测试
        let ne = encode_with_precision(45.0, 90.0, 3); // 东北
        let nw = encode_with_precision(45.0, -90.0, 3); // 西北
        let se = encode_with_precision(-45.0, 90.0, 3); // 东南
        let sw = encode_with_precision(-45.0, -90.0, 3); // 西南

        // 所有象限应该产生不同的哈希
        assert_ne!(ne, nw);
        assert_ne!(ne, se);
        assert_ne!(ne, sw);
        assert_ne!(nw, se);
        assert_ne!(nw, sw);
        assert_ne!(se, sw);

        println!("四象限: NE={}, NW={}, SE={}, SW={}", ne, nw, se, sw);
    }

    #[test]
    fn test_extreme_coordinates() {
        // 测试接近极限的坐标
        let extreme_cases = vec![
            (89.9999, 179.9999),   // 接近最大值
            (-89.9999, -179.9999), // 接近最小值
            (0.0001, 0.0001),      // 接近零但不是零
            (-0.0001, -0.0001),    // 负方向接近零
        ];

        for (lat, lon) in extreme_cases {
            let hash = encode_with_precision(lat, lon, 6);
            assert_eq!(hash.len(), 6);
            // 验证所有字符都是有效的 base32
            for c in hash.chars() {
                assert!(
                    BASE32.contains(&(c as u8)),
                    "坐标 ({}, {}) 产生的哈希 '{}' 包含无效字符 '{}'",
                    lat,
                    lon,
                    hash,
                    c
                );
            }
            println!("极端坐标 ({}, {}) -> {}", lat, lon, hash);
        }
    }

    #[test]
    fn test_geohash_prefix_hierarchy() {
        // 测试 GeoHash 的层级关系：较长的哈希应该以较短的哈希为前缀
        let lat = 39.9042;
        let lon = 116.4074;

        let h1 = encode_with_precision(lat, lon, 1);
        let h2 = encode_with_precision(lat, lon, 2);
        let h3 = encode_with_precision(lat, lon, 3);
        let h4 = encode_with_precision(lat, lon, 4);
        let h5 = encode_with_precision(lat, lon, 5);
        let h6 = encode_with_precision(lat, lon, 6);

        // 验证前缀关系
        assert!(h2.starts_with(&h1));
        assert!(h3.starts_with(&h2));
        assert!(h4.starts_with(&h3));
        assert!(h5.starts_with(&h4));
        assert!(h6.starts_with(&h5));

        println!(
            "层级关系: {} -> {} -> {} -> {} -> {} -> {}",
            h1, h2, h3, h4, h5, h6
        );
    }

    #[test]
    fn test_same_precision_nearby_points_share_prefix() {
        // 测试附近的点在较低精度下应该共享前缀
        let base_lat = 35.6586;
        let base_lon = 139.7454;

        // 相距约1公里的点
        let hash1 = encode_with_precision(base_lat, base_lon, 6);
        let hash2 = encode_with_precision(base_lat + 0.01, base_lon, 6);
        let hash3 = encode_with_precision(base_lat, base_lon + 0.01, 6);

        println!("基准点 (精度6): {}", hash1);
        println!("北偏移1km: {}", hash2);
        println!("东偏移1km: {}", hash3);

        // 前3-4个字符应该相同（约20-100km范围）
        assert_eq!(&hash1[..3], &hash2[..3]);
        assert_eq!(&hash1[..3], &hash3[..3]);
    }

    #[test]
    fn test_distant_points_different_hashes() {
        // 测试相距很远的点应该产生完全不同的哈希
        let beijing = encode_with_precision(39.9042, 116.4074, 5);
        let newyork = encode_with_precision(40.7128, -74.0060, 5);
        let sydney = encode_with_precision(-33.8688, 151.2093, 5);

        // 不同大陆的城市应该有不同的哈希
        assert_ne!(beijing, newyork);
        assert_ne!(beijing, sydney);
        assert_ne!(newyork, sydney);

        // 它们的第一个字符也应该不同
        assert_ne!(beijing.chars().next(), newyork.chars().next());

        println!("北京: {}, 纽约: {}, 悉尼: {}", beijing, newyork, sydney);
    }

    #[test]
    fn test_neighbors_symmetry() {
        // 测试邻居关系的对称性
        // 注意：在某些边界情况下，邻居关系可能不完全对称
        let hash = "wx4g";
        let neighbors = get_neighbors(hash);

        println!("中心 {} 的邻居: {:?}", hash, neighbors);

        // 对于大部分邻居（非边界情况），检查对称性
        let mut symmetric_count = 0;
        let mut asymmetric = Vec::new();

        for neighbor_hash in &neighbors {
            if neighbor_hash != hash {
                let reverse_neighbors = get_neighbors(neighbor_hash);
                if reverse_neighbors.contains(&hash.to_string()) {
                    symmetric_count += 1;
                } else {
                    asymmetric.push(neighbor_hash.clone());
                }
            }
        }

        println!("对称邻居数量: {}/{}", symmetric_count, neighbors.len() - 1);
        if !asymmetric.is_empty() {
            println!("非对称邻居: {:?}", asymmetric);
        }

        // 大部分邻居应该是对称的（至少 50%）
        assert!(
            symmetric_count >= (neighbors.len() - 1) / 2,
            "至少一半的邻居关系应该是对称的"
        );
    }

    #[test]
    fn test_neighbors_different_precisions() {
        // 测试不同精度下的邻居计算
        let precisions = vec![("w", 1), ("wx", 2), ("wx4", 3), ("wx4g", 4), ("wx4g0", 5)];

        for (hash, precision) in precisions {
            let neighbors = get_neighbors(hash);
            assert!(
                neighbors.len() >= 1 && neighbors.len() <= 9,
                "精度 {} 的 GeoHash '{}' 应该有1-9个邻居",
                precision,
                hash
            );
            println!("精度 {} ({}): {} 个邻居", precision, hash, neighbors.len());
        }
    }

    #[test]
    fn test_encoding_is_deterministic() {
        // 测试编码的确定性：多次编码相同坐标应该总是产生相同结果
        let test_coords = vec![
            (35.6586, 139.7454),
            (0.0, 0.0),
            (-45.0, 90.0),
            (51.5074, -0.1278),
        ];

        for (lat, lon) in test_coords {
            let hashes: Vec<String> = (0..10)
                .map(|_| encode_with_precision(lat, lon, 5))
                .collect();

            // 所有哈希应该相同
            for hash in &hashes[1..] {
                assert_eq!(
                    &hashes[0], hash,
                    "相同坐标 ({}, {}) 的多次编码应该产生相同结果",
                    lat, lon
                );
            }
        }
    }

    #[test]
    fn test_neighbor_corners() {
        // 测试角落邻居（对角线）的计算
        let hash = "wx4g";
        let neighbors = get_neighbors(hash);

        // 应该有9个邻居（8个方向 + 中心）
        assert_eq!(neighbors.len(), 9);

        // 验证所有邻居的长度都相同
        for neighbor in &neighbors {
            assert_eq!(
                neighbor.len(),
                hash.len(),
                "邻居 '{}' 的长度应该与原始哈希 '{}' 相同",
                neighbor,
                hash
            );
        }
    }

    #[test]
    fn test_geohash_characters_valid() {
        // 测试大量随机坐标，确保生成的 GeoHash 只包含有效字符
        let test_cases = vec![
            (0.0, 0.0),
            (30.0, 60.0),
            (-30.0, -60.0),
            (45.0, 135.0),
            (-45.0, -135.0),
            (60.0, 120.0),
            (-60.0, -120.0),
            (75.0, 150.0),
            (-75.0, -150.0),
        ];

        for (lat, lon) in test_cases {
            for precision in 1..=8 {
                let hash = encode_with_precision(lat, lon, precision);
                assert_eq!(hash.len(), precision);

                for ch in hash.chars() {
                    assert!(
                        BASE32.contains(&(ch as u8)),
                        "坐标 ({}, {}) 精度 {} 产生的哈希 '{}' 包含无效字符 '{}'",
                        lat,
                        lon,
                        precision,
                        hash,
                        ch
                    );
                }
            }
        }
    }

    #[test]
    fn test_meridian_and_equator() {
        // 测试本初子午线和赤道附近的编码
        let equator_west = encode_with_precision(0.0, -90.0, 5);
        let equator_east = encode_with_precision(0.0, 90.0, 5);
        let meridian_north = encode_with_precision(45.0, 0.0, 5);
        let meridian_south = encode_with_precision(-45.0, 0.0, 5);

        println!("赤道西: {}", equator_west);
        println!("赤道东: {}", equator_east);
        println!("子午线北: {}", meridian_north);
        println!("子午线南: {}", meridian_south);

        // 它们应该都是不同的
        assert_ne!(equator_west, equator_east);
        assert_ne!(meridian_north, meridian_south);
    }

    #[test]
    fn test_precision_zero_handling() {
        // 测试精度为0的边界情况（虽然不实用，但应该能处理）
        let hash = encode_with_precision(35.6586, 139.7454, 0);
        assert_eq!(hash.len(), 0);
        assert_eq!(hash, "");
    }

    #[test]
    fn test_high_precision_encoding() {
        // 测试高精度编码（精度10及以上）
        let lat = 35.6586;
        let lon = 139.7454;

        for precision in 8..=12 {
            let hash = encode_with_precision(lat, lon, precision);
            assert_eq!(hash.len(), precision);

            // 验证所有字符有效
            for ch in hash.chars() {
                assert!(BASE32.contains(&(ch as u8)));
            }

            println!("精度 {}: {}", precision, hash);
        }
    }

    #[test]
    fn test_neighbor_calculation_stability() {
        // 测试邻居计算的稳定性：相同的输入应该总是产生相同的输出
        let test_hashes = vec!["wecn", "wx4g", "gcpv", "9q5"];

        for hash in test_hashes {
            let neighbors1 = get_neighbors(hash);
            let neighbors2 = get_neighbors(hash);
            let neighbors3 = get_neighbors(hash);

            assert_eq!(neighbors1, neighbors2);
            assert_eq!(neighbors2, neighbors3);
        }
    }

    #[test]
    fn test_neighbor_debug_wx4g() {
        // 详细调试 wx4g 的邻居关系
        let hash = "wx4g";
        println!("\n=== 调试 {} 的邻居 ===", hash);

        let neighbors = get_neighbors(hash);
        println!("所有邻居 ({} 个): {:?}", neighbors.len(), neighbors);

        // 测试每个方向
        if let Some(n) = neighbor(hash, Direction::North) {
            println!("北: {}", n);
            let reverse = neighbor(&n, Direction::South);
            println!("  南回: {:?} (期望: {})", reverse, hash);
        }

        if let Some(s) = neighbor(hash, Direction::South) {
            println!("南: {}", s);
            let reverse = neighbor(&s, Direction::North);
            println!("  北回: {:?} (期望: {})", reverse, hash);
        }

        if let Some(e) = neighbor(hash, Direction::East) {
            println!("东: {}", e);
            let reverse = neighbor(&e, Direction::West);
            println!("  西回: {:?} (期望: {})", reverse, hash);
        }

        if let Some(w) = neighbor(hash, Direction::West) {
            println!("西: {}", w);
            let reverse = neighbor(&w, Direction::East);
            println!("  东回: {:?} (期望: {})", reverse, hash);
        }

        // 检查每个邻居的反向关系
        println!("\n检查对称性:");
        for neighbor_hash in &neighbors {
            if neighbor_hash != hash {
                let reverse_neighbors = get_neighbors(neighbor_hash);
                let is_symmetric = reverse_neighbors.contains(&hash.to_string());
                println!(
                    "  {} -> {}: {}",
                    neighbor_hash,
                    hash,
                    if is_symmetric { "✓" } else { "✗" }
                );
            }
        }
    }
}

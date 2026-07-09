//! GeoHash 编码和邻居计算

const BASE32: &[u8] = b"0123456789bcdefghjkmnpqrstuvwxyz";
const PRECISION: usize = 4;

pub fn encode(lat: f64, lon: f64) -> String {
    encode_with_precision(lat, lon, PRECISION)
}

pub fn encode_with_precision(lat: f64, lon: f64, precision: usize) -> String {
    let mut lat_range = (-90.0, 90.0);
    let mut lon_range = (-180.0, 180.0);
    let mut hash = String::new();
    let mut bits = 0u8;
    let mut bit_count = 0;

    while hash.len() < precision {
        if bit_count % 2 == 0 {
            let mid = (lon_range.0 + lon_range.1) / 2.0;
            if lon >= mid {
                bits |= 1 << (4 - (bit_count % 5));
                lon_range.0 = mid;
            } else {
                lon_range.1 = mid;
            }
        } else {
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

/// 返回中心格子、四向邻居和对角邻居
pub fn get_neighbors(geohash: &str) -> Vec<String> {
    let mut neighbors = Vec::with_capacity(9);
    neighbors.push(geohash.to_string());

    let north = neighbor(geohash, Direction::North);
    let south = neighbor(geohash, Direction::South);
    let east = neighbor(geohash, Direction::East);
    let west = neighbor(geohash, Direction::West);

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
    let type_idx = geohash.len() % 2;

    let mut base = parent.to_string();

    // 边界字符需要先向父级借位，再映射当前字符
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
        let hash = encode(35.6586, 139.7454);
        assert_eq!(hash.len(), 4);
    }

    #[test]
    fn test_encode_known_locations() {
        let beijing = encode_with_precision(39.9042, 116.4074, 4);
        assert_eq!(beijing, "wx4g");

        let shanghai = encode_with_precision(31.2397, 121.4999, 4);
        assert_eq!(shanghai, "wtw3");

        let london = encode_with_precision(51.5074, -0.1278, 4);
        assert_eq!(london, "gcpv");
    }

    #[test]
    fn test_encode_different_precisions() {
        let lat = 35.6586;
        let lon = 139.7454;

        let hash1 = encode_with_precision(lat, lon, 1);
        let hash2 = encode_with_precision(lat, lon, 2);
        let hash3 = encode_with_precision(lat, lon, 3);
        let hash5 = encode_with_precision(lat, lon, 5);

        assert_eq!(hash1.len(), 1);
        assert_eq!(hash2.len(), 2);
        assert_eq!(hash3.len(), 3);
        assert_eq!(hash5.len(), 5);

        assert!(hash5.starts_with(&hash1));
        assert!(hash5.starts_with(&hash2));
        assert!(hash5.starts_with(&hash3));
    }

    #[test]
    fn test_encode_boundary_cases() {
        let origin = encode_with_precision(0.0, 0.0, 4);
        assert_eq!(origin.len(), 4);

        let north_pole = encode_with_precision(89.9, 0.0, 4);
        assert_eq!(north_pole.len(), 4);

        let south_pole = encode_with_precision(-89.9, 0.0, 4);
        assert_eq!(south_pole.len(), 4);

        let date_line_east = encode_with_precision(0.0, 179.9, 4);
        assert_eq!(date_line_east.len(), 4);

        let date_line_west = encode_with_precision(0.0, -179.9, 4);
        assert_eq!(date_line_west.len(), 4);
    }

    #[test]
    fn test_encode_consistency() {
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

        assert_eq!(neighbors.len(), 9);

        assert!(neighbors.contains(&hash.to_string()));

        let mut sorted = neighbors.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), neighbors.len(), "邻居列表不应该有重复");
    }

    #[test]
    fn test_neighbor_directions() {
        let hash = "wecn";

        let north = neighbor(hash, Direction::North);
        let south = neighbor(hash, Direction::South);
        let east = neighbor(hash, Direction::East);
        let west = neighbor(hash, Direction::West);

        assert!(north.as_deref().is_some_and(|value| value != hash));
        assert!(south.as_deref().is_some_and(|value| value != hash));
        assert!(east.as_deref().is_some_and(|value| value != hash));
        assert!(west.as_deref().is_some_and(|value| value != hash));
    }

    #[test]
    fn test_neighbor_reciprocity() {
        let hash = "wecn";

        if let Some(north) = neighbor(hash, Direction::North)
            && let Some(south_of_north) = neighbor(&north, Direction::South)
        {
            assert_eq!(south_of_north, hash, "北邻居的南邻居应该是原点");
        }

        if let Some(east) = neighbor(hash, Direction::East)
            && let Some(west_of_east) = neighbor(&east, Direction::West)
        {
            assert_eq!(west_of_east, hash, "东邻居的西邻居应该是原点");
        }
    }

    #[test]
    fn test_neighbors_at_boundaries() {
        let boundary_hashes = vec!["0", "00", "000", "s000", "pbpbp"];

        for hash in boundary_hashes {
            let neighbors = get_neighbors(hash);
            assert!(
                !neighbors.is_empty(),
                "GeoHash {} 应该至少有1个元素（自己）",
                hash
            );
            assert!(neighbors.len() <= 9, "GeoHash {} 不应该超过9个邻居", hash);
        }
    }

    #[test]
    fn test_neighbors_uniqueness() {
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
        let base_lat = 35.6586;
        let base_lon = 139.7454;

        let hash1 = encode(base_lat, base_lon);

        let hash2 = encode(base_lat + 0.0001, base_lon);
        let hash3 = encode(base_lat, base_lon + 0.0001);

        let neighbors1 = get_neighbors(&hash1);
        assert!(neighbors1.contains(&hash1));
        assert!(neighbors1.contains(&hash2) || hash1 == hash2);
        assert!(neighbors1.contains(&hash3) || hash1 == hash3);
    }

    #[test]
    fn test_all_base32_chars() {
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
        let result = neighbor("", Direction::North);
        assert!(result.is_none(), "空 GeoHash 应该返回 None");
    }

    #[test]
    fn test_precision_increases_accuracy() {
        let lat = 35.6586;
        let lon = 139.7454;

        let hash1_p3 = encode_with_precision(lat, lon, 3);
        let hash2_p3 = encode_with_precision(lat + 1.0, lon, 3);

        assert_ne!(hash1_p3, hash2_p3, "精度3应该能区分1度差异");
    }

    #[test]
    fn test_more_known_locations() {
        let ny = encode_with_precision(40.6892, -74.0445, 9);
        assert_eq!(ny, "dr5r7p4ry");

        let paris = encode_with_precision(48.8584, 2.2945, 5);
        assert_eq!(paris, "u09tu");

        let sydney = encode_with_precision(-33.8568, 151.2153, 5);
        assert_eq!(sydney, "r3gx2");

        let tokyo = encode_with_precision(35.6762, 139.6503, 9);
        assert_eq!(tokyo, "xn76cydhz");
    }

    #[test]
    fn test_negative_coordinates() {
        let south_america = encode_with_precision(-23.5505, -46.6333, 4);
        assert_eq!(south_america.len(), 4);
        assert!(
            south_america
                .chars()
                .next()
                .is_some_and(|first| BASE32.contains(&(first as u8)))
        );

        let antarctica = encode_with_precision(-75.0, -120.0, 4);
        assert_eq!(antarctica.len(), 4);

        let ne = encode_with_precision(45.0, 90.0, 3);
        let nw = encode_with_precision(45.0, -90.0, 3);
        let se = encode_with_precision(-45.0, 90.0, 3);
        let sw = encode_with_precision(-45.0, -90.0, 3);

        assert_ne!(ne, nw);
        assert_ne!(ne, se);
        assert_ne!(ne, sw);
        assert_ne!(nw, se);
        assert_ne!(nw, sw);
        assert_ne!(se, sw);
    }

    #[test]
    fn test_extreme_coordinates() {
        let extreme_cases = vec![
            (89.9999, 179.9999),
            (-89.9999, -179.9999),
            (0.0001, 0.0001),
            (-0.0001, -0.0001),
        ];

        for (lat, lon) in extreme_cases {
            let hash = encode_with_precision(lat, lon, 6);
            assert_eq!(hash.len(), 6);
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
        }
    }

    #[test]
    fn test_geohash_prefix_hierarchy() {
        let lat = 39.9042;
        let lon = 116.4074;

        let h1 = encode_with_precision(lat, lon, 1);
        let h2 = encode_with_precision(lat, lon, 2);
        let h3 = encode_with_precision(lat, lon, 3);
        let h4 = encode_with_precision(lat, lon, 4);
        let h5 = encode_with_precision(lat, lon, 5);
        let h6 = encode_with_precision(lat, lon, 6);

        assert!(h2.starts_with(&h1));
        assert!(h3.starts_with(&h2));
        assert!(h4.starts_with(&h3));
        assert!(h5.starts_with(&h4));
        assert!(h6.starts_with(&h5));
    }

    #[test]
    fn test_same_precision_nearby_points_share_prefix() {
        let base_lat = 35.6586;
        let base_lon = 139.7454;

        let hash1 = encode_with_precision(base_lat, base_lon, 6);
        let hash2 = encode_with_precision(base_lat + 0.01, base_lon, 6);
        let hash3 = encode_with_precision(base_lat, base_lon + 0.01, 6);

        assert_eq!(&hash1[..3], &hash2[..3]);
        assert_eq!(&hash1[..3], &hash3[..3]);
    }

    #[test]
    fn test_distant_points_different_hashes() {
        let beijing = encode_with_precision(39.9042, 116.4074, 5);
        let newyork = encode_with_precision(40.7128, -74.0060, 5);
        let sydney = encode_with_precision(-33.8688, 151.2093, 5);

        assert_ne!(beijing, newyork);
        assert_ne!(beijing, sydney);
        assert_ne!(newyork, sydney);

        assert_ne!(beijing.chars().next(), newyork.chars().next());
    }

    #[test]
    fn test_neighbors_symmetry() {
        let hash = "wx4g";
        let neighbors = get_neighbors(hash);

        let mut symmetric_count = 0;

        for neighbor_hash in &neighbors {
            if neighbor_hash != hash {
                let reverse_neighbors = get_neighbors(neighbor_hash);
                if reverse_neighbors.contains(&hash.to_string()) {
                    symmetric_count += 1;
                }
            }
        }

        assert!(
            symmetric_count >= (neighbors.len() - 1) / 2,
            "至少一半的邻居关系应该是对称的"
        );
    }

    #[test]
    fn test_neighbors_different_precisions() {
        let precisions = vec![("w", 1), ("wx", 2), ("wx4", 3), ("wx4g", 4), ("wx4g0", 5)];

        for (hash, precision) in precisions {
            let neighbors = get_neighbors(hash);
            assert!(
                !neighbors.is_empty() && neighbors.len() <= 9,
                "精度 {} 的 GeoHash '{}' 应该有1-9个邻居",
                precision,
                hash
            );
        }
    }

    #[test]
    fn test_encoding_is_deterministic() {
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
        let hash = "wx4g";
        let neighbors = get_neighbors(hash);

        assert_eq!(neighbors.len(), 9);

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
        let equator_west = encode_with_precision(0.0, -90.0, 5);
        let equator_east = encode_with_precision(0.0, 90.0, 5);
        let meridian_north = encode_with_precision(45.0, 0.0, 5);
        let meridian_south = encode_with_precision(-45.0, 0.0, 5);

        assert_ne!(equator_west, equator_east);
        assert_ne!(meridian_north, meridian_south);
    }

    #[test]
    fn test_precision_zero_handling() {
        let hash = encode_with_precision(35.6586, 139.7454, 0);
        assert_eq!(hash.len(), 0);
        assert_eq!(hash, "");
    }

    #[test]
    fn test_high_precision_encoding() {
        let lat = 35.6586;
        let lon = 139.7454;

        for precision in 8..=12 {
            let hash = encode_with_precision(lat, lon, precision);
            assert_eq!(hash.len(), precision);
            for ch in hash.chars() {
                assert!(BASE32.contains(&(ch as u8)));
            }
        }
    }

    #[test]
    fn test_neighbor_calculation_stability() {
        let test_hashes = vec!["wecn", "wx4g", "gcpv", "9q5"];

        for hash in test_hashes {
            let neighbors1 = get_neighbors(hash);
            let neighbors2 = get_neighbors(hash);
            let neighbors3 = get_neighbors(hash);

            assert_eq!(neighbors1, neighbors2);
            assert_eq!(neighbors2, neighbors3);
        }
    }
}

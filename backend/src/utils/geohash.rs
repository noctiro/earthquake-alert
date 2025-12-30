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
            // 经度
            let mid = (lon_range.0 + lon_range.1) / 2.0;
            if lon >= mid {
                bits |= 1 << (4 - (bit_count % 10) / 2);
                lon_range.0 = mid;
            } else {
                lon_range.1 = mid;
            }
        } else {
            // 纬度
            let mid = (lat_range.0 + lat_range.1) / 2.0;
            if lat >= mid {
                bits |= 1 << (4 - (bit_count % 10) / 2);
                lat_range.0 = mid;
            } else {
                lat_range.1 = mid;
            }
        }

        bit_count += 1;
        if bit_count % 10 == 0 {
            hash.push(BASE32[bits as usize] as char);
            bits = 0;
        }
    }

    hash
}

/// 获取相邻的 9 个格子 (包括自己)
/// 优化版本：确保获取所有8个方向的邻居，即使在边界情况
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

    // 添加对角线方向的邻居（更稳定的计算方式）
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

    // 去重（以防边界情况产生重复）
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
        Direction::North => {
            [
                "p0r21436x8zb9dcf5h7kjnmqesgutwvy",
                "bc01fg45238967deuvhjyznpkmstqrwx",
            ]
        }
        Direction::South => {
            [
                "14365h7k9dcfesgujnmqp0r2twvyx8zb",
                "238967debc01fg45kmstqrwxuvhjyznp",
            ]
        }
        Direction::East => {
            [
                "bc01fg45238967deuvhjyznpkmstqrwx",
                "p0r21436x8zb9dcf5h7kjnmqesgutwvy",
            ]
        }
        Direction::West => {
            [
                "238967debc01fg45kmstqrwxuvhjyznp",
                "14365h7k9dcfesgujnmqp0r2twvyx8zb",
            ]
        }
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
    fn test_encode() {
        // 东京塔坐标
        let hash = encode(35.6586, 139.7454);
        assert_eq!(hash.len(), 4);
        println!("东京塔 GeoHash: {}", hash);
    }

    #[test]
    fn test_neighbors() {
        let hash = "wecn";
        let neighbors = get_neighbors(hash);
        assert_eq!(neighbors.len(), 9);
        println!("邻居: {:?}", neighbors);
    }
}

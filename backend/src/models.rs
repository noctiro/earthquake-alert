use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// 订阅信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub bark_id: String,
    pub latitude: f64,
    pub longitude: f64,
    pub min_intensity: u8, // 最小烈度阈值 (0-7)
    pub created_at: i64,
}

impl Subscription {
    pub fn new(bark_id: String, latitude: f64, longitude: f64, min_intensity: u8) -> Self {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        Self {
            bark_id,
            latitude,
            longitude,
            min_intensity,
            created_at,
        }
    }
}

/// 订阅请求
#[derive(Debug, Deserialize)]
pub struct SubscribeRequest {
    pub bark_id: String,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default = "default_min_intensity")]
    pub min_intensity: u8, // 最小烈度阈值，默认 3
}

fn default_min_intensity() -> u8 {
    3 // 默认震度 3 以上推送
}

/// API 响应
#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

impl<T> ApiResponse<T> {
    pub fn success(message: impl Into<String>, data: Option<T>) -> Self {
        Self {
            success: true,
            message: message.into(),
            data,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            data: None,
        }
    }
}

/// JMA（日本气象厅）地震预警数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JmaEew {
    #[serde(rename = "type")]
    pub alert_type: String,
    #[serde(rename = "EventID")]
    pub event_id: String,
    #[serde(rename = "AnnouncedTime")] // UTC+9
    pub announced_time: String,
    #[serde(rename = "OriginTime")] // UTC+9
    pub origin_time: String,
    #[serde(rename = "Hypocenter")]
    pub hypocenter: String,
    #[serde(rename = "Latitude")]
    pub latitude: f64,
    #[serde(rename = "Longitude")]
    pub longitude: f64,
    #[serde(rename = "Magunitude")] // 注意：API 拼写错误
    pub magnitude: f64,
    #[serde(rename = "Depth")]
    pub depth: f64,
    #[serde(rename = "MaxIntensity")]
    pub max_intensity: String,
}

/// 四川地震局预警数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SichuanEew {
    #[serde(rename = "type")]
    pub alert_type: String,
    #[serde(rename = "EventID")]
    pub event_id: String,
    #[serde(rename = "OriginTime")] // UTC+8
    pub origin_time: String,
    #[serde(rename = "HypoCenter")]
    pub hypocenter: String,
    #[serde(rename = "Latitude")]
    pub latitude: f64,
    #[serde(rename = "Longitude")]
    pub longitude: f64,
    #[serde(rename = "Magunitude")] // 注意：API 拼写错误
    pub magnitude: f64,
    #[serde(rename = "Depth")]
    pub depth: f64,
    #[serde(rename = "MaxIntensity")]
    pub max_intensity: f64,
}

/// 中国地震台网中心预警数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CencEew {
    #[serde(rename = "type")]
    pub alert_type: String,
    #[serde(rename = "EventID")]
    pub event_id: String,
    #[serde(rename = "OriginTime")] // UTC+8
    pub origin_time: String,
    #[serde(rename = "HypoCenter")]
    pub hypocenter: String,
    #[serde(rename = "Latitude")]
    pub latitude: f64,
    #[serde(rename = "Longitude")]
    pub longitude: f64,
    #[serde(rename = "Magnitude")]
    pub magnitude: f64,
    #[serde(rename = "Depth")]
    pub depth: f64,
    #[serde(rename = "MaxIntensity")]
    pub max_intensity: f64,
}

/// 福建地震局预警数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FujianEew {
    #[serde(rename = "type")]
    pub alert_type: String,
    #[serde(rename = "EventID")]
    pub event_id: String,
    #[serde(rename = "OriginTime")] // UTC+8
    pub origin_time: String,
    #[serde(rename = "HypoCenter")]
    pub hypocenter: String,
    #[serde(rename = "Latitude")]
    pub latitude: f64,
    #[serde(rename = "Longitude")]
    pub longitude: f64,
    #[serde(rename = "Magunitude")] // 注意：API 拼写错误
    pub magnitude: f64,
    #[serde(rename = "isFinal")]
    pub is_final: bool,
}

/// 未知数据源的通用结构（fallback）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnknownEarthquakeData {
    #[serde(rename = "type")]
    pub alert_type: String,
    #[serde(flatten)]
    pub data: serde_json::Value,
}

/// 地震数据枚举（支持所有数据源）
#[derive(Debug, Clone)]
pub enum EarthquakeData {
    JmaEew(JmaEew),
    SichuanEew(SichuanEew),
    CencEew(CencEew),
    FujianEew(FujianEew),
    Unknown(UnknownEarthquakeData),
}

impl EarthquakeData {
    /// 从 JSON 字符串解析
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        // 先解析消息类型
        let msg: WebSocketMessage = serde_json::from_str(json)?;

        match msg.message_type.as_str() {
            "jma_eew" => {
                let data: JmaEew = serde_json::from_str(json)?;
                Ok(EarthquakeData::JmaEew(data))
            }
            "sc_eew" => {
                let data: SichuanEew = serde_json::from_str(json)?;
                Ok(EarthquakeData::SichuanEew(data))
            }
            "cenc_eew" => {
                let data: CencEew = serde_json::from_str(json)?;
                Ok(EarthquakeData::CencEew(data))
            }
            "fj_eew" => {
                let data: FujianEew = serde_json::from_str(json)?;
                Ok(EarthquakeData::FujianEew(data))
            }
            _ => {
                // 未知数据源
                tracing::warn!(
                    "发现未适配的数据源类型: {}，请考虑添加专门的 struct 支持",
                    msg.message_type
                );
                tracing::debug!("未适配数据内容: {}", json);

                let data: UnknownEarthquakeData = serde_json::from_str(json)?;
                Ok(EarthquakeData::Unknown(data))
            }
        }
    }

    /// 转换为通用信息
    pub fn to_common_info(&self) -> Option<CommonEarthquakeInfo> {
        match self {
            EarthquakeData::JmaEew(data) => Some(CommonEarthquakeInfo {
                latitude: data.latitude,
                longitude: data.longitude,
                magnitude: data.magnitude,
                depth: data.depth,
                max_intensity: data.max_intensity.clone(),
                region: data.hypocenter.clone(),
                origin_time: data.origin_time.clone(),
                source_type: "jma_eew".to_string(),
            }),
            EarthquakeData::SichuanEew(data) => Some(CommonEarthquakeInfo {
                latitude: data.latitude,
                longitude: data.longitude,
                magnitude: data.magnitude,
                depth: data.depth,
                max_intensity: data.max_intensity.to_string(),
                region: data.hypocenter.clone(),
                origin_time: data.origin_time.clone(),
                source_type: "sc_eew".to_string(),
            }),
            EarthquakeData::CencEew(data) => Some(CommonEarthquakeInfo {
                latitude: data.latitude,
                longitude: data.longitude,
                magnitude: data.magnitude,
                depth: data.depth,
                max_intensity: data.max_intensity.to_string(),
                region: data.hypocenter.clone(),
                origin_time: data.origin_time.clone(),
                source_type: "cenc_eew".to_string(),
            }),
            EarthquakeData::FujianEew(data) => Some(CommonEarthquakeInfo {
                latitude: data.latitude,
                longitude: data.longitude,
                magnitude: data.magnitude,
                depth: 0.0, // 福建数据源没有深度
                max_intensity: "未知".to_string(),
                region: data.hypocenter.clone(),
                origin_time: data.origin_time.clone(),
                source_type: "fj_eew".to_string(),
            }),
            EarthquakeData::Unknown(data) => {
                // 尝试从未知数据源提取通用信息
                // 如果关键字段存在且类型正确，仍然可以推送
                let latitude = data.data.get("Latitude").and_then(|v| v.as_f64())?;
                let longitude = data.data.get("Longitude").and_then(|v| v.as_f64())?;
                let magnitude = data
                    .data
                    .get("Magnitude")
                    .or_else(|| data.data.get("Magunitude")) // 兼容拼写错误
                    .and_then(|v| v.as_f64())?;

                let depth = data
                    .data
                    .get("Depth")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);

                let max_intensity = data
                    .data
                    .get("MaxIntensity")
                    .and_then(|v| {
                        // 尝试字符串或数字类型
                        v.as_str()
                            .map(|s| s.to_string())
                            .or_else(|| v.as_i64().map(|i| i.to_string()))
                    })
                    .unwrap_or_else(|| "未知".to_string());

                let region = data
                    .data
                    .get("HypoCenter")
                    .or_else(|| data.data.get("Hypocenter"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let origin_time = data
                    .data
                    .get("OriginTime")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                tracing::info!(
                    "未知数据源 [{}] 成功提取通用信息: M{:.1} @ ({:.2}, {:.2})",
                    data.alert_type,
                    magnitude,
                    latitude,
                    longitude
                );

                Some(CommonEarthquakeInfo {
                    latitude,
                    longitude,
                    magnitude,
                    depth,
                    max_intensity,
                    region,
                    origin_time,
                    source_type: data.alert_type.clone(),
                })
            }
        }
    }

    /// 从 JSON 字符串解析并转换为通用信息（便捷方法）
    pub fn parse_to_common_info(json: &str) -> Result<CommonEarthquakeInfo, serde_json::Error> {
        let earthquake_data = Self::from_json(json)?;
        earthquake_data.to_common_info().ok_or_else(|| {
            serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "无法从未知数据源提取通用信息",
            ))
        })
    }
}

/// 通用地震信息（用于推送）
#[derive(Debug, Clone)]
pub struct CommonEarthquakeInfo {
    pub latitude: f64,
    pub longitude: f64,
    pub magnitude: f64,
    pub depth: f64,
    pub max_intensity: String,
    pub region: String,
    pub origin_time: String,
    pub source_type: String, // 数据源类型
}

/// WebSocket 消息包装（用于区分不同类型的消息）
#[derive(Debug, Deserialize)]
pub struct WebSocketMessage {
    #[serde(rename = "type")]
    pub message_type: String,
}

/// GeoHash 索引数据 (存储在数据库中)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoHashIndex {
    pub bark_ids: Vec<String>,
}

impl GeoHashIndex {
    pub fn new() -> Self {
        Self {
            bark_ids: Vec::new(),
        }
    }

    pub fn add(&mut self, bark_id: String) {
        if !self.bark_ids.contains(&bark_id) {
            self.bark_ids.push(bark_id);
        }
    }

    pub fn remove(&mut self, bark_id: &str) {
        self.bark_ids.retain(|id| id != bark_id);
    }
}

impl Default for GeoHashIndex {
    fn default() -> Self {
        Self::new()
    }
}

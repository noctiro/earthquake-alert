use crate::db::Database;
use crate::models::{ApiResponse, SubscribeRequest, Subscription};
use crate::utils::{distance, intensity};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::Serialize;

/// 应用状态
#[derive(Clone)]
pub struct AppState {
    pub db: Database,
}

/// 订阅处理器
pub async fn subscribe_handler(
    State(state): State<AppState>,
    Json(payload): Json<SubscribeRequest>,
) -> impl IntoResponse {
    // 验证输入
    if payload.bark_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<SubscribeResponse>::error("Bark ID 不能为空")),
        );
    }

    // Bark ID 长度限制，防止过长数据
    if payload.bark_id.len() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<SubscribeResponse>::error(
                "Bark ID 过长（最大64字符）",
            )),
        );
    }

    // 验证 Bark ID 只包含安全字符（字母、数字）
    if !payload.bark_id.chars().all(|c| c.is_alphanumeric()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<SubscribeResponse>::error(
                "Bark ID 只能包含字母、数字",
            )),
        );
    }

    if !distance::validate_coordinates(payload.latitude, payload.longitude) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<SubscribeResponse>::error("无效的经纬度坐标")),
        );
    }

    if !intensity::validate_intensity(payload.min_intensity) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<SubscribeResponse>::error(
                "烈度阈值必须在 0-7 之间",
            )),
        );
    }

    // 创建订阅
    let subscription = Subscription::new(
        payload.bark_id.clone(),
        payload.latitude,
        payload.longitude,
        payload.min_intensity,
    );

    // 打印订阅信息
    tracing::info!(
        "收到订阅请求 - Bark ID: {}, 位置: ({:.4}, {:.4}), 最小震度: {}",
        subscription.bark_id,
        subscription.latitude,
        subscription.longitude,
        subscription.min_intensity
    );

    // 保存到数据库
    let store = state.db.subscriptions();
    match store.upsert_subscription(subscription.clone()) {
        Ok(_) => {
            tracing::info!(
                "订阅成功 - Bark ID: {}, GeoHash: {}",
                subscription.bark_id,
                crate::utils::geohash::encode(subscription.latitude, subscription.longitude)
            );
            (
                StatusCode::OK,
                Json(ApiResponse::success(
                    "订阅成功",
                    Some(SubscribeResponse::from(subscription)),
                )),
            )
        }
        Err(e) => {
            tracing::error!(
                "订阅失败 - Bark ID: {}, 错误: {:?}",
                subscription.bark_id,
                e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<SubscribeResponse>::error(format!(
                    "订阅失败: {}",
                    e
                ))),
            )
        }
    }
}

/// 取消订阅处理器（路径参数版本）
pub async fn unsubscribe_by_path_handler(
    State(state): State<AppState>,
    Path(bark_id): Path<String>,
) -> impl IntoResponse {
    // 验证输入
    if bark_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error("Bark ID 不能为空")),
        );
    }

    // Bark ID 长度限制，防止过长数据
    if bark_id.len() > 256 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error("Bark ID 过长（最大256字符）")),
        );
    }

    // 验证 Bark ID 只包含安全字符（字母、数字、下划线、连字符）
    if !bark_id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error(
                "Bark ID 只能包含字母、数字、下划线和连字符",
            )),
        );
    }

    tracing::info!("收到取消订阅请求（路径参数）- Bark ID: {}", bark_id);

    let store = state.db.subscriptions();
    match store.delete_subscription(&bark_id) {
        Ok(_) => {
            tracing::info!("取消订阅成功 - Bark ID: {}", bark_id);
            (
                StatusCode::OK,
                Json(ApiResponse::<()>::success("已取消订阅", None)),
            )
        }
        Err(e) => {
            tracing::error!("取消订阅失败 - Bark ID: {}, 错误: {:?}", bark_id, e);
            (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::error(format!("取消订阅失败: {}", e))),
            )
        }
    }
}

/// 订阅成功响应
#[derive(Serialize)]
pub struct SubscribeResponse {
    pub bark_id: String,
    pub latitude: f64,
    pub longitude: f64,
    pub min_intensity: u8,
    pub created_at: i64,
}

impl From<Subscription> for SubscribeResponse {
    fn from(sub: Subscription) -> Self {
        Self {
            bark_id: sub.bark_id,
            latitude: sub.latitude,
            longitude: sub.longitude,
            min_intensity: sub.min_intensity,
            created_at: sub.created_at,
        }
    }
}

#[derive(Serialize)]
pub struct StatsResponse {
    pub total_subscriptions: usize,
}

/// 获取统计信息
pub async fn stats_handler(State(state): State<AppState>) -> impl IntoResponse {
    let store = state.db.subscriptions();
    match store.get_total_count() {
        Ok(count) => (
            StatusCode::OK,
            Json(ApiResponse::success(
                "统计成功",
                Some(StatsResponse {
                    total_subscriptions: count,
                }),
            )),
        ),
        Err(e) => {
            tracing::error!("获取统计失败: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<StatsResponse>::error(format!(
                    "获取统计失败: {}",
                    e
                ))),
            )
        }
    }
}

/// 健康检查
pub async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(ApiResponse::<()>::success("OK", None)))
}

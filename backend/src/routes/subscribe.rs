use crate::db::Database;
use crate::models::{
    ApiResponse, NotificationBand, SubscribeRequest, Subscription, SubscriptionLocation,
    UnsubscribeRequest, mask_bark_id, validate_bark_level,
};
use crate::services::BarkNotifier;
use crate::utils::distance;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub bark_notifier: BarkNotifier,
}

pub async fn subscribe_handler(
    State(state): State<AppState>,
    Json(payload): Json<SubscribeRequest>,
) -> impl IntoResponse {
    if payload.bark_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<SubscribeResponse>::error("Bark ID 不能为空")),
        );
    }

    if payload.bark_id.len() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<SubscribeResponse>::error(
                "Bark ID 过长（最大64字符）",
            )),
        );
    }

    if !payload.bark_id.chars().all(|c| c.is_alphanumeric()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<SubscribeResponse>::error(
                "Bark ID 只能包含字母、数字",
            )),
        );
    }

    let locations = normalize_locations(&payload);
    if locations.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<SubscribeResponse>::error(
                "请至少添加一个有效监测地点",
            )),
        );
    }
    let primary = locations[0].clone();

    let notify_bands = match normalize_notify_bands(&payload) {
        Ok(bands) => bands,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<SubscribeResponse>::error(message)),
            );
        }
    };
    let mut subscription =
        Subscription::new(payload.bark_id.clone(), primary.latitude, primary.longitude);
    subscription.bark_server = payload.bark_server.trim().trim_end_matches('/').to_string();
    subscription.location_name = primary.name;
    subscription.locations = locations;
    subscription.notify_bands = notify_bands;

    tracing::info!(
        event = "subscription.requested",
        bark_id = %mask_bark_id(&subscription.bark_id),
        location_count = subscription.locations.len(),
        band_count = subscription.notify_bands.len(),
        "subscription.requested"
    );

    let store = state.db.subscriptions();
    match store.upsert_subscription(subscription.clone()) {
        Ok(_) => {
            if let Err(error) = state
                .bark_notifier
                .send_subscription_confirm(&subscription)
                .await
            {
                tracing::error!(
                    event = "subscription.confirm_failed",
                    bark_id = %mask_bark_id(&subscription.bark_id),
                    error = ?error,
                    "subscription.confirm_failed"
                );
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(ApiResponse::<SubscribeResponse>::error(format!(
                        "订阅已保存，但成功提醒发送失败: {}",
                        error
                    ))),
                );
            }
            tracing::info!(
                event = "subscription.request_completed",
                bark_id = %mask_bark_id(&subscription.bark_id),
                "subscription.request_completed"
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
                event = "subscription.request_failed",
                bark_id = %mask_bark_id(&subscription.bark_id),
                error = ?e,
                "subscription.request_failed"
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

pub async fn unsubscribe_handler(
    State(state): State<AppState>,
    Json(payload): Json<UnsubscribeRequest>,
) -> impl IntoResponse {
    let bark_id = payload.bark_id.trim().to_string();
    if bark_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error("Bark ID 不能为空")),
        );
    }

    if bark_id.len() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error("Bark ID 过长（最大64字符）")),
        );
    }

    if !bark_id.chars().all(|c| c.is_alphanumeric()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error("Bark ID 只能包含字母、数字")),
        );
    }

    tracing::info!(
        event = "subscription.delete_requested",
        bark_id = %mask_bark_id(&bark_id),
        "subscription.delete_requested"
    );

    let store = state.db.subscriptions();
    match store.delete_subscription(&bark_id) {
        Ok(_) => {
            tracing::info!(
                event = "subscription.delete_completed",
                bark_id = %mask_bark_id(&bark_id),
                "subscription.delete_completed"
            );
            (
                StatusCode::OK,
                Json(ApiResponse::<()>::success("已取消订阅", None)),
            )
        }
        Err(e) => {
            tracing::error!(
                event = "subscription.delete_failed",
                bark_id = %mask_bark_id(&bark_id),
                error = ?e,
                "subscription.delete_failed"
            );
            (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::error(format!("取消订阅失败: {}", e))),
            )
        }
    }
}

#[derive(Serialize)]
pub struct SubscribeResponse {
    pub saved: bool,
}

impl From<Subscription> for SubscribeResponse {
    fn from(_sub: Subscription) -> Self {
        Self { saved: true }
    }
}

fn normalize_locations(payload: &SubscribeRequest) -> Vec<SubscriptionLocation> {
    let mut locations = if payload.locations.is_empty() {
        vec![SubscriptionLocation {
            name: payload.location_name.trim().to_string(),
            latitude: payload.latitude,
            longitude: payload.longitude,
        }]
    } else {
        payload.locations.clone()
    };
    locations.retain(|item| distance::validate_coordinates(item.latitude, item.longitude));
    for location in &mut locations {
        location.name = location.name.trim().chars().take(80).collect();
    }
    locations.truncate(3);
    locations
}

fn normalize_notify_bands(payload: &SubscribeRequest) -> Result<Vec<NotificationBand>, String> {
    if payload.notify_bands.is_empty() {
        return Err("请至少添加一条通知级别规则".to_string());
    }
    if payload.notify_bands.len() > 3 {
        return Err("通知级别规则最多 3 条".to_string());
    }
    let mut bands = payload.notify_bands.clone();
    bands.sort_by_key(|band| band.min);
    let mut levels = std::collections::HashSet::new();
    let mut used = std::collections::HashSet::new();
    for band in &mut bands {
        band.level = band.level.trim().to_ascii_lowercase();
        if !validate_bark_level(&band.level) {
            return Err("通知级别必须是 passive、active 或 critical".to_string());
        }
        if !levels.insert(band.level.clone()) {
            return Err("每个通知级别只能添加一条规则".to_string());
        }
        if band.min > band.max || band.min > 99 || band.max > 99 {
            return Err("通知级别烈度范围无效".to_string());
        }
        if band.level == "critical" && band.max < 7 {
            band.max = 99;
        }
        band.label = band.label.trim().chars().take(32).collect();
        for value in band.min..=band.max {
            if !used.insert(value) {
                return Err("通知级别烈度范围不能重叠".to_string());
            }
        }
    }
    Ok(bands)
}

#[derive(Serialize)]
pub struct StatsResponse {
    pub total_subscriptions: usize,
}

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
            tracing::error!(event = "stats.load_failed", error = ?e, "stats.load_failed");
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

pub async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(ApiResponse::<()>::success("OK", None)))
}

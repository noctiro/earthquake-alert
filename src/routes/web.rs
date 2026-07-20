use crate::delivery::NotificationVerifyError;
use crate::models::IncidentId;
use crate::routes::AppState;
use crate::routes::detail_page::{
    detail_error, detail_not_found, detail_response, detail_unavailable, render_incident_page,
};
use axum::{
    extract::{Path, State},
    http::header::{CACHE_CONTROL, CONTENT_TYPE},
    response::{Html, IntoResponse, Response},
};
use std::sync::OnceLock;

const INDEX_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/index.min.html"));
const FRONTEND_CONFIG_JS: &str =
    "window.DISASTER_API_BASE = window.DISASTER_API_BASE || window.location.origin;\n";
const INSTANCE_NOTICE_MARKER: &str = "__DISASTER_ALERT_INSTANCE_NOTICE__";
const INSTANCE_TERMS_NOTICE: &str = r#"
<dialog id="instance-terms-dialog" class="instance-terms-dialog" aria-labelledby="instance-terms-title" aria-describedby="instance-terms-summary">
  <div class="instance-terms-heading">
    <span class="instance-terms-icon" aria-hidden="true">!</span>
    <div>
      <span class="instance-terms-eyebrow">实例配置提醒</span>
      <h2 id="instance-terms-title">当前实例尚未确认部署责任声明</h2>
    </div>
  </div>
  <p id="instance-terms-summary">此实例未设置 <code>INSTANCE_TERMS_ACCEPTED=true</code>。服务仍在运行，但部署者尚未通过本项目的显式确认流程。</p>
  <ul>
    <li>项目维护者仅提供可自部署的软件，不运营、控制或认可本实例提供的实时灾害信息、订阅或通知服务。</li>
    <li>启用实时数据或向他人提供服务前，部署者应自行核查适用法律法规，并取得所需许可、数据授权和个人信息处理依据；自部署不等于获准公开发布预警。</li>
    <li>信息可能延迟、缺失或误报，不属于官方预警，也不应作为唯一的安全决策依据。</li>
  </ul>
  <p class="instance-terms-note">本提示只反映环境变量状态，不能替代法律评估、主管部门许可或数据提供方授权。</p>
  <div class="instance-terms-actions">
    <a href="https://github.com/noctiro/disaster-alert#使用与部署责任" target="_blank" rel="noopener noreferrer">查看完整声明</a>
    <button id="dismiss-instance-terms" class="primary" type="button">继续查看</button>
  </div>
</dialog>
"#;
static ACCEPTED_INDEX_HTML: OnceLock<String> = OnceLock::new();
static UNACCEPTED_INDEX_HTML: OnceLock<String> = OnceLock::new();

enum DetailLoadError {
    InvalidLink(anyhow::Error),
    Storage(anyhow::Error),
}

pub(crate) async fn index_handler(State(state): State<AppState>) -> impl IntoResponse {
    Html(render_index_html(state.instance_terms_accepted))
}

pub(crate) async fn frontend_config_handler() -> impl IntoResponse {
    (
        [
            (CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (CACHE_CONTROL, "no-store"),
        ],
        FRONTEND_CONFIG_JS,
    )
}

fn render_index_html(instance_terms_accepted: bool) -> &'static str {
    let (rendered, notice) = if instance_terms_accepted {
        (&ACCEPTED_INDEX_HTML, "")
    } else {
        (&UNACCEPTED_INDEX_HTML, INSTANCE_TERMS_NOTICE)
    };
    rendered
        .get_or_init(|| INDEX_HTML.replace(INSTANCE_NOTICE_MARKER, notice))
        .as_str()
}

pub(crate) async fn incident_detail_handler(
    State(state): State<AppState>,
    Path((incident_id, token)): Path<(String, String)>,
) -> Response {
    let Some(incident_id) = IncidentId::parse(&incident_id) else {
        return detail_not_found();
    };
    let Ok(permit) = state.detail_concurrency.clone().try_acquire_owned() else {
        tracing::warn!(
            event = "incident.detail_overloaded",
            incident_id = %incident_id.as_str(),
            "incident.detail_overloaded"
        );
        return detail_unavailable();
    };
    let Ok(database_permit) = state.storage_concurrency.clone().try_acquire_owned() else {
        tracing::warn!(
            event = "incident.detail_storage_overloaded",
            incident_id = %incident_id.as_str(),
            "incident.detail_storage_overloaded"
        );
        return detail_unavailable();
    };
    let links = state.notification_links.clone();
    let verify_incident_id = incident_id.clone();
    let storage = state.storage.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _database_permit = database_permit;
        let snapshot = links
            .verify(&verify_incident_id, &token)
            .map_err(|error| match error {
                NotificationVerifyError::Invalid(error) => DetailLoadError::InvalidLink(error),
                NotificationVerifyError::Storage(error) => DetailLoadError::Storage(error),
            })?;
        let incident = storage
            .incident(&verify_incident_id)
            .map_err(DetailLoadError::Storage)?;
        Ok::<_, DetailLoadError>((snapshot, incident))
    })
    .await;
    let (snapshot, incident) = match loaded {
        Ok(Ok(loaded)) => loaded,
        Ok(Err(DetailLoadError::InvalidLink(error))) => {
            tracing::warn!(
                event = "incident.invalid_notification_link",
                incident_id = %incident_id.as_str(),
                error = %error,
                "incident.invalid_notification_link"
            );
            return detail_not_found();
        }
        Ok(Err(DetailLoadError::Storage(error))) => {
            tracing::error!(
                event = "incident.read_failed",
                incident_id = %incident_id.as_str(),
                error = ?error,
                "incident.read_failed"
            );
            return detail_error();
        }
        Err(error) => {
            tracing::error!(
                event = "incident.notification_verify_task_failed",
                incident_id = %incident_id.as_str(),
                error = ?error,
                "incident.notification_verify_task_failed"
            );
            return detail_error();
        }
    };
    detail_response(render_incident_page(&snapshot, incident.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::{INSTANCE_NOTICE_MARKER, render_index_html};

    #[test]
    fn accepted_instance_has_no_terms_dialog() {
        let html = render_index_html(true);
        assert!(!html.contains(INSTANCE_NOTICE_MARKER));
        assert!(!html.contains("id=\"instance-terms-dialog\""));
    }

    #[test]
    fn unaccepted_instance_has_terms_dialog() {
        let html = render_index_html(false);
        assert!(!html.contains(INSTANCE_NOTICE_MARKER));
        assert!(html.contains("id=\"instance-terms-dialog\""));
        assert!(html.contains("INSTANCE_TERMS_ACCEPTED=true"));
    }
}

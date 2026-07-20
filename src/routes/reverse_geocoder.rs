use crate::config::Config;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use url::Url;

const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(1);
const MAX_CACHE_ENTRIES: usize = 1_024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_SEARCH_QUERY_CHARS: usize = 160;
const MAX_SEARCH_RESULTS: usize = 5;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReverseGeocodeResult {
    pub(crate) province: String,
    pub(crate) city: String,
    pub(crate) district: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GeocodeSearchResult {
    pub(crate) name: String,
    pub(crate) address: String,
    pub(crate) latitude: f64,
    pub(crate) longitude: f64,
    pub(crate) province: String,
    pub(crate) city: String,
    pub(crate) district: String,
}

#[derive(Clone)]
pub(crate) struct ReverseGeocoder {
    enabled: Option<Arc<EnabledGeocoder>>,
}

struct EnabledGeocoder {
    endpoint: Url,
    client: reqwest::Client,
    state: Arc<Mutex<GeocoderState>>,
    coordinate_locks: Arc<StdMutex<HashMap<CoordinateKey, Weak<Mutex<()>>>>>,
}

#[derive(Default)]
struct GeocoderState {
    cache: HashMap<CoordinateKey, CacheEntry>,
    cache_order: VecDeque<CoordinateKey>,
    last_request: Option<Instant>,
}

struct CacheEntry {
    value: ReverseGeocodeResult,
    stored_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CoordinateKey {
    latitude: i32,
    longitude: i32,
}

#[derive(Deserialize)]
struct NominatimResponse {
    #[serde(default)]
    address: NominatimAddress,
    error: Option<String>,
}

#[derive(Deserialize)]
struct NominatimSearchItem {
    display_name: Option<String>,
    name: Option<String>,
    lat: String,
    lon: String,
    #[serde(default)]
    address: NominatimAddress,
}

#[derive(Clone, Default, Deserialize)]
struct NominatimAddress {
    state: Option<String>,
    province: Option<String>,
    region: Option<String>,
    city: Option<String>,
    town: Option<String>,
    municipality: Option<String>,
    county: Option<String>,
    city_district: Option<String>,
    district: Option<String>,
    borough: Option<String>,
    suburb: Option<String>,
}

impl ReverseGeocoder {
    pub(crate) fn new(config: &Config) -> Result<Self> {
        Self::from_settings(
            config.reverse_geocoding_enabled,
            &config.reverse_geocoding_url,
        )
    }

    fn from_settings(enabled: bool, reverse_geocoding_url: &str) -> Result<Self> {
        if !enabled {
            return Ok(Self { enabled: None });
        }
        let endpoint =
            Url::parse(reverse_geocoding_url).context("failed to parse reverse geocoding URL")?;
        let client = reqwest::Client::builder()
            .user_agent("disaster-alert/1.0 (https://github.com/noctiro/disaster-alert)")
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .pool_max_idle_per_host(2)
            .build()?;
        Ok(Self {
            enabled: Some(Arc::new(EnabledGeocoder {
                endpoint,
                client,
                state: Arc::new(Mutex::new(GeocoderState::default())),
                coordinate_locks: Arc::new(StdMutex::new(HashMap::new())),
            })),
        })
    }

    pub(crate) async fn resolve(
        &self,
        latitude: f64,
        longitude: f64,
    ) -> Result<ReverseGeocodeResult> {
        let enabled = self
            .enabled
            .as_ref()
            .context("reverse geocoding is disabled")?;
        let key = CoordinateKey::new(latitude, longitude)?;
        let coordinate_lock = enabled.coordinate_lock(key);
        let _coordinate_guard = coordinate_lock.lock().await;
        if let Some(cached) = enabled.cached(key).await {
            return Ok(cached);
        }
        enabled.acquire_request_slot().await;

        let mut url = enabled.endpoint.clone();
        url.query_pairs_mut()
            .append_pair("format", "jsonv2")
            .append_pair("addressdetails", "1")
            .append_pair("accept-language", "zh-CN,zh,en")
            .append_pair("zoom", "14")
            .append_pair("lat", &latitude.to_string())
            .append_pair("lon", &longitude.to_string());
        let response = enabled
            .client
            .get(url)
            .send()
            .await
            .context("reverse geocoding request failed")?
            .error_for_status()
            .context("reverse geocoding service returned an error")?;
        let response = limited_response_json(response).await?;
        anyhow::ensure!(
            response.error.as_deref().is_none_or(str::is_empty),
            "reverse geocoding service rejected the coordinates"
        );
        let value = response.address.into_result();
        enabled.cache(key, value.clone()).await;
        Ok(value)
    }

    pub(crate) async fn search(&self, query: &str) -> Result<Vec<GeocodeSearchResult>> {
        let enabled = self
            .enabled
            .as_ref()
            .context("reverse geocoding is disabled")?;
        let query = query.trim();
        anyhow::ensure!(query.chars().count() >= 2, "search query is too short");
        anyhow::ensure!(
            query.chars().count() <= MAX_SEARCH_QUERY_CHARS,
            "search query is too long"
        );
        enabled.acquire_request_slot().await;

        let mut url = enabled.search_endpoint();
        url.query_pairs_mut()
            .append_pair("format", "jsonv2")
            .append_pair("addressdetails", "1")
            .append_pair("accept-language", "zh-CN,zh,en")
            .append_pair("limit", &MAX_SEARCH_RESULTS.to_string())
            .append_pair("q", query);
        let response = enabled
            .client
            .get(url)
            .send()
            .await
            .context("geocoding search request failed")?
            .error_for_status()
            .context("geocoding search service returned an error")?;
        let items = limited_search_response_json(response).await?;
        Ok(items
            .into_iter()
            .filter_map(NominatimSearchItem::into_result)
            .take(MAX_SEARCH_RESULTS)
            .collect())
    }
}

impl EnabledGeocoder {
    fn search_endpoint(&self) -> Url {
        let mut url = self.endpoint.clone();
        let path = url.path().trim_end_matches('/');
        if path.ends_with("/reverse") {
            let search_path = format!("{}/search", path.trim_end_matches("/reverse"));
            url.set_path(&search_path);
        } else {
            url.set_path("/search");
        }
        url.set_query(None);
        url
    }

    fn coordinate_lock(&self, key: CoordinateKey) -> Arc<Mutex<()>> {
        let mut locks = self
            .coordinate_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        locks.retain(|_, weak| weak.strong_count() > 0);
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }

    async fn cached(&self, key: CoordinateKey) -> Option<ReverseGeocodeResult> {
        let state = self.state.lock().await;
        state
            .cache
            .get(&key)
            .filter(|entry| entry.stored_at.elapsed() <= CACHE_TTL)
            .map(|entry| entry.value.clone())
    }

    async fn acquire_request_slot(&self) {
        loop {
            let delay = {
                let mut state = self.state.lock().await;
                match state.last_request {
                    Some(last_request) => {
                        let elapsed = last_request.elapsed();
                        if elapsed < MIN_REQUEST_INTERVAL {
                            Some(MIN_REQUEST_INTERVAL - elapsed)
                        } else {
                            state.last_request = Some(Instant::now());
                            None
                        }
                    }
                    None => {
                        state.last_request = Some(Instant::now());
                        None
                    }
                }
            };
            let Some(delay) = delay else {
                return;
            };
            tokio::time::sleep(delay).await;
        }
    }

    async fn cache(&self, key: CoordinateKey, value: ReverseGeocodeResult) {
        let mut state = self.state.lock().await;
        state.cache.insert(
            key,
            CacheEntry {
                value: value.clone(),
                stored_at: Instant::now(),
            },
        );
        state.cache_order.retain(|cached| *cached != key);
        state.cache_order.push_back(key);
        while state.cache_order.len() > MAX_CACHE_ENTRIES {
            if let Some(expired) = state.cache_order.pop_front() {
                state.cache.remove(&expired);
            }
        }
    }
}

async fn limited_response_json(mut response: reqwest::Response) -> Result<NominatimResponse> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        bail!("reverse geocoding response exceeded size limit");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read reverse geocoding response")?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            bail!("reverse geocoding response exceeded size limit");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).context("invalid reverse geocoding response")
}

async fn limited_search_response_json(
    mut response: reqwest::Response,
) -> Result<Vec<NominatimSearchItem>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        bail!("geocoding search response exceeded size limit");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read geocoding search response")?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            bail!("geocoding search response exceeded size limit");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).context("invalid geocoding search response")
}

impl CoordinateKey {
    fn new(latitude: f64, longitude: f64) -> Result<Self> {
        if !latitude.is_finite()
            || !longitude.is_finite()
            || !(-90.0..=90.0).contains(&latitude)
            || !(-180.0..=180.0).contains(&longitude)
        {
            bail!("invalid coordinates");
        }
        Ok(Self {
            latitude: (latitude * 10_000.0).round() as i32,
            longitude: (longitude * 10_000.0).round() as i32,
        })
    }
}

impl NominatimAddress {
    fn into_result(self) -> ReverseGeocodeResult {
        ReverseGeocodeResult {
            province: first_non_empty([self.state, self.province, self.region]),
            city: first_non_empty([self.city, self.town, self.municipality]),
            district: first_non_empty([
                self.city_district,
                self.district,
                self.borough,
                self.suburb,
                self.county,
            ]),
        }
    }
}

impl NominatimSearchItem {
    fn into_result(self) -> Option<GeocodeSearchResult> {
        let latitude = self.lat.parse::<f64>().ok()?;
        let longitude = self.lon.parse::<f64>().ok()?;
        if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
            return None;
        }
        let region = self.address.clone().into_result();
        let name = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| short_display_name(self.display_name.as_deref()))
            .or_else(|| {
                [
                    region.district.as_str(),
                    region.city.as_str(),
                    region.province.as_str(),
                ]
                .into_iter()
                .find(|value| !value.is_empty())
                .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| "搜索结果".to_string());
        Some(GeocodeSearchResult {
            name,
            address: self.display_name.unwrap_or_default(),
            latitude,
            longitude,
            province: region.province,
            city: region.city,
            district: region.district,
        })
    }
}

fn short_display_name(value: Option<&str>) -> Option<String> {
    value
        .and_then(|display| display.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn first_non_empty<const N: usize>(values: [Option<String>; N]) -> String {
    values
        .into_iter()
        .flatten()
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        CoordinateKey, EnabledGeocoder, GeocoderState, MIN_REQUEST_INTERVAL, NominatimAddress,
        ReverseGeocoder,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};
    use tokio::sync::Mutex;
    use url::Url;

    #[test]
    fn disabled_geocoder_does_not_parse_its_endpoint() -> anyhow::Result<()> {
        let geocoder = ReverseGeocoder::from_settings(false, "not a URL")?;
        anyhow::ensure!(geocoder.enabled.is_none());
        Ok(())
    }

    #[test]
    fn maps_nominatim_address_fallbacks() -> anyhow::Result<()> {
        let result = NominatimAddress {
            state: Some("四川省".to_string()),
            province: None,
            region: None,
            city: None,
            town: Some("成都市".to_string()),
            municipality: None,
            county: None,
            city_district: Some("武侯区".to_string()),
            district: None,
            borough: None,
            suburb: None,
        }
        .into_result();
        anyhow::ensure!(result.province == "四川省");
        anyhow::ensure!(result.city == "成都市");
        anyhow::ensure!(result.district == "武侯区");
        Ok(())
    }

    #[test]
    fn county_is_a_district_fallback() -> anyhow::Result<()> {
        let result = NominatimAddress {
            city: Some("杭州市".to_string()),
            county: Some("余杭区".to_string()),
            ..NominatimAddress::default()
        }
        .into_result();
        anyhow::ensure!(result.city == "杭州市");
        anyhow::ensure!(result.district == "余杭区");
        Ok(())
    }

    #[test]
    fn coordinate_cache_key_uses_four_decimal_places() -> anyhow::Result<()> {
        anyhow::ensure!(
            CoordinateKey::new(35.12344, 139.12344)? == CoordinateKey::new(35.12343, 139.12343)?
        );
        anyhow::ensure!(CoordinateKey::new(91.0, 0.0).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn rate_limit_wait_does_not_hold_the_state_lock() -> anyhow::Result<()> {
        let state = Arc::new(Mutex::new(GeocoderState {
            last_request: Some(Instant::now()),
            ..GeocoderState::default()
        }));
        let geocoder = EnabledGeocoder {
            endpoint: Url::parse("http://127.0.0.1:9/reverse")?,
            client: reqwest::Client::new(),
            state: Arc::clone(&state),
            coordinate_locks: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        };
        let task = tokio::spawn(async move { geocoder.acquire_request_slot().await });
        tokio::task::yield_now().await;

        let guard = tokio::time::timeout(Duration::from_millis(100), state.lock()).await?;
        drop(guard);
        task.abort();
        drop(task.await);
        anyhow::ensure!(MIN_REQUEST_INTERVAL > Duration::from_millis(100));
        Ok(())
    }

    #[tokio::test]
    async fn provider_error_is_rejected_without_caching() -> anyhow::Result<()> {
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = Arc::clone(&calls);
        let app = axum::Router::new().route(
            "/reverse",
            axum::routing::get(move || {
                let calls = Arc::clone(&server_calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    axum::Json(serde_json::json!({"error": "coordinates rejected"}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/reverse", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let geocoder = ReverseGeocoder::from_settings(true, &endpoint)?;

        anyhow::ensure!(geocoder.resolve(35.0, 105.0).await.is_err());
        let enabled = geocoder
            .enabled
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("test geocoder unexpectedly disabled"))?;
        anyhow::ensure!(enabled.state.lock().await.cache.is_empty());
        anyhow::ensure!(calls.load(Ordering::SeqCst) == 1);
        server.abort();
        drop(server.await);
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_same_coordinate_uses_one_upstream_request() -> anyhow::Result<()> {
        let calls = Arc::new(AtomicUsize::new(0));
        let server_calls = Arc::clone(&calls);
        let app = axum::Router::new().route(
            "/reverse",
            axum::routing::get(move || {
                let calls = Arc::clone(&server_calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    axum::Json(serde_json::json!({
                        "address": {"state": "四川省", "city": "成都市", "county": "武侯区"}
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/reverse", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let geocoder = ReverseGeocoder::from_settings(true, &endpoint)?;
        let first = geocoder.resolve(30.6, 104.0);
        let second = geocoder.resolve(30.6, 104.0);

        let (first, second) = tokio::join!(first, second);
        let first = first?;
        let second = second?;
        anyhow::ensure!(first.district == "武侯区");
        anyhow::ensure!(second.district == first.district);
        anyhow::ensure!(calls.load(Ordering::SeqCst) == 1);
        server.abort();
        drop(server.await);
        Ok(())
    }

    #[test]
    fn coordinate_lock_table_removes_expired_entries() -> anyhow::Result<()> {
        let geocoder = EnabledGeocoder {
            endpoint: Url::parse("http://127.0.0.1:9/reverse")?,
            client: reqwest::Client::new(),
            state: Arc::new(Mutex::new(GeocoderState::default())),
            coordinate_locks: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        };
        drop(geocoder.coordinate_lock(CoordinateKey::new(1.0, 1.0)?));
        let active = geocoder.coordinate_lock(CoordinateKey::new(2.0, 2.0)?);
        let locks = geocoder
            .coordinate_locks
            .lock()
            .map_err(|error| anyhow::anyhow!("coordinate lock table poisoned: {error}"))?;
        anyhow::ensure!(locks.len() == 1);
        drop(locks);
        drop(active);
        Ok(())
    }
}

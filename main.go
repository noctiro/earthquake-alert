package main

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"embed"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"html/template"
	"io"
	"log"
	"math"
	"net"
	"net/http"
	"net/url"
	"os"
	"path"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/gorilla/websocket"
	bolt "go.etcd.io/bbolt"
	"gopkg.in/yaml.v3"
)

//go:embed public/*
var publicFS embed.FS

type Config struct {
	Bark   BarkConfig   `yaml:"bark"`
	Server ServerConfig `yaml:"server"`
	Wolfx  WolfxConfig  `yaml:"wolfx"`
	Alert  AlertConfig  `yaml:"alert"`
}

type BarkConfig struct {
	Server           string `yaml:"server"`
	SelfHostedServer string `yaml:"self_hosted_server"`
	DeviceDBPath     string `yaml:"device_db_path"`
	Level            string `yaml:"level"`
	Sound            string `yaml:"sound"`
	Volume           int    `yaml:"volume"`
	Group            string `yaml:"group"`
	Call             bool   `yaml:"call"`
}

type ServerConfig struct {
	Host                      string `yaml:"host"`
	Port                      int    `yaml:"port"`
	DataPath                  string `yaml:"data_path"`
	HistoryPath               string `yaml:"history_path"`
	AuditPath                 string `yaml:"audit_path"`
	HistoryRefreshMinutes     int    `yaml:"history_refresh_minutes"`
	GeocodeProvider           string `yaml:"geocode_provider"`
	GeocodeURL                string `yaml:"geocode_url"`
	AmapKey                   string `yaml:"amap_key"`
	AmapPlaceURL              string `yaml:"amap_place_url"`
	AmapGeocodeURL            string `yaml:"amap_geocode_url"`
	AmapReverseURL            string `yaml:"amap_reverse_url"`
	PublicURL                 string `yaml:"public_url"`
	SimulateToken             string `yaml:"simulate_token"`
	SubscriptionPaused        bool   `yaml:"subscription_paused"`
	SubscriptionPausedMessage string `yaml:"subscription_paused_message"`
}

type WolfxConfig struct {
	WebSocketURL       string `yaml:"websocket_url"`
	ReconnectMinSecond int    `yaml:"reconnect_min_seconds"`
	ReconnectMaxSecond int    `yaml:"reconnect_max_seconds"`
}

type AlertConfig struct {
	PushUpdates           bool    `yaml:"push_updates"`
	UpdateMinReportGap    int     `yaml:"update_min_report_gap"`
	IgnoreTraining        bool    `yaml:"ignore_training"`
	IgnoreCancel          bool    `yaml:"ignore_cancel"`
	SWaveKMS              float64 `yaml:"s_wave_km_s"`
	PWaveKMS              float64 `yaml:"p_wave_km_s"`
	StaleOriginSecond     int     `yaml:"stale_origin_seconds"`
	DedupKeepMinutes      int     `yaml:"dedup_keep_minutes"`
	MaxDistanceKM         float64 `yaml:"max_distance_km"`
	FanoutConcurrency     int     `yaml:"fanout_concurrency"`
	SelfHostedConcurrency int     `yaml:"self_hosted_fanout_concurrency"`
	FanoutErrorBudget     int     `yaml:"fanout_error_budget"`
	KeyFailureThreshold   int     `yaml:"key_failure_threshold"`
	KeyQuarantineMinute   int     `yaml:"key_quarantine_minutes"`
	ClickURL              string  `yaml:"click_url"`
	WeChatURL             string  `yaml:"wechat_url"`
}

type AlertPage struct {
	Token      string
	Event      Event
	Decision   Decision
	Subscriber Subscription
	CreatedAt  time.Time
	WeChatURL  string
	MapURL     string
}

type PushOptions struct {
	Level  string
	Sound  string
	Volume int
	Call   bool
}

type AlertCache struct {
	mu    sync.RWMutex
	items map[string]AlertPage
	ttl   time.Duration
}

type Subscription struct {
	BarkID       string                 `json:"bark_id"`
	BarkServer   string                 `json:"bark_server,omitempty"`
	LocationName string                 `json:"location_name,omitempty"`
	Latitude     float64                `json:"latitude"`
	Longitude    float64                `json:"longitude"`
	Locations    []SubscriptionLocation `json:"locations,omitempty"`
	NotifyRules  NotificationRules      `json:"notify_rules"`
	NotifyBands  []NotificationBand     `json:"notify_bands,omitempty"`
	CreatedAt    int64                  `json:"created_at"`
	UpdatedAt    int64                  `json:"updated_at"`
}

type SubscriptionLocation struct {
	Name      string  `json:"name,omitempty"`
	Latitude  float64 `json:"latitude"`
	Longitude float64 `json:"longitude"`
}

type NotificationRules struct {
	PassiveMax  int `json:"passive_max"`
	ActiveMax   int `json:"active_max"`
	CriticalMin int `json:"critical_min"`
}

type NotificationBand struct {
	Min   int    `json:"min"`
	Max   int    `json:"max"`
	Level string `json:"level"`
	Label string `json:"label,omitempty"`
}

type GeocodeResult struct {
	Name      string  `json:"name"`
	Address   string  `json:"address"`
	Latitude  float64 `json:"latitude"`
	Longitude float64 `json:"longitude"`
}

type SafeJS = template.JS

var beijingTZ = time.FixedZone("Asia/Shanghai", 8*3600)

const notificationOpenEndedMax = 99

type Store struct {
	mu            sync.RWMutex
	path          string
	subscriptions map[string]Subscription
}

type APIResponse struct {
	Success bool   `json:"success"`
	Message string `json:"message"`
	Data    any    `json:"data,omitempty"`
}

type RawEvent map[string]any

type Event struct {
	Type          string
	EventID       string
	ReportNum     int
	OriginTime    time.Time
	AnnouncedTime time.Time
	Hypocenter    string
	Latitude      float64
	Longitude     float64
	Magnitude     float64
	DepthKM       float64
	MaxIntensity  string
	Final         bool
	Cancel        bool
	Training      bool
	Serial        string
	Raw           RawEvent
}

type HistoryRecord struct {
	Source             string  `json:"source"`
	Key                string  `json:"key"`
	EventID            string  `json:"event_id"`
	OriginTime         string  `json:"origin_time"`
	Hypocenter         string  `json:"hypocenter"`
	Latitude           float64 `json:"latitude"`
	Longitude          float64 `json:"longitude"`
	Magnitude          float64 `json:"magnitude"`
	DepthKM            float64 `json:"depth_km"`
	MaxIntensity       string  `json:"max_intensity"`
	Note               string  `json:"note,omitempty"`
	EstimatedIntensity int     `json:"estimated_intensity"`
	DistanceKM         float64 `json:"distance_km,omitempty"`
	HypocentralKM      float64 `json:"hypocentral_km,omitempty"`
}

type HistoryCacheFile struct {
	UpdatedAt int64           `json:"updated_at"`
	Records   []HistoryRecord `json:"records"`
}

type SimulationPreview struct {
	Kind               string  `json:"kind"`
	Label              string  `json:"label"`
	Magnitude          float64 `json:"magnitude"`
	MaxIntensity       string  `json:"max_intensity"`
	EstimatedIntensity int     `json:"estimated_intensity"`
	NotifyLevel        string  `json:"notify_level"`
	NotifyLabel        string  `json:"notify_label"`
	DistanceKM         float64 `json:"distance_km"`
	HypocentralKM      float64 `json:"hypocentral_km"`
}

type Decision struct {
	DistanceKM         float64
	HypocentralKM      float64
	EstimatedIntensity int
	SArrival           time.Time
	PArrival           time.Time
	SecondsToS         int
	SecondsToP         int
}

type Notifier struct {
	cfg        BarkConfig
	client     *http.Client
	errorGuard *BarkErrorGuard
}

type HTTPStatusError struct {
	StatusCode int
	Body       string
}

func (e *HTTPStatusError) Error() string {
	return fmt.Sprintf("http %d: %s", e.StatusCode, e.Body)
}

type BarkErrorGuard struct {
	mu                  sync.Mutex
	window              time.Duration
	budget              int
	keyFailureThreshold int
	keyQuarantine       time.Duration
	badRequests         []time.Time
	keys                map[string]keyFailure
}

type keyFailure struct {
	Count            int
	LastFailure      time.Time
	QuarantinedUntil time.Time
}

type Deduper struct {
	mu      sync.Mutex
	seen    map[string]seenEvent
	keepFor time.Duration
}

type seenEvent struct {
	ReportNum int
	At        time.Time
}

func main() {
	configPath := flag.String("config", "config.yaml", "path to config.yaml")
	testBark := flag.String("test-bark", "", "send a Bark test notification to this key and exit")
	flag.Parse()

	cfg, err := loadConfig(*configPath)
	if err != nil {
		log.Fatalf("load config: %v", err)
	}

	notifier := NewNotifier(cfg.Bark, cfg.Alert)
	if strings.TrimSpace(*testBark) != "" {
		err := notifier.Send(context.Background(), cfg.Bark.Server, strings.TrimSpace(*testBark), "EEW Bark 测试", "订阅服务", "配置可用，Bark 推送链路正常。", nil, PushOptions{Level: "passive"})
		if err != nil {
			log.Fatalf("test Bark failed: %v", err)
		}
		log.Println("test Bark notification sent")
		return
	}

	store, err := NewStore(cfg.Server.DataPath)
	if err != nil {
		log.Fatalf("load subscriptions: %v", err)
	}

	log.Printf("http=%s:%d subscriptions=%d websocket=%s",
		cfg.Server.Host, cfg.Server.Port, store.Count(), cfg.Wolfx.WebSocketURL)

	alertCache := NewAlertCache(2 * time.Hour)
	go func() {
		if err := serveHTTP(cfg, store, alertCache, notifier); err != nil {
			log.Fatalf("http server: %v", err)
		}
	}()

	deduper := NewDeduper(time.Duration(cfg.Alert.DedupKeepMinutes) * time.Minute)
	run(context.Background(), cfg, notifier, deduper, store, alertCache)
}

func loadConfig(path string) (Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return Config{}, err
	}

	var cfg Config
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return Config{}, err
	}

	if cfg.Bark.Server == "" {
		cfg.Bark.Server = "https://api.day.app"
	}
	if cfg.Bark.Level == "" {
		cfg.Bark.Level = "critical"
	}
	if cfg.Bark.Group == "" {
		cfg.Bark.Group = "地震预警"
	}
	if cfg.Wolfx.WebSocketURL == "" {
		cfg.Wolfx.WebSocketURL = "wss://ws-api.wolfx.jp/all_eew"
	}
	if cfg.Wolfx.ReconnectMinSecond <= 0 {
		cfg.Wolfx.ReconnectMinSecond = 1
	}
	if cfg.Wolfx.ReconnectMaxSecond <= 0 {
		cfg.Wolfx.ReconnectMaxSecond = 30
	}
	if cfg.Alert.SWaveKMS <= 0 {
		cfg.Alert.SWaveKMS = 3.5
	}
	if cfg.Alert.PWaveKMS <= 0 {
		cfg.Alert.PWaveKMS = 6.0
	}
	if cfg.Alert.StaleOriginSecond <= 0 {
		cfg.Alert.StaleOriginSecond = 600
	}
	if cfg.Alert.DedupKeepMinutes <= 0 {
		cfg.Alert.DedupKeepMinutes = 120
	}
	if cfg.Alert.UpdateMinReportGap <= 0 {
		cfg.Alert.UpdateMinReportGap = 1
	}
	if cfg.Alert.MaxDistanceKM <= 0 {
		cfg.Alert.MaxDistanceKM = 1000
	}
	if cfg.Alert.FanoutConcurrency <= 0 {
		cfg.Alert.FanoutConcurrency = 100
	}
	if cfg.Alert.SelfHostedConcurrency <= 0 {
		cfg.Alert.SelfHostedConcurrency = 1000
	}
	if cfg.Alert.FanoutErrorBudget <= 0 {
		cfg.Alert.FanoutErrorBudget = 800
	}
	if cfg.Alert.KeyFailureThreshold <= 0 {
		cfg.Alert.KeyFailureThreshold = 3
	}
	if cfg.Alert.KeyQuarantineMinute <= 0 {
		cfg.Alert.KeyQuarantineMinute = 24 * 60
	}
	if cfg.Alert.WeChatURL == "" {
		cfg.Alert.WeChatURL = "https://wxmpurl.cn/n87Wlase6iu"
	}
	if cfg.Server.Host == "" {
		cfg.Server.Host = "0.0.0.0"
	}
	if cfg.Server.Port <= 0 {
		cfg.Server.Port = 30010
	}
	if cfg.Server.DataPath == "" {
		cfg.Server.DataPath = "./data/subscriptions.json"
	}
	if cfg.Server.HistoryPath == "" {
		cfg.Server.HistoryPath = filepath.Join(filepath.Dir(cfg.Server.DataPath), "history.json")
	}
	if cfg.Server.AuditPath == "" {
		cfg.Server.AuditPath = filepath.Join(filepath.Dir(cfg.Server.DataPath), "audit")
	}
	if cfg.Server.HistoryRefreshMinutes <= 0 {
		cfg.Server.HistoryRefreshMinutes = 60
	}
	if cfg.Server.GeocodeProvider == "" {
		if cfg.Server.AmapKey != "" {
			cfg.Server.GeocodeProvider = "amap"
		} else {
			cfg.Server.GeocodeProvider = "nominatim"
		}
	}
	if cfg.Server.GeocodeURL == "" {
		cfg.Server.GeocodeURL = "https://nominatim.openstreetmap.org/search"
	}
	if cfg.Server.AmapPlaceURL == "" {
		cfg.Server.AmapPlaceURL = "https://restapi.amap.com/v3/place/text"
	}
	if cfg.Server.AmapGeocodeURL == "" {
		cfg.Server.AmapGeocodeURL = "https://restapi.amap.com/v3/geocode/geo"
	}
	if cfg.Server.AmapReverseURL == "" {
		cfg.Server.AmapReverseURL = "https://restapi.amap.com/v3/geocode/regeo"
	}
	if cfg.Server.SubscriptionPausedMessage == "" {
		cfg.Server.SubscriptionPausedMessage = "由于当前订阅人数较多，且 Bark 官方次数限制，再增加人数会影响预警时间。现决定停止订阅，将在后续恢复，现有已订阅用户不受影响，可正常接收地震预警。"
	}
	cfg.Bark.Server = strings.TrimRight(cfg.Bark.Server, "/")
	cfg.Bark.SelfHostedServer = strings.TrimRight(cfg.Bark.SelfHostedServer, "/")
	return cfg, nil
}

func NewStore(path string) (*Store, error) {
	store := &Store{
		path:          path,
		subscriptions: make(map[string]Subscription),
	}
	data, err := os.ReadFile(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return store, nil
		}
		return nil, err
	}
	if len(bytes.TrimSpace(data)) == 0 {
		return store, nil
	}
	var subs []Subscription
	if err := json.Unmarshal(data, &subs); err != nil {
		return nil, err
	}
	for _, sub := range subs {
		sub.BarkID = strings.TrimSpace(sub.BarkID)
		if sub.BarkID != "" {
			if strings.TrimSpace(sub.BarkServer) == "" {
				sub.BarkServer = "https://api.day.app"
			}
			normalizeSubscription(&sub)
			if len(sub.Locations) == 0 {
				log.Printf("skip subscription with no valid location bark=%s", maskKey(sub.BarkID))
				continue
			}
			store.subscriptions[sub.BarkID] = sub
		}
	}
	return store, nil
}

func (s *Store) Upsert(sub Subscription) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := time.Now().UnixMilli()
	existing, ok := s.subscriptions[sub.BarkID]
	if ok {
		sub.CreatedAt = existing.CreatedAt
	} else {
		sub.CreatedAt = now
	}
	normalizeSubscription(&sub)
	sub.UpdatedAt = now
	s.subscriptions[sub.BarkID] = sub
	return s.saveLocked()
}

func (s *Store) Delete(barkID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.subscriptions, barkID)
	return s.saveLocked()
}

func (s *Store) Get(barkID string) (Subscription, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	sub, ok := s.subscriptions[barkID]
	return sub, ok
}

func (s *Store) List() []Subscription {
	s.mu.RLock()
	defer s.mu.RUnlock()
	subs := make([]Subscription, 0, len(s.subscriptions))
	for _, sub := range s.subscriptions {
		subs = append(subs, sub)
	}
	return subs
}

func (s *Store) Count() int {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return len(s.subscriptions)
}

func (s *Store) saveLocked() error {
	subs := make([]Subscription, 0, len(s.subscriptions))
	for _, sub := range s.subscriptions {
		subs = append(subs, sub)
	}
	data, err := json.MarshalIndent(subs, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(s.path), 0o755); err != nil {
		return err
	}
	tmp := s.path + ".tmp"
	if err := os.WriteFile(tmp, data, 0o640); err != nil {
		return err
	}
	return os.Rename(tmp, s.path)
}

func NewAlertCache(ttl time.Duration) *AlertCache {
	return &AlertCache{
		items: make(map[string]AlertPage),
		ttl:   ttl,
	}
}

func (c *AlertCache) Put(page AlertPage) (string, error) {
	token, err := randomToken()
	if err != nil {
		return "", err
	}
	page.Token = token
	page.CreatedAt = time.Now()

	c.mu.Lock()
	defer c.mu.Unlock()
	now := time.Now()
	for key, item := range c.items {
		if now.Sub(item.CreatedAt) > c.ttl {
			delete(c.items, key)
		}
	}
	c.items[token] = page
	return token, nil
}

func (c *AlertCache) Get(token string) (AlertPage, bool) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	page, ok := c.items[token]
	if !ok || time.Since(page.CreatedAt) > c.ttl {
		return AlertPage{}, false
	}
	return page, true
}

func randomToken() (string, error) {
	var b [18]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b[:]), nil
}

func serveHTTP(cfg Config, store *Store, alertCache *AlertCache, notifier *Notifier) error {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /health", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, http.StatusOK, APIResponse{Success: true, Message: "ok", Data: map[string]any{"subscriptions": store.Count()}})
	})
	mux.HandleFunc("GET /api/stats", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, http.StatusOK, APIResponse{
			Success: true,
			Message: "ok",
			Data: map[string]any{
				"total_subscriptions":         store.Count(),
				"subscription_paused":         cfg.Server.SubscriptionPaused,
				"subscription_paused_message": cfg.Server.SubscriptionPausedMessage,
			},
		})
	})
	mux.HandleFunc("GET /api/geocode", func(w http.ResponseWriter, r *http.Request) {
		query := strings.TrimSpace(r.URL.Query().Get("q"))
		if len([]rune(query)) < 2 {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "请输入至少两个字的地址"})
			return
		}
		results, err := geocodeAddress(r.Context(), cfg, query)
		if err != nil {
			log.Printf("geocode failed q=%q: %v", query, err)
			writeJSON(w, http.StatusBadGateway, APIResponse{Success: false, Message: "地址搜索暂时不可用，请手动点击地图或输入经纬度"})
			return
		}
		if len(results) == 0 {
			writeJSON(w, http.StatusNotFound, APIResponse{Success: false, Message: "未找到匹配地址，请换个关键词"})
			return
		}
		writeJSON(w, http.StatusOK, APIResponse{Success: true, Message: "ok", Data: results})
	})
	mux.HandleFunc("GET /api/reverse-geocode", func(w http.ResponseWriter, r *http.Request) {
		lat, latErr := strconv.ParseFloat(strings.TrimSpace(r.URL.Query().Get("lat")), 64)
		lon, lonErr := strconv.ParseFloat(strings.TrimSpace(r.URL.Query().Get("lon")), 64)
		if latErr != nil || lonErr != nil || !validCoordinate(lat, lon) {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "经纬度无效"})
			return
		}
		result, err := reverseGeocodeAddress(r.Context(), cfg, lat, lon)
		if err != nil {
			log.Printf("reverse geocode failed lat=%.5f lon=%.5f: %v", lat, lon, err)
			writeJSON(w, http.StatusBadGateway, APIResponse{Success: false, Message: "地点名称解析暂时不可用"})
			return
		}
		writeJSON(w, http.StatusOK, APIResponse{Success: true, Message: "ok", Data: result})
	})
	mux.HandleFunc("GET /api/subscription/", func(w http.ResponseWriter, r *http.Request) {
		barkID, err := pathBarkID(r.URL.Path, "/api/subscription/")
		if err != nil {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "Bark Key 无效"})
			return
		}
		sub, ok := store.Get(barkID)
		if !ok {
			writeJSON(w, http.StatusNotFound, APIResponse{Success: false, Message: "未找到订阅"})
			return
		}
		writeJSON(w, http.StatusOK, APIResponse{Success: true, Message: "ok", Data: sub})
	})
	mux.HandleFunc("GET /api/bark-key/", func(w http.ResponseWriter, r *http.Request) {
		barkID, err := pathBarkID(r.URL.Path, "/api/bark-key/")
		if err != nil {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "Bark Key 无效"})
			return
		}
		if sub, ok := store.Get(barkID); ok {
			source := "self_hosted"
			if isOfficialBarkServer(sub.BarkServer) {
				source = "existing_official_subscription"
			}
			writeJSON(w, http.StatusOK, APIResponse{Success: true, Message: "Bark Key 已验证", Data: map[string]any{"exists": true, "source": source}})
			return
		}
		exists, err := selfHostedBarkKeyExists(cfg, barkID)
		if err != nil {
			log.Printf("verify bark key failed key=%s: %v", maskKey(barkID), err)
			writeJSON(w, http.StatusServiceUnavailable, APIResponse{Success: false, Message: "暂时无法验证 Bark Key，请稍后再试"})
			return
		}
		if !exists {
			writeJSON(w, http.StatusNotFound, APIResponse{Success: false, Message: "未找到这个 Bark Key，请先在 Bark App 添加自建服务器并复制该服务器生成的 Key"})
			return
		}
		writeJSON(w, http.StatusOK, APIResponse{Success: true, Message: "Bark Key 已验证", Data: map[string]any{"exists": true, "source": "self_hosted"}})
	})
	mux.HandleFunc("GET /api/history", func(w http.ResponseWriter, r *http.Request) {
		serveHistoryAPI(w, r, cfg, store, "")
	})
	mux.HandleFunc("GET /api/history/", func(w http.ResponseWriter, r *http.Request) {
		barkID, err := pathBarkID(r.URL.Path, "/api/history/")
		if err != nil {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "Bark Key 无效"})
			return
		}
		serveHistoryAPI(w, r, cfg, store, barkID)
	})
	mux.HandleFunc("GET /api/simulations/", func(w http.ResponseWriter, r *http.Request) {
		barkID, err := pathBarkID(r.URL.Path, "/api/simulations/")
		if err != nil {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "Bark Key 无效"})
			return
		}
		sub, ok := store.Get(barkID)
		if !ok {
			writeJSON(w, http.StatusNotFound, APIResponse{Success: false, Message: "未找到订阅"})
			return
		}
		writeJSON(w, http.StatusOK, APIResponse{Success: true, Message: "ok", Data: simulationPreviews(cfg, sub)})
	})
	mux.HandleFunc("POST /api/simulate-history/", func(w http.ResponseWriter, r *http.Request) {
		barkID, err := pathBarkID(r.URL.Path, "/api/simulate-history/")
		if err != nil {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "Bark Key 无效"})
			return
		}
		sub, ok := store.Get(barkID)
		if !ok {
			writeJSON(w, http.StatusNotFound, APIResponse{Success: false, Message: "未找到订阅"})
			return
		}
		source := strings.ToLower(strings.TrimSpace(r.URL.Query().Get("source")))
		key := strings.TrimSpace(r.URL.Query().Get("key"))
		records, err := historyRecords(r.Context(), cfg, false)
		if err != nil {
			log.Printf("fetch history failed: %v", err)
			writeJSON(w, http.StatusBadGateway, APIResponse{Success: false, Message: "历史地震数据获取失败"})
			return
		}
		record, ok := findHistoryRecord(records, source, key)
		if !ok {
			writeJSON(w, http.StatusNotFound, APIResponse{Success: false, Message: "未找到历史地震记录"})
			return
		}
		event := historicalEvent(record)
		pushed, skipped := dispatchOne(r.Context(), cfg, notifier, alertCache, event, sub)
		writeJSON(w, http.StatusOK, APIResponse{
			Success: true,
			Message: "已使用历史真实地震数据发送测试预警",
			Data:    map[string]any{"pushed": pushed, "skipped": skipped, "event_id": event.EventID, "source": record.Source, "key": record.Key},
		})
	})
	mux.HandleFunc("POST /api/simulate/", func(w http.ResponseWriter, r *http.Request) {
		barkID, err := pathBarkID(r.URL.Path, "/api/simulate/")
		if err != nil {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "Bark Key 无效"})
			return
		}
		sub, ok := store.Get(barkID)
		if !ok {
			writeJSON(w, http.StatusNotFound, APIResponse{Success: false, Message: "未找到订阅"})
			return
		}
		kind := strings.ToLower(strings.TrimSpace(r.URL.Query().Get("kind")))
		if kind == "" {
			kind = "small"
		}
		event := simulatedEvent([]Subscription{sub}, kind)
		pushed, skipped := dispatchOne(r.Context(), cfg, notifier, alertCache, event, sub)
		writeJSON(w, http.StatusOK, APIResponse{
			Success: true,
			Message: "已向当前 Bark Key 发送模拟预警",
			Data:    map[string]any{"pushed": pushed, "skipped": skipped, "event_id": event.EventID},
		})
	})
	mux.HandleFunc("POST /api/simulate", func(w http.ResponseWriter, r *http.Request) {
		if cfg.Server.SimulateToken == "" || r.URL.Query().Get("token") != cfg.Server.SimulateToken {
			writeJSON(w, http.StatusForbidden, APIResponse{Success: false, Message: "forbidden"})
			return
		}
		kind := strings.ToLower(strings.TrimSpace(r.URL.Query().Get("kind")))
		if kind == "" {
			kind = "small"
		}
		event := simulatedEvent(store.List(), kind)
		if event.Magnitude <= 0 {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "没有订阅者可模拟"})
			return
		}
		pushed, skipped := dispatchEvent(r.Context(), cfg, notifier, store, alertCache, event, time.Now())
		writeJSON(w, http.StatusOK, APIResponse{
			Success: true,
			Message: "模拟预警已发送",
			Data:    map[string]any{"pushed": pushed, "skipped": skipped, "event_id": event.EventID},
		})
	})
	mux.HandleFunc("POST /api/subscribe", func(w http.ResponseWriter, r *http.Request) {
		var sub Subscription
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20)).Decode(&sub); err != nil {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "请求格式错误"})
			return
		}
		var err error
		sub.BarkID, sub.BarkServer, err = normalizeBarkInput(sub.BarkID, sub.BarkServer, cfg)
		if err != nil {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: err.Error()})
			return
		}
		existingSub, exists := store.Get(sub.BarkID)
		if exists && isOfficialBarkServer(existingSub.BarkServer) && isSelfHostedBarkServer(sub.BarkServer, cfg) {
			sub.BarkServer = existingSub.BarkServer
		}
		if !exists && isSelfHostedBarkServer(sub.BarkServer, cfg) {
			ok, err := selfHostedBarkKeyExists(cfg, sub.BarkID)
			if err != nil {
				log.Printf("verify bark key failed key=%s: %v", maskKey(sub.BarkID), err)
				writeJSON(w, http.StatusServiceUnavailable, APIResponse{Success: false, Message: "暂时无法验证 Bark Key，请稍后再试"})
				return
			}
			if !ok {
				writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "未找到这个 Bark Key，请先在 Bark App 添加自建服务器并复制该服务器生成的 Key"})
				return
			}
		}
		if err := validateSubscription(sub); err != nil {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: err.Error()})
			return
		}
		if cfg.Server.SubscriptionPaused {
			if !exists {
				writeJSON(w, http.StatusServiceUnavailable, APIResponse{Success: false, Message: cfg.Server.SubscriptionPausedMessage})
				return
			}
		}
		if err := store.Upsert(sub); err != nil {
			log.Printf("save subscription failed: %v", err)
			writeJSON(w, http.StatusInternalServerError, APIResponse{Success: false, Message: "保存订阅失败"})
			return
		}
		log.Printf("subscription upserted bark=%s server=%s lat=%.4f lon=%.4f", maskKey(sub.BarkID), sub.BarkServer, sub.Latitude, sub.Longitude)
		writeJSON(w, http.StatusOK, APIResponse{
			Success: true,
			Message: "订阅成功",
			Data: map[string]any{
				"bark_id":     sub.BarkID,
				"bark_server": sub.BarkServer,
				"manage_url":  publicURL(cfg, "/manage/"+url.PathEscape(sub.BarkID)),
			},
		})
	})
	mux.HandleFunc("DELETE /api/unsubscribe/", func(w http.ResponseWriter, r *http.Request) {
		barkID, err := url.PathUnescape(strings.TrimPrefix(r.URL.Path, "/api/unsubscribe/"))
		if err != nil || strings.TrimSpace(barkID) == "" {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: "Bark Key 无效"})
			return
		}
		barkID, err = normalizeBarkIDInput(barkID)
		if err != nil {
			writeJSON(w, http.StatusBadRequest, APIResponse{Success: false, Message: err.Error()})
			return
		}
		if err := store.Delete(barkID); err != nil {
			log.Printf("delete subscription failed: %v", err)
			writeJSON(w, http.StatusInternalServerError, APIResponse{Success: false, Message: "取消订阅失败"})
			return
		}
		log.Printf("subscription deleted bark=%s", maskKey(barkID))
		writeJSON(w, http.StatusOK, APIResponse{Success: true, Message: "已取消订阅"})
	})
	mux.HandleFunc("GET /alert/", func(w http.ResponseWriter, r *http.Request) {
		token := strings.TrimPrefix(r.URL.Path, "/alert/")
		page, ok := alertCache.Get(token)
		if !ok {
			http.Error(w, "预警详情不存在或已过期", http.StatusNotFound)
			return
		}
		renderAlertPage(w, page)
	})
	mux.HandleFunc("GET /manage", func(w http.ResponseWriter, r *http.Request) {
		renderManageEntryPage(w)
	})
	mux.HandleFunc("GET /manage/", func(w http.ResponseWriter, r *http.Request) {
		barkID, err := pathBarkID(r.URL.Path, "/manage/")
		if err != nil {
			http.Error(w, "Bark Key 无效", http.StatusBadRequest)
			return
		}
		renderManagePage(w, barkID)
	})
	mux.HandleFunc("GET /tutorial.md", func(w http.ResponseWriter, r *http.Request) {
		data, err := publicFS.ReadFile("public/tutorial.md")
		if err != nil {
			http.Error(w, "tutorial not found", http.StatusNotFound)
			return
		}
		w.Header().Set("Content-Type", "text/markdown; charset=utf-8")
		_, _ = w.Write(data)
	})
	mux.HandleFunc("GET /tutorial-assets/", func(w http.ResponseWriter, r *http.Request) {
		name := path.Base(r.URL.Path)
		if name == "." || name == "/" || strings.Contains(name, "..") {
			http.NotFound(w, r)
			return
		}
		data, err := publicFS.ReadFile("public/tutorial-assets/" + name)
		if err != nil {
			http.NotFound(w, r)
			return
		}
		switch strings.ToLower(path.Ext(name)) {
		case ".png":
			w.Header().Set("Content-Type", "image/png")
		case ".jpg", ".jpeg":
			w.Header().Set("Content-Type", "image/jpeg")
		default:
			w.Header().Set("Content-Type", "application/octet-stream")
		}
		w.Header().Set("Cache-Control", "public, max-age=86400")
		_, _ = w.Write(data)
	})
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/" {
			http.NotFound(w, r)
			return
		}
		data, err := publicFS.ReadFile("public/index.html")
		if err != nil {
			http.Error(w, "index not found", http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = w.Write(data)
	})

	addr := fmt.Sprintf("%s:%d", cfg.Server.Host, cfg.Server.Port)
	server := &http.Server{
		Addr:              addr,
		Handler:           logRequest(redirectPublicHTTP(mux, cfg)),
		ReadHeaderTimeout: 5 * time.Second,
	}
	log.Printf("http server listening on %s", addr)
	return server.ListenAndServe()
}

func redirectPublicHTTP(next http.Handler, cfg Config) http.Handler {
	publicURL := strings.TrimSpace(cfg.Server.PublicURL)
	if publicURL == "" {
		return next
	}
	parsed, err := url.Parse(publicURL)
	if err != nil || parsed.Host == "" || parsed.Scheme != "https" {
		return next
	}
	publicHost := strings.ToLower(parsed.Host)
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		host := strings.ToLower(r.Host)
		if host == publicHost && strings.EqualFold(r.Header.Get("X-Forwarded-Proto"), "http") {
			target := "https://" + parsed.Host + r.URL.RequestURI()
			http.Redirect(w, r, target, http.StatusMovedPermanently)
			return
		}
		next.ServeHTTP(w, r)
	})
}

func validateSubscription(sub Subscription) error {
	if err := validateBarkID(sub.BarkID); err != nil {
		return err
	}
	if strings.TrimSpace(sub.BarkServer) == "" {
		return errors.New("Bark 服务器不能为空")
	}
	normalizeSubscription(&sub)
	if len(sub.Locations) == 0 {
		return errors.New("请至少添加一个监测地点")
	}
	if len(sub.Locations) > 3 {
		return errors.New("监测地点最多支持 3 个")
	}
	for _, loc := range sub.Locations {
		if !validSubscriptionCoordinate(loc.Latitude, loc.Longitude) {
			return errors.New("监测地点经纬度无效")
		}
	}
	if err := validateNotificationRules(sub.NotifyRules); err != nil {
		return err
	}
	if err := validateNotificationBands(sub.NotifyBands); err != nil {
		return err
	}
	return nil
}

func validSubscriptionCoordinate(lat, lon float64) bool {
	return validCoordinate(lat, lon) && !(lat == 0 && lon == 0)
}

func geocodeAddress(ctx context.Context, cfg Config, query string) ([]GeocodeResult, error) {
	provider := strings.ToLower(strings.TrimSpace(cfg.Server.GeocodeProvider))
	if provider == "amap" || (provider == "" && strings.TrimSpace(cfg.Server.AmapKey) != "") {
		results, err := geocodeAmap(ctx, cfg, query)
		if err == nil && len(results) > 0 {
			return results, nil
		}
		fallbackResults, fallbackErr := geocodeNominatim(ctx, cfg, query)
		if fallbackErr == nil {
			return fallbackResults, nil
		}
		if err != nil {
			return nil, err
		}
		return nil, fallbackErr
	}
	return geocodeNominatim(ctx, cfg, query)
}

func reverseGeocodeAddress(ctx context.Context, cfg Config, lat, lon float64) (GeocodeResult, error) {
	if !validCoordinate(lat, lon) {
		return GeocodeResult{}, errors.New("invalid coordinate")
	}
	if strings.TrimSpace(cfg.Server.AmapKey) == "" {
		return GeocodeResult{
			Name:      fmt.Sprintf("%.4f, %.4f", lat, lon),
			Address:   fmt.Sprintf("%.4f, %.4f", lat, lon),
			Latitude:  lat,
			Longitude: lon,
		}, nil
	}
	return reverseGeocodeAmap(ctx, cfg, lat, lon)
}

func reverseGeocodeAmap(ctx context.Context, cfg Config, lat, lon float64) (GeocodeResult, error) {
	baseURL := strings.TrimSpace(cfg.Server.AmapReverseURL)
	if baseURL == "" {
		baseURL = "https://restapi.amap.com/v3/geocode/regeo"
	}
	u, err := url.Parse(baseURL)
	if err != nil {
		return GeocodeResult{}, err
	}
	gcjLat, gcjLon := wgs84ToGCJ02(lat, lon)
	values := u.Query()
	values.Set("key", strings.TrimSpace(cfg.Server.AmapKey))
	values.Set("location", fmt.Sprintf("%.6f,%.6f", gcjLon, gcjLat))
	values.Set("extensions", "base")
	values.Set("radius", "1000")
	u.RawQuery = values.Encode()

	var raw struct {
		Status    string `json:"status"`
		Info      string `json:"info"`
		InfoCode  string `json:"infocode"`
		Regeocode struct {
			FormattedAddress string `json:"formatted_address"`
			AddressComponent struct {
				Province string      `json:"province"`
				City     interface{} `json:"city"`
				District string      `json:"district"`
				Township string      `json:"township"`
			} `json:"addressComponent"`
		} `json:"regeocode"`
	}
	if err := getJSON(ctx, u.String(), &raw); err != nil {
		return GeocodeResult{}, err
	}
	if raw.Status != "1" {
		return GeocodeResult{}, fmt.Errorf("amap reverse geocode status %s %s", raw.InfoCode, raw.Info)
	}
	component := raw.Regeocode.AddressComponent
	address := formatAmapReverseAddress(
		raw.Regeocode.FormattedAddress,
		component.Province,
		stringFromAny(component.City),
		component.District,
		component.Township,
	)
	name := firstNonEmpty(
		address,
		strings.TrimSpace(component.Township),
		strings.TrimSpace(component.District),
		stringFromAny(component.City),
		strings.TrimSpace(component.Province),
		fmt.Sprintf("%.4f, %.4f", lat, lon),
	)
	return GeocodeResult{Name: name, Address: address, Latitude: lat, Longitude: lon}, nil
}

func formatAmapReverseAddress(formatted, province, city, district, township string) string {
	formatted = strings.TrimSpace(formatted)
	parts := []string{}
	for _, part := range []string{province, city, district, township} {
		part = strings.TrimSpace(part)
		if part == "" || part == "[]" {
			continue
		}
		if len(parts) > 0 && parts[len(parts)-1] == part {
			continue
		}
		parts = append(parts, part)
	}
	rest := formatted
	for _, part := range parts {
		rest = strings.TrimPrefix(rest, part)
	}
	rest = strings.TrimSpace(rest)
	if rest != "" {
		parts = append(parts, rest)
	}
	if len(parts) == 0 {
		return formatted
	}
	return strings.Join(parts, " ")
}

func geocodeAmap(ctx context.Context, cfg Config, query string) ([]GeocodeResult, error) {
	key := strings.TrimSpace(cfg.Server.AmapKey)
	if key == "" {
		return nil, errors.New("amap key is empty")
	}
	results, err := geocodeAmapPlace(ctx, cfg, query, key)
	if err == nil && len(results) > 0 {
		return results, nil
	}
	geoResults, geoErr := geocodeAmapGeo(ctx, cfg, query, key)
	if geoErr == nil {
		return geoResults, nil
	}
	if err != nil {
		return nil, err
	}
	return nil, geoErr
}

func geocodeAmapPlace(ctx context.Context, cfg Config, query, key string) ([]GeocodeResult, error) {
	baseURL := strings.TrimSpace(cfg.Server.AmapPlaceURL)
	if baseURL == "" {
		baseURL = "https://restapi.amap.com/v3/place/text"
	}
	u, err := url.Parse(baseURL)
	if err != nil {
		return nil, err
	}
	values := u.Query()
	values.Set("key", key)
	values.Set("keywords", query)
	values.Set("offset", "8")
	values.Set("page", "1")
	values.Set("extensions", "base")
	values.Set("citylimit", "false")
	u.RawQuery = values.Encode()

	var raw struct {
		Status   string `json:"status"`
		Info     string `json:"info"`
		InfoCode string `json:"infocode"`
		POIs     []struct {
			Name     string      `json:"name"`
			Address  interface{} `json:"address"`
			Location string      `json:"location"`
			Province string      `json:"pname"`
			City     interface{} `json:"cityname"`
			District string      `json:"adname"`
		} `json:"pois"`
	}
	if err := getJSON(ctx, u.String(), &raw); err != nil {
		return nil, err
	}
	if raw.Status != "1" {
		return nil, fmt.Errorf("amap place status %s %s", raw.InfoCode, raw.Info)
	}

	results := make([]GeocodeResult, 0, len(raw.POIs))
	seen := make(map[string]bool)
	for _, item := range raw.POIs {
		lat, lon, ok := parseAmapLocation(item.Location)
		if !ok {
			continue
		}
		name := strings.TrimSpace(item.Name)
		address := joinNonEmpty(" ", strings.TrimSpace(item.Province), stringFromAny(item.City), strings.TrimSpace(item.District), stringFromAny(item.Address))
		appendGeocodeResult(&results, seen, name, address, lat, lon)
		if len(results) >= 6 {
			break
		}
	}
	return results, nil
}

func geocodeAmapGeo(ctx context.Context, cfg Config, query, key string) ([]GeocodeResult, error) {
	baseURL := strings.TrimSpace(cfg.Server.AmapGeocodeURL)
	if baseURL == "" {
		baseURL = "https://restapi.amap.com/v3/geocode/geo"
	}
	u, err := url.Parse(baseURL)
	if err != nil {
		return nil, err
	}
	values := u.Query()
	values.Set("key", key)
	values.Set("address", query)
	u.RawQuery = values.Encode()

	var raw struct {
		Status   string `json:"status"`
		Info     string `json:"info"`
		InfoCode string `json:"infocode"`
		Geocodes []struct {
			FormattedAddress string `json:"formatted_address"`
			Province         string `json:"province"`
			City             string `json:"city"`
			District         string `json:"district"`
			Location         string `json:"location"`
		} `json:"geocodes"`
	}
	if err := getJSON(ctx, u.String(), &raw); err != nil {
		return nil, err
	}
	if raw.Status != "1" {
		return nil, fmt.Errorf("amap geocode status %s %s", raw.InfoCode, raw.Info)
	}

	results := make([]GeocodeResult, 0, len(raw.Geocodes))
	seen := make(map[string]bool)
	for _, item := range raw.Geocodes {
		lat, lon, ok := parseAmapLocation(item.Location)
		if !ok {
			continue
		}
		address := strings.TrimSpace(item.FormattedAddress)
		name := firstNonEmpty(address, strings.TrimSpace(item.District), strings.TrimSpace(item.City), strings.TrimSpace(item.Province), query)
		appendGeocodeResult(&results, seen, name, address, lat, lon)
		if len(results) >= 6 {
			break
		}
	}
	return results, nil
}

func geocodeNominatim(ctx context.Context, cfg Config, query string) ([]GeocodeResult, error) {
	baseURL := strings.TrimSpace(cfg.Server.GeocodeURL)
	if baseURL == "" {
		baseURL = "https://nominatim.openstreetmap.org/search"
	}
	u, err := url.Parse(baseURL)
	if err != nil {
		return nil, err
	}
	values := u.Query()
	values.Set("q", query)
	values.Set("format", "jsonv2")
	values.Set("limit", "6")
	values.Set("addressdetails", "1")
	values.Set("accept-language", "zh-CN,zh;q=0.9,en;q=0.4")
	if values.Get("countrycodes") == "" {
		values.Set("countrycodes", "cn")
	}
	u.RawQuery = values.Encode()

	reqCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(reqCtx, http.MethodGet, u.String(), nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", "earthquake-alert/1.0")
	req.Header.Set("Accept", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("geocode status %d", resp.StatusCode)
	}
	var raw []struct {
		DisplayName string `json:"display_name"`
		Name        string `json:"name"`
		Lat         string `json:"lat"`
		Lon         string `json:"lon"`
	}
	if err := json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(&raw); err != nil {
		return nil, err
	}
	results := make([]GeocodeResult, 0, len(raw))
	seen := make(map[string]bool)
	for _, item := range raw {
		lat, latErr := strconv.ParseFloat(strings.TrimSpace(item.Lat), 64)
		lon, lonErr := strconv.ParseFloat(strings.TrimSpace(item.Lon), 64)
		if latErr != nil || lonErr != nil || !validCoordinate(lat, lon) {
			continue
		}
		name := strings.TrimSpace(item.Name)
		address := strings.TrimSpace(item.DisplayName)
		if name == "" {
			name = firstAddressPart(address)
		}
		appendGeocodeResult(&results, seen, name, address, lat, lon)
	}
	return results, nil
}

func getJSON(ctx context.Context, endpoint string, target any) error {
	reqCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(reqCtx, http.MethodGet, endpoint, nil)
	if err != nil {
		return err
	}
	req.Header.Set("User-Agent", "earthquake-alert/1.0")
	req.Header.Set("Accept", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("geocode status %d", resp.StatusCode)
	}
	return json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(target)
}

func parseAmapLocation(location string) (float64, float64, bool) {
	parts := strings.Split(strings.TrimSpace(location), ",")
	if len(parts) != 2 {
		return 0, 0, false
	}
	gcjLon, lonErr := strconv.ParseFloat(strings.TrimSpace(parts[0]), 64)
	gcjLat, latErr := strconv.ParseFloat(strings.TrimSpace(parts[1]), 64)
	if latErr != nil || lonErr != nil || !validCoordinate(gcjLat, gcjLon) {
		return 0, 0, false
	}
	lat, lon := gcj02ToWGS84(gcjLat, gcjLon)
	return lat, lon, validCoordinate(lat, lon)
}

func appendGeocodeResult(results *[]GeocodeResult, seen map[string]bool, name, address string, lat, lon float64) {
	if !validCoordinate(lat, lon) {
		return
	}
	key := fmt.Sprintf("%.5f,%.5f", lat, lon)
	if seen[key] {
		return
	}
	seen[key] = true
	name = strings.TrimSpace(name)
	address = strings.TrimSpace(address)
	if name == "" {
		name = firstAddressPart(address)
	}
	if address == "" {
		address = name
	}
	*results = append(*results, GeocodeResult{Name: name, Address: address, Latitude: lat, Longitude: lon})
}

func stringFromAny(value interface{}) string {
	switch v := value.(type) {
	case string:
		return strings.TrimSpace(v)
	case []interface{}:
		parts := make([]string, 0, len(v))
		for _, item := range v {
			if s, ok := item.(string); ok && strings.TrimSpace(s) != "" {
				parts = append(parts, strings.TrimSpace(s))
			}
		}
		return strings.Join(parts, " ")
	default:
		return ""
	}
}

func joinNonEmpty(sep string, values ...string) string {
	parts := make([]string, 0, len(values))
	for _, value := range values {
		if trimmed := strings.TrimSpace(value); trimmed != "" && trimmed != "[]" {
			parts = append(parts, trimmed)
		}
	}
	return strings.Join(parts, sep)
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if trimmed := strings.TrimSpace(value); trimmed != "" {
			return trimmed
		}
	}
	return ""
}

func firstAddressPart(address string) string {
	parts := strings.Split(address, ",")
	for _, part := range parts {
		if trimmed := strings.TrimSpace(part); trimmed != "" {
			return trimmed
		}
	}
	return strings.TrimSpace(address)
}

func normalizeBarkIDInput(value string) (string, error) {
	key, _, err := normalizeBarkInput(value, "", Config{Bark: BarkConfig{Server: "https://api.day.app"}})
	return key, err
}

func normalizeBarkInput(value, preferredServer string, cfg Config) (string, string, error) {
	value = strings.TrimSpace(value)
	value = strings.Trim(value, "\"'")
	if value == "" {
		return "", "", errors.New("Bark Key 不能为空")
	}
	server := normalizeBarkServer(preferredServer, cfg)
	if strings.Contains(value, "://") {
		parsed, err := url.Parse(value)
		if err != nil || parsed.Host == "" {
			return "", "", errors.New("Bark URL 无效")
		}
		urlServer := strings.TrimRight(parsed.Scheme+"://"+strings.ToLower(parsed.Host), "/")
		if !isAllowedBarkServer(urlServer, cfg) {
			return "", "", errors.New("仅支持 api.day.app 或本服务自建 Bark URL")
		}
		server = urlServer
		parts := strings.Split(strings.Trim(parsed.EscapedPath(), "/"), "/")
		if len(parts) == 0 || strings.TrimSpace(parts[0]) == "" {
			return "", "", errors.New("Bark URL 中未找到 Key")
		}
		key, err := url.PathUnescape(parts[0])
		if err != nil {
			return "", "", errors.New("Bark URL 中的 Key 无效")
		}
		value = key
	}
	value = strings.TrimSpace(strings.Trim(value, "/"))
	if err := validateBarkID(value); err != nil {
		return "", "", err
	}
	return value, server, nil
}

func normalizeBarkServer(server string, cfg Config) string {
	server = strings.TrimRight(strings.TrimSpace(server), "/")
	if server == "" {
		server = strings.TrimRight(strings.TrimSpace(cfg.Bark.SelfHostedServer), "/")
	}
	if server == "" {
		server = strings.TrimRight(strings.TrimSpace(cfg.Bark.Server), "/")
	}
	if server == "" {
		server = "https://api.day.app"
	}
	return server
}

func isAllowedBarkServer(server string, cfg Config) bool {
	server = normalizeBarkServer(server, cfg)
	for _, allowed := range []string{cfg.Bark.Server, cfg.Bark.SelfHostedServer, "https://api.day.app"} {
		if server == normalizeBarkServer(allowed, cfg) {
			return true
		}
	}
	return false
}

func isSelfHostedBarkServer(server string, cfg Config) bool {
	server = normalizeBarkServer(server, cfg)
	selfHosted := strings.TrimRight(strings.TrimSpace(cfg.Bark.SelfHostedServer), "/")
	return selfHosted != "" && server == selfHosted
}

func isOfficialBarkServer(server string) bool {
	return strings.TrimRight(strings.TrimSpace(server), "/") == "https://api.day.app"
}

func selfHostedBarkKeyExists(cfg Config, barkID string) (bool, error) {
	barkID = strings.TrimSpace(barkID)
	if err := validateBarkID(barkID); err != nil {
		return false, err
	}
	path := strings.TrimSpace(cfg.Bark.DeviceDBPath)
	if path == "" {
		return false, errors.New("bark device db path is empty")
	}
	db, cleanup, err := openBarkDeviceDB(path)
	if err != nil {
		return false, err
	}
	defer cleanup()
	defer db.Close()

	var exists bool
	err = db.View(func(tx *bolt.Tx) error {
		bucket := tx.Bucket([]byte("device"))
		if bucket == nil {
			return errors.New("device bucket not found")
		}
		exists = bucket.Get([]byte(barkID)) != nil
		return nil
	})
	return exists, err
}

func openBarkDeviceDB(path string) (*bolt.DB, func(), error) {
	db, err := bolt.Open(path, 0o444, &bolt.Options{ReadOnly: true, Timeout: 50 * time.Millisecond})
	if err == nil {
		return db, func() {}, nil
	}

	src, readErr := os.Open(path)
	if readErr != nil {
		return nil, func() {}, err
	}
	defer src.Close()

	tmp, tmpErr := os.CreateTemp("", "eew-bark-device-*.db")
	if tmpErr != nil {
		return nil, func() {}, tmpErr
	}
	tmpPath := tmp.Name()
	cleanup := func() { _ = os.Remove(tmpPath) }
	if _, copyErr := io.Copy(tmp, src); copyErr != nil {
		_ = tmp.Close()
		cleanup()
		return nil, func() {}, copyErr
	}
	if closeErr := tmp.Close(); closeErr != nil {
		cleanup()
		return nil, func() {}, closeErr
	}
	db, openErr := bolt.Open(tmpPath, 0o444, &bolt.Options{ReadOnly: true, Timeout: 50 * time.Millisecond})
	if openErr != nil {
		cleanup()
		return nil, func() {}, openErr
	}
	return db, cleanup, nil
}

func validateBarkID(value string) error {
	if value == "" {
		return errors.New("Bark Key 不能为空")
	}
	if len(value) > 128 {
		return errors.New("Bark Key 过长")
	}
	for _, r := range value {
		if !(r >= 'a' && r <= 'z') && !(r >= 'A' && r <= 'Z') && !(r >= '0' && r <= '9') && r != '_' && r != '-' {
			return errors.New("Bark Key 只能包含字母、数字、下划线和连字符")
		}
	}
	return nil
}

func normalizeSubscription(sub *Subscription) {
	if sub.NotifyRules == (NotificationRules{}) {
		sub.NotifyRules = defaultNotificationRules()
	}
	sub.NotifyBands = normalizeNotificationBands(sub.NotifyBands, sub.NotifyRules)
	sub.LocationName = strings.TrimSpace(sub.LocationName)
	locations := make([]SubscriptionLocation, 0, len(sub.Locations)+1)
	seen := make(map[string]bool)
	for _, loc := range sub.Locations {
		appendSubscriptionLocation(&locations, seen, loc)
	}
	if len(locations) == 0 && validSubscriptionCoordinate(sub.Latitude, sub.Longitude) {
		appendSubscriptionLocation(&locations, seen, SubscriptionLocation{
			Name:      sub.LocationName,
			Latitude:  sub.Latitude,
			Longitude: sub.Longitude,
		})
	}
	if len(locations) > 3 {
		locations = locations[:3]
	}
	sub.Locations = locations
	if len(sub.Locations) > 0 {
		sub.LocationName = sub.Locations[0].Name
		sub.Latitude = sub.Locations[0].Latitude
		sub.Longitude = sub.Locations[0].Longitude
	}
}

func appendSubscriptionLocation(locations *[]SubscriptionLocation, seen map[string]bool, loc SubscriptionLocation) {
	loc.Name = strings.TrimSpace(loc.Name)
	if !validSubscriptionCoordinate(loc.Latitude, loc.Longitude) {
		return
	}
	key := fmt.Sprintf("%.5f,%.5f", loc.Latitude, loc.Longitude)
	if seen[key] {
		return
	}
	seen[key] = true
	*locations = append(*locations, loc)
}

func defaultNotificationRules() NotificationRules {
	return NotificationRules{PassiveMax: 1, ActiveMax: 2, CriticalMin: 3}
}

func defaultNotificationBands() []NotificationBand {
	return []NotificationBand{
		{Min: 1, Max: 1, Level: "passive", Label: "低烈度"},
		{Min: 2, Max: 2, Level: "active", Label: "中等烈度"},
		{Min: 3, Max: notificationOpenEndedMax, Level: "critical", Label: "高烈度"},
	}
}

func bandsFromNotificationRules(rules NotificationRules) []NotificationBand {
	if rules == (NotificationRules{}) {
		return defaultNotificationBands()
	}
	return []NotificationBand{
		{Min: 1, Max: rules.PassiveMax, Level: "passive", Label: "低烈度"},
		{Min: rules.PassiveMax + 1, Max: rules.ActiveMax, Level: "active", Label: "中等烈度"},
		{Min: rules.CriticalMin, Max: notificationOpenEndedMax, Level: "critical", Label: "高烈度"},
	}
}

func normalizeNotificationBands(bands []NotificationBand, rules NotificationRules) []NotificationBand {
	if bands == nil {
		bands = bandsFromNotificationRules(rules)
	}
	out := make([]NotificationBand, 0, len(bands))
	for _, band := range bands {
		band.Level = normalizeNotifyLevel(band.Level)
		band.Label = strings.TrimSpace(band.Label)
		if band.Label == "" {
			band.Label = intensityBandLabel(band.Level)
		}
		if band.Min < 0 {
			band.Min = 0
		}
		if band.Level == "critical" {
			band.Max = notificationOpenEndedMax
		}
		if band.Max > notificationOpenEndedMax {
			band.Max = notificationOpenEndedMax
		}
		out = append(out, band)
	}
	sort.SliceStable(out, func(i, j int) bool {
		if out[i].Min != out[j].Min {
			return out[i].Min < out[j].Min
		}
		return out[i].Max < out[j].Max
	})
	return out
}

func validateNotificationBands(bands []NotificationBand) error {
	if len(bands) == 0 {
		return errors.New("请至少保留一条通知级别规则")
	}
	if len(bands) > 3 {
		return errors.New("通知级别规则最多 3 条")
	}
	levels := map[string]bool{}
	used := map[int]bool{}
	for _, band := range bands {
		if band.Min < 0 || band.Min > 7 || band.Max < 0 || band.Max > notificationOpenEndedMax || band.Min > band.Max {
			return errors.New("通知级别烈度范围必须在 0 到 7+ 之间")
		}
		if !validNotifyLevel(band.Level) {
			return errors.New("通知级别只能选择 passive、active 或 critical")
		}
		if levels[band.Level] {
			return errors.New("每个通知级别只能添加一条规则")
		}
		levels[band.Level] = true
		if band.Level == "critical" && band.Max != notificationOpenEndedMax {
			return errors.New("高烈度上限必须为 7+")
		}
		if band.Level != "critical" && band.Max > 7 {
			return errors.New("只有高烈度上限可以设置为 7+")
		}
		for intensity := band.Min; intensity <= band.Max; intensity++ {
			if used[intensity] {
				return errors.New("通知级别烈度范围不能重叠")
			}
			used[intensity] = true
		}
	}
	return nil
}

func validateNotificationRules(rules NotificationRules) error {
	if rules == (NotificationRules{}) {
		return nil
	}
	if rules.PassiveMax < 0 || rules.PassiveMax > 7 || rules.ActiveMax < 0 || rules.ActiveMax > 7 || rules.CriticalMin < 0 || rules.CriticalMin > 7 {
		return errors.New("通知级别烈度范围必须在 0 到 7 之间")
	}
	if rules.PassiveMax >= rules.ActiveMax || rules.ActiveMax >= rules.CriticalMin {
		return errors.New("通知级别范围必须满足 passive < active < critical")
	}
	if rules.ActiveMax+1 != rules.CriticalMin {
		return errors.New("critical 起始烈度必须紧接 active 最高烈度，例如 active 到 2、critical 从 3 开始")
	}
	return nil
}

func notifyLevelForIntensity(sub Subscription, intensity int) string {
	band, ok := notifyBandForIntensity(sub, intensity)
	if !ok {
		return ""
	}
	return band.Level
}

func notifyBandForIntensity(sub Subscription, intensity int) (NotificationBand, bool) {
	bands := normalizeNotificationBands(sub.NotifyBands, sub.NotifyRules)
	for _, band := range bands {
		if intensity >= band.Min && intensity <= band.Max {
			return band, true
		}
	}
	return NotificationBand{}, false
}

func normalizeNotifyLevel(level string) string {
	switch strings.ToLower(strings.TrimSpace(level)) {
	case "critical":
		return "critical"
	case "active", "timesensitive", "timeSensitive":
		return "active"
	default:
		return "passive"
	}
}

func validNotifyLevel(level string) bool {
	switch strings.ToLower(strings.TrimSpace(level)) {
	case "passive", "active", "timesensitive", "timeSensitive", "critical":
		return true
	default:
		return false
	}
}

func notifyLabel(level string) string {
	switch level {
	case "critical":
		return "critical + sound/volume + call"
	case "active":
		return "active"
	default:
		return "passive"
	}
}

func intensityBandLabel(level string) string {
	switch strings.ToLower(strings.TrimSpace(level)) {
	case "critical":
		return "高烈度"
	case "active", "timesensitive":
		return "中等烈度"
	default:
		return "低烈度"
	}
}

func pathBarkID(pathValue, prefix string) (string, error) {
	value, err := url.PathUnescape(strings.TrimPrefix(pathValue, prefix))
	if err != nil {
		return "", err
	}
	value = strings.TrimSpace(value)
	value = strings.Trim(value, "\"'")
	if value == "" || strings.Contains(value, "/") {
		return "", errors.New("invalid bark id")
	}
	return value, nil
}

func publicURL(cfg Config, pathValue string) string {
	base := strings.TrimRight(strings.TrimSpace(cfg.Server.PublicURL), "/")
	if base == "" {
		return pathValue
	}
	return base + pathValue
}

func formatSubscriptionLocation(sub Subscription) string {
	coords := fmt.Sprintf("%.4f, %.4f", sub.Latitude, sub.Longitude)
	if name := strings.TrimSpace(sub.LocationName); name != "" {
		return name + " (" + coords + ")"
	}
	return coords
}

func writeJSON(w http.ResponseWriter, status int, value APIResponse) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}

func renderAlertPage(w http.ResponseWriter, page AlertPage) {
	view := struct {
		EventID            string
		Region             string
		Source             string
		ReportNum          int
		Magnitude          string
		Depth              string
		MaxIntensity       string
		Epicenter          string
		SubscriberLocation string
		Distance           string
		Hypocentral        string
		EstimatedIntensity int
		SecondsToP         int
		SecondsToS         int
		PArrival           string
		SArrival           string
		OriginTime         string
		CreatedAt          string
		WeChatURL          string
		MapURL             string
		EpicenterLat       string
		EpicenterLon       string
		SubscriberLat      string
		SubscriberLon      string
		MapEpicenterLat    string
		MapEpicenterLon    string
		MapSubscriberLat   string
		MapSubscriberLon   string
		SArrivalUnix       int64
		PArrivalUnix       int64
		IsTest             bool
		ManageURL          string
		BarkIDJSON         SafeJS
	}{
		EventID:            page.Event.EventID,
		Region:             fallback(page.Event.Hypocenter, "未知区域"),
		Source:             page.Event.Type,
		ReportNum:          page.Event.ReportNum,
		Magnitude:          fmt.Sprintf("M%.1f", page.Event.Magnitude),
		Depth:              fmt.Sprintf("%.0f km", page.Event.DepthKM),
		MaxIntensity:       fallback(page.Event.MaxIntensity, "未知"),
		Epicenter:          fmt.Sprintf("%.4f, %.4f", page.Event.Latitude, page.Event.Longitude),
		SubscriberLocation: formatSubscriptionLocation(page.Subscriber),
		Distance:           fmt.Sprintf("%.1f km", page.Decision.DistanceKM),
		Hypocentral:        fmt.Sprintf("%.1f km", page.Decision.HypocentralKM),
		EstimatedIntensity: page.Decision.EstimatedIntensity,
		SecondsToP:         page.Decision.SecondsToP,
		SecondsToS:         page.Decision.SecondsToS,
		PArrival:           formatBeijing(page.Decision.PArrival, "15:04:05"),
		SArrival:           formatBeijing(page.Decision.SArrival, "15:04:05"),
		CreatedAt:          formatBeijing(page.CreatedAt, "2006-01-02 15:04:05"),
		WeChatURL:          page.WeChatURL,
		MapURL:             page.MapURL,
		EpicenterLat:       fmt.Sprintf("%.6f", page.Event.Latitude),
		EpicenterLon:       fmt.Sprintf("%.6f", page.Event.Longitude),
		SubscriberLat:      fmt.Sprintf("%.6f", page.Subscriber.Latitude),
		SubscriberLon:      fmt.Sprintf("%.6f", page.Subscriber.Longitude),
		SArrivalUnix:       page.Decision.SArrival.UnixMilli(),
		PArrivalUnix:       page.Decision.PArrival.UnixMilli(),
		IsTest:             isTestEvent(page.Event),
		ManageURL:          "/manage/" + url.PathEscape(page.Subscriber.BarkID),
	}
	barkIDJSON, _ := json.Marshal(page.Subscriber.BarkID)
	view.BarkIDJSON = SafeJS(barkIDJSON)
	mapEventLat, mapEventLon := wgs84ToGCJ02(page.Event.Latitude, page.Event.Longitude)
	mapSubLat, mapSubLon := wgs84ToGCJ02(page.Subscriber.Latitude, page.Subscriber.Longitude)
	view.MapEpicenterLat = fmt.Sprintf("%.6f", mapEventLat)
	view.MapEpicenterLon = fmt.Sprintf("%.6f", mapEventLon)
	view.MapSubscriberLat = fmt.Sprintf("%.6f", mapSubLat)
	view.MapSubscriberLon = fmt.Sprintf("%.6f", mapSubLon)
	view.OriginTime = alertOriginTimeLabel(page.Event)

	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := alertPageTemplate.Execute(w, view); err != nil {
		log.Printf("render alert page failed: %v", err)
	}
}

func renderManagePage(w http.ResponseWriter, barkID string) {
	keyJSON, _ := json.Marshal(barkID)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := managePageTemplate.Execute(w, struct {
		BarkIDJSON SafeJS
	}{BarkIDJSON: SafeJS(keyJSON)}); err != nil {
		log.Printf("render manage page failed: %v", err)
	}
}

func renderManageEntryPage(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := manageEntryTemplate.Execute(w, nil); err != nil {
		log.Printf("render manage entry page failed: %v", err)
	}
}

var manageEntryTemplate = template.Must(template.New("manage-entry").Parse(`<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover">
  <title>测试页</title>
  <style>
    :root{color-scheme:light dark;--bg:#f6f7f9;--panel:#fff;--text:#171717;--muted:#667085;--line:#d8dee7;--red:#d92d20;--soft:#f2f4f7}
    @media(prefers-color-scheme:dark){:root{--bg:#101214;--panel:#1c1f23;--text:#f5f7fa;--muted:#a5acb5;--line:#3a4048;--soft:#252a31}}
    *{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}main{width:min(560px,calc(100% - 24px));margin:0 auto;padding:24px 0 calc(28px + env(safe-area-inset-bottom))}.top-nav{display:flex;gap:8px;align-items:center;margin-bottom:18px}.top-nav a{height:40px;border-radius:8px;border:1px solid var(--line);background:var(--panel);color:var(--text);text-decoration:none;font-weight:900;display:flex;align-items:center;padding:0 13px}.top-nav a.active{background:var(--red);border-color:var(--red);color:#fff}.panel{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:16px}h1{font-size:28px;margin:0 0 8px}.muted{color:var(--muted);line-height:1.55;margin:0 0 16px}label{display:block;font-weight:900;margin-bottom:8px}input{width:100%;height:48px;border:1px solid var(--line);border-radius:8px;background:var(--panel);color:var(--text);font:inherit;padding:0 12px}button{width:100%;height:48px;border:0;border-radius:8px;background:var(--red);color:#fff;font:inherit;font-weight:900;margin-top:12px}.status{margin-top:10px;color:var(--muted);line-height:1.45}.err{color:var(--red)}@media(max-width:560px){input,select,textarea{font-size:16px}}
  </style>
</head>
<body>
<main>
  <nav class="top-nav" aria-label="页面导航"><a href="/">订阅页</a><a class="active" href="/manage">测试页</a></nav>
  <section class="panel">
    <h1>测试页</h1>
    <p class="muted">如果本机保存过 Bark Key，会自动进入测试页。也可以在这里输入 Bark Key 打开测试页面。</p>
    <label for="bark-id">Bark Key</label>
    <input id="bark-id" autocomplete="off" placeholder="例如：vRvm6tubpnHJYsX9fE2EYQ">
    <button id="open-manage" type="button">打开测试页</button>
    <div class="status" id="status"></div>
  </section>
</main>
<script>
  const input=document.getElementById("bark-id"), statusEl=document.getElementById("status");
  function extractKey(value){
    value=String(value||"").trim();
    try{if(value.includes("://")){const u=new URL(value); const parts=u.pathname.split("/").filter(Boolean); if(parts.length) value=parts[0];}}catch(e){}
    return value.trim().replace(/^["']|["']$/g,"");
  }
  function openManage(){
    const key=extractKey(input.value||localStorage.getItem("eew_bark_id")||"");
    if(!key){statusEl.textContent="请先输入 Bark Key。";statusEl.className="status err";input.focus();return;}
    localStorage.setItem("eew_bark_id",key);
    location.href="/manage/"+encodeURIComponent(key);
  }
  const saved=extractKey(localStorage.getItem("eew_bark_id")||"");
  if(saved){input.value=saved; location.replace("/manage/"+encodeURIComponent(saved));}
  document.getElementById("open-manage").addEventListener("click",openManage);
  input.addEventListener("keydown",function(e){if(e.key==="Enter") openManage();});
</script>
</body>
</html>`))

var managePageTemplate = template.Must(template.New("manage").Parse(`<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover">
  <title>地震预警测试页</title>
  <style>
    :root{color-scheme:light dark;--bg:#f6f7f9;--panel:#fff;--text:#171717;--muted:#667085;--line:#d8dee7;--red:#d92d20;--blue:#175cd3;--soft:#f2f4f7;--ok:#087443;--warn:#b54708}
    @media(prefers-color-scheme:dark){:root{--bg:#101214;--panel:#1c1f23;--text:#f5f7fa;--muted:#a5acb5;--line:#3a4048;--soft:#252a31}}
    *{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}
    main{width:min(680px,calc(100% - 24px));margin:0 auto;padding:24px 0 calc(28px + env(safe-area-inset-bottom))}
    .top-nav{display:flex;gap:8px;align-items:center;margin-bottom:18px}.top-nav a{height:40px;border-radius:8px;border:1px solid var(--line);background:var(--panel);color:var(--text);text-decoration:none;font-weight:900;display:flex;align-items:center;padding:0 13px}.top-nav a.active{background:var(--red);border-color:var(--red);color:#fff}
    h1{font-size:28px;margin:0 0 8px}h2{font-size:18px;margin:0 0 8px}.muted{color:var(--muted);line-height:1.55}.panel{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:16px;margin-top:14px}
    dl{display:grid;grid-template-columns:104px 1fr;gap:10px 12px;margin:0}dt{color:var(--muted)}dd{margin:0;font-weight:750;word-break:break-word}.key-row{display:flex;align-items:center;gap:4px;min-width:0}.key-value{display:inline-flex;align-items:center;min-height:32px;max-width:calc(100% - 76px);overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-variant-numeric:tabular-nums}.key-action{width:32px;height:32px;border-radius:8px;border:1px solid var(--line);background:var(--soft);color:var(--text);padding:0;flex:0 0 auto}.key-action svg{width:17px;height:17px;stroke:currentColor;stroke-width:2;fill:none;stroke-linecap:round;stroke-linejoin:round}.key-action.copied{color:var(--ok);border-color:rgba(8,116,67,.45);background:rgba(8,116,67,.1)}.key-action:active{transform:translateY(1px)}#pos{display:grid;gap:8px;font-weight:700}.location-row{display:grid;gap:4px;border:1px solid var(--line);border-radius:8px;padding:9px 10px;background:var(--soft)}.location-coords{font-weight:900;color:var(--text);font-variant-numeric:tabular-nums}.location-address{color:var(--muted);font-size:13px;line-height:1.45;font-weight:650}
    .actions{display:grid;grid-template-columns:1fr 1fr;gap:10px;margin-top:14px}.actions.single{grid-template-columns:1fr}button,a.btn{height:50px;border-radius:8px;border:0;display:flex;align-items:center;justify-content:center;text-decoration:none;font-weight:900;font-size:15px;font-family:inherit}
    .notify-card{border:1px solid var(--line);border-radius:8px;overflow:hidden;background:var(--panel);margin-top:14px}.threshold-row{display:grid;grid-template-columns:1fr 150px;gap:12px;align-items:center;padding:12px;background:var(--soft);border-bottom:1px solid var(--line)}.setting-title{font-weight:900;line-height:1.25}.setting-note{margin-top:4px;color:var(--muted);font-size:13px;line-height:1.35}.rule-rows{display:grid}.rule-row{display:grid;grid-template-columns:minmax(120px,1fr) minmax(220px,1.35fr) 58px;gap:10px;align-items:center;padding:12px;border-top:1px solid var(--line)}.rule-row:first-child{border-top:0}.range-fields{display:grid;grid-template-columns:auto minmax(58px,80px);gap:7px;align-items:center;justify-content:start}.range-fields span{color:var(--muted);font-weight:900;white-space:nowrap}.range-value{font-weight:900;color:var(--text);font-variant-numeric:tabular-nums}.band-choice{min-height:46px;border:1px solid var(--line);border-radius:8px;padding:7px 12px;display:flex;align-items:center}.band-text{display:grid;gap:2px;min-width:0}.band-main{font-weight:900;line-height:1.15}.band-sub{font-size:12px;font-weight:800;line-height:1.2;opacity:.78;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}.level-passive{color:#344054;background:#eef2f6;border-color:#d5dbe5}.level-active{color:#175cd3;background:#eff6ff;border-color:#bfd7ff}.level-critical{color:#b42318;background:#fff1f0;border-color:#ffc9c4}.level-test{height:40px;min-width:58px;border-radius:8px;padding:0 10px;font-size:13px;background:var(--panel);border:1px solid var(--line);color:var(--text);font-weight:900}
    .simulate{line-height:1.1}.btn-title{font-weight:900}.btn-sub{font-size:12px;color:var(--muted);font-weight:800}.primary .btn-sub{color:rgba(255,255,255,.86)}
    .primary{background:var(--red);color:#fff}.secondary{background:var(--soft);color:var(--text);border:1px solid var(--line)}.status{display:none;margin-top:12px;padding:12px;border-radius:8px;background:var(--soft);line-height:1.5}.status.show{display:block}.ok{color:var(--ok)}.err{color:var(--red)}
    .history-head{display:flex;align-items:flex-start;justify-content:space-between;gap:12px;margin-bottom:12px}.history-head .muted{font-size:14px}.history-tools{display:grid;grid-template-columns:120px 120px auto;gap:8px;margin-bottom:12px}.history-tools select{height:42px;border:1px solid var(--line);border-radius:8px;background:var(--panel);color:var(--text);padding:0 10px;font:inherit}.history-tools button{height:42px;padding:0 14px}.history-list,.major-list{display:grid;gap:10px}.history-empty{padding:14px;border:1px dashed var(--line);border-radius:8px;color:var(--muted);line-height:1.5}.pager{display:grid;grid-template-columns:1fr auto auto;gap:8px;align-items:center;margin-top:12px}.pager button{height:40px;padding:0 14px}.pager button:disabled{opacity:.45}.page-info{color:var(--muted);font-size:13px}
    .history-item{display:grid;grid-template-columns:1fr auto;gap:10px;align-items:center;border:1px solid var(--line);border-radius:8px;padding:12px;background:var(--soft)}.history-title{font-weight:900;margin-bottom:5px}.history-meta{display:flex;flex-wrap:wrap;gap:6px 10px;color:var(--muted);font-size:13px;line-height:1.4}.badge{display:inline-flex;align-items:center;height:22px;padding:0 8px;border-radius:999px;background:rgba(23,92,211,.12);color:var(--blue);font-weight:900;font-size:12px;text-transform:uppercase}.estimate{color:var(--red);font-weight:900}.history-test{min-width:108px;height:42px;padding:0 12px}.history-note{margin-top:10px;color:var(--warn);font-size:13px;line-height:1.5}
    @media(max-width:560px){input,select,textarea{font-size:16px}.actions{grid-template-columns:1fr 1fr}.actions .btn{grid-column:1/-1}dl{grid-template-columns:88px 1fr}.threshold-row{grid-template-columns:1fr}.rule-row{grid-template-columns:minmax(82px,0.9fr) minmax(150px,1.4fr) 54px;gap:6px;padding:10px 8px;align-items:center}.range-fields{grid-column:1/2;grid-template-columns:auto minmax(42px,1fr);gap:5px;justify-content:stretch}.band-choice{grid-column:2/3;min-height:38px;padding:5px 9px}.level-test{grid-column:3/4;width:100%;height:38px;min-width:0;padding:0 6px}.band-main{font-size:13px}.band-sub{font-size:10px}.history-head{display:block}.history-tools{grid-template-columns:1fr 1fr}.history-tools button{grid-column:1/-1}.history-item{grid-template-columns:1fr}.history-test{width:100%}.pager{grid-template-columns:1fr 1fr}.page-info{grid-column:1/-1}}
  </style>
</head>
<body>
<main>
  <nav class="top-nav" aria-label="页面导航"><a href="/">订阅页</a><a class="active" href="/manage">测试页</a></nav>
  <h1>测试页</h1>
  <div class="muted">当前页面展示这个 Bark Key 在订阅页保存的配置。测试推送只会发送到这个 Key，不影响其他用户。</div>
  <section class="panel">
    <dl>
      <dt>Bark Key</dt><dd><div class="key-row"><span class="key-value" id="bark"></span><button class="key-action" id="toggle-key" type="button" title="显示 Bark Key" aria-label="显示 Bark Key"></button><button class="key-action" id="copy-key" type="button" title="复制 Bark Key" aria-label="复制 Bark Key"></button></div></dd>
      <dt>Bark 服务器</dt><dd id="bark-server">加载中</dd>
      <dt>订阅位置</dt><dd id="pos">加载中</dd>
      <dt>通知规则</dt><dd id="min">加载中</dd>
      <dt>更新时间</dt><dd id="updated">加载中</dd>
    </dl>
    <div class="notify-card">
      <div class="threshold-row">
        <div>
          <div class="setting-title">当前通知规则</div>
          <div class="setting-note">这里只展示订阅页保存的规则。需要修改位置或通知级别时，请返回订阅页更新。</div>
        </div>
        <a class="btn secondary" href="/">修改配置</a>
      </div>
      <div class="rule-rows" id="manage-bands"><div class="history-empty">正在加载通知规则...</div></div>
    </div>
    <div class="actions single">
      <a class="btn secondary" href="/">返回订阅页修改配置</a>
    </div>
    <div class="status" id="status"></div>
  </section>
  <section class="panel">
    <div class="history-head">
      <div>
        <h2>历史真实数据测试</h2>
        <div class="muted">默认显示本地缓存中的最新 5 条；Wolfx 历史接口不支持任意历史搜索，因此这里不提供搜索框。</div>
      </div>
    </div>
    <div class="history-tools">
      <select id="history-source" aria-label="数据来源">
        <option value="all">全部来源</option>
        <option value="cenc">CENC</option>
        <option value="jma">JMA</option>
      </select>
      <select id="history-min-mag" aria-label="最低震级">
        <option value="">全部震级</option>
        <option value="3">M3+</option>
        <option value="4">M4+</option>
        <option value="5">M5+</option>
        <option value="6">M6+</option>
        <option value="7">M7+</option>
      </select>
      <button class="secondary" id="history-refresh" type="button">刷新</button>
    </div>
    <div class="history-list" id="history-list">
      <div class="history-empty">正在加载历史地震数据...</div>
    </div>
    <div class="pager" id="history-pager">
      <div class="page-info" id="history-page-info">第 1 页</div>
      <button class="secondary" id="history-prev" type="button">上一页</button>
      <button class="secondary" id="history-next" type="button">下一页</button>
    </div>
    <div class="history-note">历史数据用于测试复现，不代表实时预警正在发生；通知内容会保留所选历史记录的真实发震时间。</div>
  </section>
  <section class="panel">
    <div class="history-head">
      <div>
        <h2>历史大地震复现</h2>
        <div class="muted">独立保存汶川、唐山等历史大地震参数，用于复现测试订阅地预估烈度和推送效果。</div>
      </div>
    </div>
    <div class="major-list" id="major-list">
      <div class="history-empty">正在加载历史大地震...</div>
    </div>
  </section>
</main>
<script>
  const barkID={{.BarkIDJSON}}, api=location.origin, statusEl=document.getElementById("status"), historyList=document.getElementById("history-list"), majorList=document.getElementById("major-list"), historySource=document.getElementById("history-source"), historyMinMag=document.getElementById("history-min-mag"), historyRefresh=document.getElementById("history-refresh"), historyPrev=document.getElementById("history-prev"), historyNext=document.getElementById("history-next"), historyPageInfo=document.getElementById("history-page-info"), manageBands=document.getElementById("manage-bands"), barkEl=document.getElementById("bark"), toggleKey=document.getElementById("toggle-key"), copyKey=document.getElementById("copy-key");
  let historyPage=0, historyPageSize=5, historyHasNext=false;
  let simulationItems=[];
  let currentSub=null;
  let keyVisible=false;
  const iconEye='<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6S2 12 2 12Z"></path><circle cx="12" cy="12" r="3"></circle></svg>';
  const iconEyeOff='<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m3 3 18 18"></path><path d="M10.6 10.6A3 3 0 0 0 12 15a3 3 0 0 0 2.4-1.2"></path><path d="M9.9 5.2A11 11 0 0 1 12 5c6.5 0 10 7 10 7a17.8 17.8 0 0 1-2.6 3.6"></path><path d="M6.1 6.7C3.5 8.5 2 12 2 12s3.5 7 10 7a10.7 10.7 0 0 0 5-1.2"></path></svg>';
  const iconCopy='<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="8" y="8" width="12" height="12" rx="2"></rect><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"></path></svg>';
  const iconCheck='<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m20 6-11 11-5-5"></path></svg>';
  function mask(v){return "•".repeat(Math.max(8,Math.min(16,String(v||"").length)))}
  function show(msg,cls){statusEl.className="status show "+(cls||"");statusEl.textContent=msg}
  function escapeHTML(v){return String(v??"").replace(/[&<>"']/g,function(c){return {"&":"&amp;","<":"&lt;",">":"&gt;","\"":"&quot;","'":"&#39;"}[c]})}
  function renderLocations(locs){return locs.map(function(loc){const lat=Number(loc.latitude), lon=Number(loc.longitude);const coords=(Number.isFinite(lat)?lat.toFixed(4):"-")+", "+(Number.isFinite(lon)?lon.toFixed(4):"-");const address=String(loc.name||loc.address||"").trim();return '<div class="location-row"><div class="location-coords">'+escapeHTML(coords)+'</div><div class="location-address">'+escapeHTML(address||"未命名地点")+'</div></div>'}).join("")}
  function buttonTitle(btn){return btn.dataset.label||(btn.querySelector(".btn-title")||btn).textContent.trim()}
  function levelText(level){return level==="critical"?"critical + call":level}
  function bandLabel(level){return level==="critical"?"高烈度":level==="active"?"中等烈度":"低烈度"}
  function bandDesc(level){return level==="critical"?"强提醒":level==="active"?"主动提醒":"不打扰"}
  function bandsFromRules(rules){rules=rules||{passive_max:1,active_max:2,critical_min:3};return [{min:0,max:Number(rules.passive_max??1),level:"passive",label:"低烈度"},{min:Number(rules.passive_max??1)+1,max:Number(rules.active_max??2),level:"active",label:"中等烈度"},{min:Number(rules.critical_min??3),max:99,level:"critical",label:"高烈度"}].filter(function(b){return b.min<=b.max})}
  function currentBands(){if(currentSub&&Array.isArray(currentSub.notify_bands)&&currentSub.notify_bands.length)return currentSub.notify_bands;return bandsFromRules(currentSub&&currentSub.notify_rules)}
  function rangeText(b){return b.min+"-"+(Number(b.max)>7?"7+":b.max)}
  function simulationRule(kind){const level=kind==="large"?"critical":kind==="medium"?"active":"passive";const band=currentBands().find(function(b){return b.level===level});return band?{range:"烈度 "+rangeText(band),label:levelText(level)}:{range:"未启用",label:levelText(level)}}
  function renderBands(){const bands=currentBands();document.getElementById("min").textContent=bands.length?bands.map(function(b){return rangeText(b)+" "+levelText(b.level)}).join("；"):"未配置";manageBands.innerHTML=bands.length?bands.map(function(b){const kind=b.level==="critical"?"large":b.level==="active"?"medium":"small";return '<div class="rule-row"><div class="range-fields"><span>烈度</span><span class="range-value">'+escapeHTML(rangeText(b))+'</span></div><div class="band-choice level-'+escapeHTML(b.level)+'"><div class="band-text"><span class="band-main">'+escapeHTML(b.label||bandLabel(b.level))+'</span><span class="band-sub">'+escapeHTML(bandDesc(b.level))+" · "+escapeHTML(levelText(b.level))+'</span></div></div><button class="level-test simulate" data-kind="'+kind+'" data-label="'+escapeHTML(b.label||bandLabel(b.level))+' 测试" type="button">测试</button></div>'}).join(""):'<div class="history-empty">当前未配置任何通知规则，请返回订阅页添加。</div>';renderSimulations()}
  function renderSimulations(){simulationItems.forEach(function(item){const btn=Array.from(document.querySelectorAll(".simulate")).find(function(node){return node.dataset.kind===item.kind});if(!btn) return;const rule=simulationRule(item.kind);btn.title=item.label+"对应当前配置："+rule.range+"，通知 "+rule.label})}
  function renderBarkKey(){barkEl.textContent=keyVisible?barkID:mask(barkID);toggleKey.innerHTML=keyVisible?iconEyeOff:iconEye;toggleKey.title=keyVisible?"隐藏 Bark Key":"显示 Bark Key";toggleKey.setAttribute("aria-label",toggleKey.title)}
  async function copyText(text){try{if(navigator.clipboard&&window.isSecureContext){await navigator.clipboard.writeText(text)}else{const input=document.createElement("textarea");input.value=text;input.setAttribute("readonly","");input.style.position="fixed";input.style.left="-9999px";input.style.top="0";document.body.appendChild(input);input.focus();input.select();input.setSelectionRange(0,input.value.length);const ok=document.execCommand("copy");input.remove();if(!ok)throw new Error("copy failed")}return true}catch{return false}}
  renderBarkKey();
  copyKey.innerHTML=iconCopy;
  toggleKey.addEventListener("click",function(){keyVisible=!keyVisible;renderBarkKey()});
  copyKey.addEventListener("click",async function(){const ok=await copyText(barkID);if(ok){copyKey.classList.add("copied");copyKey.innerHTML=iconCheck;show("Bark Key 已复制。","ok");setTimeout(function(){copyKey.classList.remove("copied");copyKey.innerHTML=iconCopy},1200)}else{show("复制失败，请先点眼睛显示后手动复制。","err")}});
  async function load(){
    try{
      const res=await fetch(api+"/api/subscription/"+encodeURIComponent(barkID));
      const json=await res.json();
      if(!res.ok||!json.success) throw new Error(json.message||"加载失败");
      const s=json.data;
      currentSub=s;
      document.getElementById("bark-server").textContent=s.bark_server||"https://api.day.app";
      const locs=Array.isArray(s.locations)&&s.locations.length?s.locations:[{name:s.location_name||"",latitude:s.latitude,longitude:s.longitude}];
      document.getElementById("pos").innerHTML=renderLocations(locs);
      document.getElementById("updated").textContent=new Date(s.updated_at).toLocaleString("zh-CN",{timeZone:"Asia/Shanghai",hour12:false});
      renderBands();
      show("已加载订阅页保存的配置。","ok");
      await Promise.all([loadSimulations(),loadHistory(),loadMajorHistory()]);
    }catch(e){show(e.message||"加载失败","err")}
  }
  async function loadSimulations(){
    try{
      const res=await fetch(api+"/api/simulations/"+encodeURIComponent(barkID));
      const json=await res.json();
      if(!res.ok||!json.success) throw new Error(json.message||"模拟等级加载失败");
      simulationItems=json.data||[];
      renderSimulations();
    }catch(e){show(e.message||"模拟等级加载失败","err")}
  }
  function sourceLabel(source){
    const key=String(source||"").toLowerCase();
    return key==="major"?"历史大地震":String(source||"").toUpperCase();
  }
  function renderHistory(target, records, emptyText){
    if(!records||!records.length){
      target.innerHTML='<div class="history-empty">'+escapeHTML(emptyText||"暂无可用历史地震数据。")+'</div>';
      return;
    }
    target.innerHTML=records.slice(0,12).map(function(r){
      const title=escapeHTML(r.hypocenter||"未知震中");
      const source=escapeHTML(sourceLabel(r.source));
      const mag=Number(r.magnitude||0).toFixed(1);
      const depth=Number(r.depth_km||0).toFixed(0);
      const intensity=escapeHTML(r.max_intensity||"未知");
      const estimated=Number.isFinite(Number(r.estimated_intensity))?String(r.estimated_intensity):"未知";
      const distance=Number.isFinite(Number(r.distance_km))?Number(r.distance_km).toFixed(1)+"km":"未知";
      const time=escapeHTML(r.origin_time||"未知时间");
      const note=r.note?'<span>'+escapeHTML(r.note)+'</span>':'';
      return '<article class="history-item"><div><div class="history-title"><span class="badge">'+source+'</span> '+title+'</div><div class="history-meta"><span>'+time+'</span><span>M'+mag+'</span><span>深度 '+depth+'km</span><span>最大烈度 '+intensity+'</span><span class="estimate">订阅地预估烈度 '+escapeHTML(estimated)+'</span><span>震中距 '+escapeHTML(distance)+'</span>'+note+'</div></div><button class="secondary history-test" type="button" data-source="'+escapeHTML(r.source)+'" data-key="'+escapeHTML(r.key)+'" data-estimated-intensity="'+escapeHTML(estimated)+'">用此测试</button></article>';
    }).join("");
  }
  async function loadHistory(forceRefresh){
    try{
      const qs=new URLSearchParams({limit:String(historyPageSize),offset:String(historyPage*historyPageSize)});
      const source=historySource.value, minMag=historyMinMag.value;
      if(source&&source!=="all") qs.set("source",source);
      if(minMag) qs.set("min_magnitude",minMag);
      if(forceRefresh) qs.set("refresh","1");
      const res=await fetch(api+"/api/history/"+encodeURIComponent(barkID)+"?"+qs.toString());
      const json=await res.json();
      if(!res.ok||!json.success) throw new Error(json.message||"历史数据加载失败");
      renderHistory(historyList,json.data,"暂无可用历史地震数据。");
      historyHasNext=(json.data||[]).length===historyPageSize;
      historyPageInfo.textContent="第 "+String(historyPage+1)+" 页";
      historyPrev.disabled=historyPage<=0;
      historyNext.disabled=!historyHasNext;
    }catch(e){
      historyList.innerHTML='<div class="history-empty">'+escapeHTML(e.message||"历史数据加载失败")+'</div>';
      historyNext.disabled=true;
    }
  }
  async function loadMajorHistory(){
    try{
      const qs=new URLSearchParams({source:"major",limit:"20"});
      const res=await fetch(api+"/api/history/"+encodeURIComponent(barkID)+"?"+qs.toString());
      const json=await res.json();
      if(!res.ok||!json.success) throw new Error(json.message||"历史大地震加载失败");
      renderHistory(majorList,json.data,"暂无历史大地震数据。");
    }catch(e){
      majorList.innerHTML='<div class="history-empty">'+escapeHTML(e.message||"历史大地震加载失败")+'</div>';
    }
  }
  [historySource,historyMinMag].forEach(el=>el.addEventListener("input",()=>{
    historyPage=0;
    loadHistory(false);
  }));
  historyRefresh.addEventListener("click",()=>{historyPage=0; loadHistory(true);});
  historyPrev.addEventListener("click",()=>{if(historyPage>0){historyPage--; loadHistory(false);}});
  historyNext.addEventListener("click",()=>{if(historyHasNext){historyPage++; loadHistory(false);}});
  document.addEventListener("click",async(event)=>{
    const simulateBtn=event.target.closest(".simulate");
    if(!simulateBtn) return;
    const btn=simulateBtn;
    const kind=btn.dataset.kind||"small";
    const title=buttonTitle(btn);
    if(kind==="large"&&!confirm("确认发送 critical 测试？这会按高烈度提醒发送到当前 Bark Key。")) return;
    show("正在发送"+title+"，仅发送到当前 Bark Key...","");
    try{
      const res=await fetch(api+"/api/simulate/"+encodeURIComponent(barkID)+"?kind="+encodeURIComponent(kind),{method:"POST"});
      const json=await res.json();
      if(!res.ok||!json.success) throw new Error(json.message||"发送失败");
      show("已发送"+title+"。通知级别按当前这一档配置执行。","ok");
    }catch(e){show(e.message||"发送失败","err")}
  });
  document.addEventListener("click",async(event)=>{
    const btn=event.target.closest(".history-test");
    if(!btn) return;
    const item=btn.closest(".history-item");
    const title=item?(item.querySelector(".history-title")||item).textContent.trim():"所选历史地震";
    const estimated=Number(btn.dataset.estimatedIntensity);
    const criticalBand=currentBands().find(function(b){return b.level==="critical"&&estimated>=Number(b.min)&&estimated<=Number(b.max)});
    if(Number.isFinite(estimated)&&criticalBand&&!confirm("确认使用「"+title+"」发送高烈度历史地震测试？这会向当前 Bark Key 推送复现预警。")) return;
    show("正在用历史真实地震参数发送测试，仅发送到当前 Bark Key...","");
    try{
      const qs=new URLSearchParams({source:btn.dataset.source||"",key:btn.dataset.key||""});
      const res=await fetch(api+"/api/simulate-history/"+encodeURIComponent(barkID)+"?"+qs.toString(),{method:"POST"});
      const json=await res.json();
      if(!res.ok||!json.success) throw new Error(json.message||"发送失败");
      show("已发送历史数据测试。震源、震级、深度、烈度和发震时间均来自所选历史记录。","ok");
    }catch(e){show(e.message||"发送失败","err")}
  });
  load();
</script>
</body>
</html>`))

var alertPageTemplate = template.Must(template.New("alert").Parse(`<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover">
  <title>地震预警详情</title>
  <link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css">
  <style>
    :root{color-scheme:light dark;--bg:#0f1216;--panel:rgba(255,255,255,.72);--text:#14171a;--muted:#46515e;--line:rgba(255,255,255,.5);--red:#d92d20;--amber:#f79009;--blue:#175cd3;--soft:rgba(255,255,255,.58)}
    @media (prefers-color-scheme:dark){:root{--panel:rgba(25,28,33,.68);--text:#f5f7fa;--muted:#c2c8d0;--line:rgba(255,255,255,.2);--soft:rgba(25,28,33,.58)}}
    *{box-sizing:border-box} html,body{min-height:100%} html{scroll-padding-bottom:calc(24px + env(safe-area-inset-bottom))} body{margin:0;background:var(--bg);color:var(--text);font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;overflow:auto}
    .map-backdrop{position:fixed;inset:0;z-index:0;overflow:hidden;background:#20252b}.map-backdrop:after{content:"";position:absolute;inset:0;z-index:500;pointer-events:none;background:linear-gradient(180deg,rgba(15,18,22,0),rgba(15,18,22,.04) 48%,rgba(15,18,22,.12))}.map-backdrop .leaflet-control-container{display:none}#map{width:100%;height:100%}.leaflet-control-attribution{display:none}
    .map-backdrop.fullscreen{z-index:30}.map-wrap{height:clamp(260px,44dvh,420px);position:relative;pointer-events:none}
    .map-full-btn{position:fixed;right:12px;top:12px;z-index:650;width:44px;height:44px;border:1px solid rgba(255,255,255,.55);border-radius:8px;background:rgba(18,22,27,.76);color:#fff;display:flex;align-items:center;justify-content:center;padding:0;box-shadow:0 8px 24px rgba(0,0,0,.24);cursor:pointer}.map-full-btn svg{width:21px;height:21px;stroke:currentColor;stroke-width:2.25;fill:none;stroke-linecap:round;stroke-linejoin:round}.map-full-btn .exit-icon{display:none}.map-backdrop.fullscreen .map-full-btn{top:auto!important;right:12px!important;bottom:12px!important}.map-backdrop.fullscreen .map-full-btn .enter-icon{display:none}.map-backdrop.fullscreen .map-full-btn .exit-icon{display:block}
    main{min-height:100dvh;display:flex;flex-direction:column;position:relative;z-index:1;pointer-events:none}
    .content{width:min(760px,calc(100% - 20px));margin:0 auto;padding:10px 0 calc(18px + env(safe-area-inset-bottom));display:flex;flex-direction:column;gap:8px;position:relative;pointer-events:none}
    .hero{color:#fff;text-shadow:none;flex:0 0 auto}
    .tag{display:inline-flex;background:rgba(255,255,255,.2);border:1px solid rgba(255,255,255,.28);border-radius:999px;padding:6px 11px;font-size:13px;font-weight:800;margin-bottom:10px}
    .tag.test{background:rgba(247,144,9,.22)}h1{font-size:clamp(22px,6vw,34px);line-height:1.12;margin:0 0 6px;letter-spacing:0}.meta{font-size:13px;line-height:1.45;opacity:.88}
    .status-card{background:rgba(18,22,27,.72);border:1px solid rgba(255,255,255,.2);border-radius:12px;padding:12px;box-shadow:0 14px 32px rgba(0,0,0,.18);pointer-events:auto}
    .countdown{display:grid;grid-template-columns:1fr auto;align-items:end;gap:8px;margin:10px 0}.count-label{font-size:14px;font-weight:800;opacity:.92}.count-value{font-size:clamp(46px,15vw,78px);line-height:.88;font-weight:900;letter-spacing:0}.count-unit{font-size:20px;margin-left:4px}.arrived{font-size:clamp(32px,10vw,52px)}
    .quick{display:grid;grid-template-columns:repeat(3,1fr);gap:8px}.quick .tile{background:rgba(255,255,255,.12);border:1px solid rgba(255,255,255,.14);border-radius:8px;padding:9px}.tile .label{font-size:12px;opacity:.78;margin-bottom:4px}.tile .value{font-size:18px;font-weight:900}
    .panel{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:12px;box-shadow:0 12px 28px rgba(0,0,0,.16);pointer-events:auto}h2{font-size:17px;margin:0 0 12px}
    dl{display:grid;grid-template-columns:112px 1fr;gap:10px 12px;margin:0}dt{color:var(--muted)}dd{margin:0;font-weight:700;word-break:break-word}.actions{display:grid;grid-template-columns:1fr 1fr;gap:10px}
    .btn{min-height:52px;border-radius:8px;display:flex;align-items:center;justify-content:center;text-align:center;text-decoration:none;font-weight:900;border:0;font-size:15px;font-family:inherit;padding:0 12px;line-height:1.2;cursor:pointer}.primary{background:var(--red);color:#fff}.secondary{background:var(--soft);color:var(--text);border:1px solid var(--line)}.danger{background:rgba(217,45,32,.12);color:var(--red);border:1px solid rgba(217,45,32,.36)}.btn:disabled{opacity:.56;cursor:not-allowed}.action-status{grid-column:1/-1;margin:0;color:var(--muted);font-size:13px;line-height:1.45;min-height:18px}.action-status.ok{color:#067647}.action-status.err{color:var(--red)}
    details{grid-column:1/-1;overflow:visible}details summary{min-height:52px;border-radius:8px;cursor:pointer;font-weight:900;list-style:none;display:grid;grid-template-columns:1fr 24px;align-items:center;gap:8px;padding:0 14px;line-height:1;background:var(--soft);color:var(--text);border:1px solid var(--line)}details summary:after{content:"";width:9px;height:9px;border-right:2px solid var(--muted);border-bottom:2px solid var(--muted);transform:rotate(45deg);justify-self:center;transition:transform .18s ease;margin-top:-4px}details[open] summary:after{transform:rotate(225deg);margin-top:4px}details summary::-webkit-details-marker{display:none}.detail-body{margin-top:10px;border-top:1px solid var(--line);padding-top:12px}
    @media(max-width:560px){.map-wrap{height:clamp(220px,38dvh,300px)}.content{width:min(100% - 16px,760px);padding-top:8px;padding-bottom:calc(28px + env(safe-area-inset-bottom))}.quick{grid-template-columns:repeat(3,1fr);gap:6px}.quick .tile{padding:8px 7px}.actions{grid-template-columns:1fr;gap:8px}dl{grid-template-columns:86px 1fr;gap:9px 10px}.panel{padding:10px}.tile .label{font-size:11px}.tile .value{font-size:16px}.btn,details summary{min-height:48px}.countdown{align-items:center}.count-value{font-size:clamp(40px,13vw,60px)}}
  </style>
</head>
<body>
<div class="map-backdrop" id="map-layer">
  <div id="map"></div>
  <button class="map-full-btn" id="map-full-btn" type="button" aria-label="全屏显示地图" title="全屏显示地图"><svg class="enter-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M8 3H5a2 2 0 0 0-2 2v3"/><path d="M16 3h3a2 2 0 0 1 2 2v3"/><path d="M8 21H5a2 2 0 0 1-2-2v-3"/><path d="M16 21h3a2 2 0 0 0 2-2v-3"/></svg><svg class="exit-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M8 3v3a2 2 0 0 1-2 2H3"/><path d="M16 3v3a2 2 0 0 0 2 2h3"/><path d="M8 21v-3a2 2 0 0 0-2-2H3"/><path d="M16 21v-3a2 2 0 0 1 2-2h3"/></svg></button>
</div>
<main>
  <div class="map-wrap" aria-hidden="true"></div>
  <div class="content">
    <section class="hero">
    <div class="status-card">
      <div class="tag {{if .IsTest}}test{{end}}">{{if .IsTest}}模拟测试{{else}}地震预警{{end}}</div>
      <h1>{{.Region}} {{.Magnitude}}</h1>
      <div class="meta">第 {{.ReportNum}} 报 · 发震 {{.OriginTime}}</div>
      <div class="countdown">
        <div>
          <div class="count-label">S 波到达订阅地</div>
          <div id="p-count" class="meta">P 波 {{if gt .SecondsToP 0}}+{{.SecondsToP}}秒{{else}}已到达{{end}}</div>
        </div>
        <div id="s-count" class="count-value">{{if gt .SecondsToS 0}}{{.SecondsToS}}<span class="count-unit">秒</span>{{else}}<span class="arrived">已到达</span>{{end}}</div>
      </div>
      <div class="quick">
        <div class="tile"><div class="label">预计烈度</div><div class="value">{{.EstimatedIntensity}}</div></div>
        <div class="tile"><div class="label">震中距</div><div class="value">{{.Distance}}</div></div>
        <div class="tile"><div class="label">震级</div><div class="value">{{.Magnitude}}</div></div>
      </div>
    </div>
    </section>
    <section class="panel">
    <div class="actions"><button class="btn primary" id="wechat-btn" type="button" data-url="{{.WeChatURL}}">中国地震台网</button><a class="btn secondary" href="{{.MapURL}}">苹果地图路线</a><a class="btn secondary" href="{{.ManageURL}}">测试页</a><button class="btn danger" id="alert-unsubscribe" type="button">取消订阅</button><p class="action-status" id="alert-action-status" aria-live="polite"></p>
    <details id="detail-panel">
      <summary>详细信息</summary>
      <div class="detail-body">
        <dl>
          <dt>事件 ID</dt><dd>{{.EventID}}</dd><dt>来源</dt><dd>{{.Source}}</dd><dt>震中</dt><dd>{{.Epicenter}}</dd><dt>订阅位置</dt><dd>{{.SubscriberLocation}}</dd><dt>震源深度</dt><dd>{{.Depth}}</dd><dt>最大烈度</dt><dd>{{.MaxIntensity}}</dd><dt>P 波到达</dt><dd>{{.PArrival}}</dd><dt>S 波到达</dt><dd>{{.SArrival}}</dd><dt>震源距</dt><dd>{{.Hypocentral}}</dd><dt>页面生成</dt><dd>{{.CreatedAt}}</dd>
        </dl>
      </div>
    </details>
    </div>
    </section>
  </div>
</main>
<script src="https://unpkg.com/leaflet@1.9.4/dist/leaflet.js"></script>
<script>
  const alertBarkID={{.BarkIDJSON}}, api=location.origin;
  const epicenter=[{{.MapEpicenterLat}},{{.MapEpicenterLon}}], subscriber=[{{.MapSubscriberLat}},{{.MapSubscriberLon}}];
  const tileURL="https://webrd0{s}.is.autonavi.com/appmaptile?lang=zh_cn&size=1&scale=1&style=7&x={x}&y={y}&z={z}";
  function drawAlertMap(mapInstance){
    L.tileLayer(tileURL,{maxZoom:18,subdomains:"1234"}).addTo(mapInstance);
    const route=L.polyline([subscriber,epicenter],{color:"#f04438",weight:4,opacity:.9,dashArray:"8 8"}).addTo(mapInstance);
    L.circleMarker(epicenter,{radius:9,color:"#fff",weight:2,fillColor:"#f04438",fillOpacity:1}).addTo(mapInstance);
    L.circleMarker(subscriber,{radius:8,color:"#fff",weight:2,fillColor:"#175cd3",fillOpacity:1}).addTo(mapInstance);
    return route;
  }
  const map=L.map("map",{zoomControl:false,attributionControl:false,dragging:true,scrollWheelZoom:true,doubleClickZoom:true,boxZoom:true,keyboard:true,tap:true});
  const line=drawAlertMap(map);
  const clearMapArea=document.querySelector(".map-wrap"), statusCard=document.querySelector(".status-card");
  const mapLayer=document.getElementById("map-layer"), fullBtn=document.getElementById("map-full-btn");
  function focusAlertBounds(){
    const active=mapLayer.classList.contains("fullscreen");
    const clearHeight=clearMapArea.getBoundingClientRect().height||Math.round(window.innerHeight*.4);
    const bottomPadding=active?72:Math.max(96,Math.round(window.innerHeight-clearHeight+28));
    map.fitBounds(line.getBounds(),{
      paddingTopLeft:[54,active?72:44],
      paddingBottomRight:[54,bottomPadding],
      maxZoom:active?11:9,
      animate:false
    });
  }
  function syncMapButton(){
    if(mapLayer.classList.contains("fullscreen")){
      fullBtn.style.top="";
      fullBtn.style.right="12px";
      fullBtn.style.bottom="12px";
      return;
    }
    const rect=statusCard.getBoundingClientRect();
    const top=Math.max(12,Math.round(rect.top-52));
    const right=Math.max(8,Math.round(window.innerWidth-rect.right));
    fullBtn.style.top=String(top)+"px";
    fullBtn.style.right=String(right)+"px";
    fullBtn.style.bottom="auto";
  }
  focusAlertBounds();
  syncMapButton();
  function syncFullscreenButton(){
    const active=mapLayer.classList.contains("fullscreen");
    fullBtn.setAttribute("aria-label",active?"退出全屏地图":"全屏显示地图");
    fullBtn.setAttribute("title",active?"退出全屏地图":"全屏显示地图");
    syncMapButton();
    setTimeout(function(){map.invalidateSize(); focusAlertBounds(); syncMapButton();},80);
  }
  fullBtn.addEventListener("click",async function(event){
    event.stopPropagation();
    const active=mapLayer.classList.contains("fullscreen");
    if(active){
      mapLayer.classList.remove("fullscreen");
      if(document.fullscreenElement&&document.exitFullscreen){try{await document.exitFullscreen();}catch(e){}}
    }else{
      mapLayer.classList.add("fullscreen");
      if(mapLayer.requestFullscreen){try{await mapLayer.requestFullscreen();}catch(e){}}
    }
    syncFullscreenButton();
  });
  document.addEventListener("fullscreenchange",function(){
    if(!document.fullscreenElement&&mapLayer.classList.contains("fullscreen")){
      mapLayer.classList.remove("fullscreen");
      syncFullscreenButton();
    }
  });
  window.addEventListener("resize",function(){setTimeout(function(){map.invalidateSize(); focusAlertBounds(); syncMapButton();},80);});
  window.addEventListener("scroll",syncMapButton,{passive:true});
  const sArrival={{.SArrivalUnix}}, pArrival={{.PArrivalUnix}}, sEl=document.getElementById("s-count"), pEl=document.getElementById("p-count");
  document.getElementById("wechat-btn").addEventListener("click",function(){
    const target=this.dataset.url||"weixin://";
    window.location.assign(target);
    setTimeout(function(){window.location.href=target;},120);
  });
  const unsubscribeBtn=document.getElementById("alert-unsubscribe"), actionStatus=document.getElementById("alert-action-status");
  function showActionStatus(message,type){
    actionStatus.textContent=message||"";
    actionStatus.className="action-status"+(type?" "+type:"");
  }
  unsubscribeBtn.addEventListener("click",async function(){
    if(!confirm("确认取消当前 Bark Key 的地震预警订阅？取消后将不再接收预警。")) return;
    unsubscribeBtn.disabled=true;
    showActionStatus("正在取消订阅...","");
    try{
      const res=await fetch(api+"/api/unsubscribe/"+encodeURIComponent(alertBarkID),{method:"DELETE"});
      const json=await res.json().catch(function(){return {}});
      if(!res.ok||!json.success) throw new Error(json.message||"取消订阅失败");
      showActionStatus("已取消订阅。","ok");
    }catch(e){
      showActionStatus(e.message||"取消订阅失败","err");
      unsubscribeBtn.disabled=false;
    }
  });
  const detailPanel=document.getElementById("detail-panel");
  detailPanel.addEventListener("toggle",function(){
    document.body.classList.toggle("details-open", detailPanel.open);
    if(detailPanel.open){
      setTimeout(function(){detailPanel.scrollIntoView({block:"start",behavior:"smooth"});},30);
    }
  });
  function tick(){
    const now=Date.now(), s=Math.ceil((sArrival-now)/1000), p=Math.ceil((pArrival-now)/1000);
    sEl.innerHTML=s>0?String(s)+'<span class="count-unit">秒</span>':'<span class="arrived">已到达</span>';
    pEl.textContent=p>0?'+'+String(p)+'秒':'已到达';
  }
  tick(); setInterval(tick,250);
</script>
</body>
</html>`))

func logRequest(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		next.ServeHTTP(w, r)
		if strings.HasPrefix(r.URL.Path, "/api/") || r.URL.Path == "/health" {
			log.Printf("%s %s %s", r.Method, r.URL.Path, time.Since(start))
		}
	})
}

func run(ctx context.Context, cfg Config, notifier *Notifier, deduper *Deduper, store *Store, alertCache *AlertCache) {
	minDelay := time.Duration(cfg.Wolfx.ReconnectMinSecond) * time.Second
	maxDelay := time.Duration(cfg.Wolfx.ReconnectMaxSecond) * time.Second
	delay := minDelay

	for {
		err := listenOnce(ctx, cfg, notifier, deduper, store, alertCache)
		if err != nil {
			log.Printf("websocket disconnected: %v", err)
		}
		log.Printf("reconnect in %s", delay)
		time.Sleep(delay)
		delay *= 2
		if delay > maxDelay {
			delay = maxDelay
		}
	}
}

func listenOnce(ctx context.Context, cfg Config, notifier *Notifier, deduper *Deduper, store *Store, alertCache *AlertCache) error {
	dialer := websocket.Dialer{
		Proxy:            http.ProxyFromEnvironment,
		HandshakeTimeout: 10 * time.Second,
		NetDialContext: (&net.Dialer{
			Timeout:   5 * time.Second,
			KeepAlive: 30 * time.Second,
		}).DialContext,
		TLSClientConfig: &tls.Config{MinVersion: tls.VersionTLS12},
	}

	conn, _, err := dialer.DialContext(ctx, cfg.Wolfx.WebSocketURL, http.Header{
		"User-Agent": []string{"eew-bark/1.0"},
	})
	if err != nil {
		return err
	}
	defer conn.Close()
	log.Printf("connected: %s", cfg.Wolfx.WebSocketURL)

	conn.SetReadLimit(1 << 20)
	_ = conn.SetReadDeadline(time.Now().Add(90 * time.Second))
	conn.SetPongHandler(func(string) error {
		return conn.SetReadDeadline(time.Now().Add(90 * time.Second))
	})

	done := make(chan struct{})
	defer close(done)
	go func() {
		ticker := time.NewTicker(30 * time.Second)
		defer ticker.Stop()
		for {
			select {
			case <-done:
				return
			case <-ticker.C:
				_ = conn.SetWriteDeadline(time.Now().Add(5 * time.Second))
				if err := conn.WriteMessage(websocket.PingMessage, nil); err != nil {
					return
				}
			}
		}
	}()

	for {
		msgType, payload, err := conn.ReadMessage()
		if err != nil {
			return err
		}
		_ = conn.SetReadDeadline(time.Now().Add(90 * time.Second))
		if msgType != websocket.TextMessage && msgType != websocket.BinaryMessage {
			continue
		}
		handlePayload(ctx, cfg, notifier, deduper, store, alertCache, payload)
	}
}

func handlePayload(ctx context.Context, cfg Config, notifier *Notifier, deduper *Deduper, store *Store, alertCache *AlertCache, payload []byte) {
	event, ok, err := parseEvent(payload)
	if err != nil {
		log.Printf("parse skipped: %v raw=%s", err, compact(payload, 512))
		return
	}
	if !ok {
		return
	}

	if cfg.Alert.IgnoreTraining && event.Training {
		log.Printf("ignored training event id=%s type=%s", event.EventID, event.Type)
		return
	}
	if cfg.Alert.IgnoreCancel && event.Cancel {
		log.Printf("ignored cancel event id=%s type=%s", event.EventID, event.Type)
		return
	}
	if !cfg.Alert.PushUpdates && deduper.Seen(event.Key(), event.ReportNum, 999999) {
		return
	}
	if cfg.Alert.PushUpdates && deduper.Seen(event.Key(), event.ReportNum, cfg.Alert.UpdateMinReportGap) {
		return
	}

	now := time.Now()
	if event.OriginTime.IsZero() {
		log.Printf("event id=%s type=%s has no origin time, using receive time for ETA", event.EventID, event.Type)
	} else if now.Sub(event.OriginTime) > time.Duration(cfg.Alert.StaleOriginSecond)*time.Second {
		log.Printf("ignored stale event id=%s type=%s origin=%s", event.EventID, event.Type, event.OriginTime.Format(time.RFC3339))
		return
	}

	receivedAt := time.Now()
	log.Printf("event received id=%s report=%d type=%s received_at=%s origin=%s subscriptions=%d",
		event.EventID, event.ReportNum, event.Type, formatBeijing(receivedAt, time.RFC3339Nano), formatBeijing(event.OriginTime, time.RFC3339), store.Count())
	pushed, skipped := dispatchEvent(ctx, cfg, notifier, store, alertCache, event, receivedAt)
	log.Printf("event id=%s report=%d type=%s fanout pushed=%d skipped=%d total=%d",
		event.EventID, event.ReportNum, event.Type, pushed, skipped, store.Count())
}

type fanoutTarget struct {
	Sub      Subscription
	Decision Decision
	Title    string
	Subtitle string
	Body     string
	Params   map[string]string
	Options  PushOptions
	Level    string
	Priority int
}

type fanoutResult struct {
	Target  fanoutTarget
	Pushed  bool
	Skipped bool
	Err     error
	Reason  string
	Elapsed time.Duration
	Until   time.Time
}

type deliveryAuditRecord struct {
	EventID            string  `json:"event_id"`
	ReportNum          int     `json:"report_num"`
	Type               string  `json:"type"`
	OriginTime         string  `json:"origin_time"`
	ReceivedAt         string  `json:"received_at"`
	FanoutStartedAt    string  `json:"fanout_started_at"`
	RecordedAt         string  `json:"recorded_at"`
	Status             string  `json:"status"`
	Reason             string  `json:"reason,omitempty"`
	BarkMasked         string  `json:"bark_masked"`
	BarkHash           string  `json:"bark_hash"`
	BarkServer         string  `json:"bark_server"`
	NotifyLevel        string  `json:"notify_level,omitempty"`
	EstimatedIntensity int     `json:"estimated_intensity"`
	DistanceKM         float64 `json:"distance_km"`
	HypocentralKM      float64 `json:"hypocentral_km"`
	SecondsToS         int     `json:"seconds_to_s"`
	ElapsedMS          int64   `json:"elapsed_ms,omitempty"`
	Error              string  `json:"error,omitempty"`
	LocationName       string  `json:"location_name,omitempty"`
	Latitude           float64 `json:"latitude"`
	Longitude          float64 `json:"longitude"`
}

type deliveryAuditSummary struct {
	EventID               string         `json:"event_id"`
	ReportNum             int            `json:"report_num"`
	Type                  string         `json:"type"`
	OriginTime            string         `json:"origin_time"`
	ReceivedAt            string         `json:"received_at"`
	FanoutStartedAt       string         `json:"fanout_started_at"`
	FanoutDoneAt          string         `json:"fanout_done_at"`
	DurationMS            int64          `json:"duration_ms"`
	TotalSubscriptions    int            `json:"total_subscriptions"`
	Queued                int            `json:"queued"`
	Pushed                int            `json:"pushed"`
	Filtered              int            `json:"filtered"`
	Skipped               int            `json:"skipped"`
	Failed                int            `json:"failed"`
	Official              int            `json:"official"`
	SelfHosted            int            `json:"self_hosted"`
	OfficialConcurrency   int            `json:"official_concurrency"`
	SelfHostedConcurrency int            `json:"self_hosted_concurrency"`
	StatusCounts          map[string]int `json:"status_counts"`
	ReasonCounts          map[string]int `json:"reason_counts"`
	ServerCounts          map[string]int `json:"server_counts"`
	NotifyLevelCounts     map[string]int `json:"notify_level_counts"`
	IntensityCounts       map[int]int    `json:"intensity_counts"`
	ElapsedP50MS          int64          `json:"elapsed_p50_ms,omitempty"`
	ElapsedP90MS          int64          `json:"elapsed_p90_ms,omitempty"`
	ElapsedP99MS          int64          `json:"elapsed_p99_ms,omitempty"`
	DetailPath            string         `json:"detail_path"`
}

func dispatchEvent(ctx context.Context, cfg Config, notifier *Notifier, store *Store, alertCache *AlertCache, event Event, receivedAt time.Time) (int, int) {
	subs := store.List()
	if len(subs) == 0 {
		log.Printf("event id=%s type=%s skipped: no subscribers", event.EventID, event.Type)
		return 0, 0
	}

	startedAt := time.Now()
	var skipped int
	targets := make([]fanoutTarget, 0, len(subs))
	auditEnabled := shouldAuditEvent(event)
	auditRecords := make([]deliveryAuditRecord, 0, len(subs))
	for _, sub := range subs {
		selectedSub, decision := nearestSubscriptionForEvent(cfg, sub, event)
		if cfg.Alert.MaxDistanceKM > 0 && decision.DistanceKM > cfg.Alert.MaxDistanceKM && !bypassDeliveryFilters(event) {
			skipped++
			if auditEnabled {
				auditRecords = append(auditRecords, deliveryAuditRecordForTarget(cfg, event, selectedSub, decision, receivedAt, startedAt, "filtered", "max_distance", "", 0, nil))
			}
			continue
		}
		level := notifyLevelForIntensity(selectedSub, decision.EstimatedIntensity)
		if level == "" && !bypassDeliveryFilters(event) {
			skipped++
			if auditEnabled {
				auditRecords = append(auditRecords, deliveryAuditRecordForTarget(cfg, event, selectedSub, decision, receivedAt, startedAt, "filtered", "notify_band", "", 0, nil))
			}
			continue
		}

		title, subtitle, body := formatAlert(event, decision, selectedSub)
		mapURL := appleMapsDirectionsURL(selectedSub, event)
		params := map[string]string{
			"url": clickURL(cfg, alertCache, event, decision, selectedSub, mapURL),
		}
		options := pushOptions(cfg, selectedSub, decision)
		level = fallback(options.Level, cfg.Bark.Level)
		targets = append(targets, fanoutTarget{
			Sub:      selectedSub,
			Decision: decision,
			Title:    title,
			Subtitle: subtitle,
			Body:     body,
			Params:   params,
			Options:  options,
			Level:    level,
			Priority: notifyPriority(level),
		})
	}
	sort.SliceStable(targets, func(i, j int) bool {
		a, b := targets[i], targets[j]
		if a.Priority != b.Priority {
			return a.Priority < b.Priority
		}
		if a.Decision.SecondsToS != b.Decision.SecondsToS {
			return a.Decision.SecondsToS < b.Decision.SecondsToS
		}
		if a.Decision.EstimatedIntensity != b.Decision.EstimatedIntensity {
			return a.Decision.EstimatedIntensity > b.Decision.EstimatedIntensity
		}
		return a.Decision.DistanceKM < b.Decision.DistanceKM
	})
	officialCount, selfHostedCount := fanoutServerCounts(cfg, targets)
	log.Printf("event fanout start id=%s report=%d type=%s queued=%d filtered=%d official=%d self_hosted=%d official_concurrency=%d self_hosted_concurrency=%d started_at=%s",
		event.EventID, event.ReportNum, event.Type, len(targets), skipped, officialCount, selfHostedCount,
		officialFanoutConcurrency(cfg, officialCount), selfHostedFanoutConcurrency(cfg, selfHostedCount), formatBeijing(startedAt, time.RFC3339Nano))

	pushed, guarded, failed, results := sendFanout(ctx, cfg, notifier, event, targets)
	skipped += guarded
	log.Printf("event fanout done id=%s report=%d type=%s queued=%d pushed=%d skipped=%d failed=%d duration=%s done_at=%s",
		event.EventID, event.ReportNum, event.Type, len(targets), pushed, skipped, failed, time.Since(startedAt), formatBeijing(time.Now(), time.RFC3339Nano))
	if auditEnabled {
		for _, result := range results {
			status := "failed"
			reason := ""
			if result.Pushed {
				status = "pushed"
			} else if result.Skipped {
				status = "skipped"
				reason = result.Reason
			}
			auditRecords = append(auditRecords, deliveryAuditRecordForTarget(cfg, event, result.Target.Sub, result.Target.Decision, receivedAt, startedAt, status, reason, result.Target.Level, result.Elapsed, result.Err))
		}
		if err := writeDeliveryAudit(cfg, event, receivedAt, startedAt, time.Now(), len(subs), officialCount, selfHostedCount, officialFanoutConcurrency(cfg, officialCount), selfHostedFanoutConcurrency(cfg, selfHostedCount), auditRecords); err != nil {
			log.Printf("write delivery audit failed id=%s report=%d type=%s: %v", event.EventID, event.ReportNum, event.Type, err)
		}
	}
	return pushed, skipped
}

func sendFanout(ctx context.Context, cfg Config, notifier *Notifier, event Event, targets []fanoutTarget) (int, int, int, []fanoutResult) {
	if len(targets) == 0 {
		return 0, 0, 0, nil
	}
	officialTargets, selfHostedTargets := splitFanoutTargets(cfg, targets)
	results := make(chan fanoutResult)
	var wg sync.WaitGroup
	if len(officialTargets) > 0 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			sendFanoutGroup(ctx, notifier, event, officialTargets, officialFanoutConcurrency(cfg, len(officialTargets)), results)
		}()
	}
	if len(selfHostedTargets) > 0 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			sendFanoutGroup(ctx, notifier, event, selfHostedTargets, selfHostedFanoutConcurrency(cfg, len(selfHostedTargets)), results)
		}()
	}
	go func() {
		wg.Wait()
		close(results)
	}()

	var pushed, skipped, failed int
	collected := make([]fanoutResult, 0, len(targets))
	for result := range results {
		collected = append(collected, result)
		target := result.Target
		switch {
		case result.Pushed:
			pushed++
			log.Printf("pushed id=%s report=%d bark=%s server=%s type=%s level=%s M%.1f dist=%.1fkm intensity=%d eta_s=%ds elapsed=%s",
				event.EventID, event.ReportNum, maskKey(target.Sub.BarkID), normalizeBarkServer(target.Sub.BarkServer, cfg), event.Type, target.Level, event.Magnitude, target.Decision.DistanceKM,
				target.Decision.EstimatedIntensity, target.Decision.SecondsToS, result.Elapsed)
		case result.Skipped:
			skipped++
			log.Printf("bark send skipped id=%s bark=%s server=%s type=%s reason=%s until=%s",
				event.EventID, maskKey(target.Sub.BarkID), normalizeBarkServer(target.Sub.BarkServer, cfg), event.Type, result.Reason, formatBeijing(result.Until, time.RFC3339))
		default:
			failed++
			log.Printf("bark send failed id=%s bark=%s server=%s type=%s level=%s: %v",
				event.EventID, maskKey(target.Sub.BarkID), normalizeBarkServer(target.Sub.BarkServer, cfg), event.Type, target.Level, result.Err)
		}
	}
	return pushed, skipped, failed, collected
}

func shouldAuditEvent(event Event) bool {
	return !isTestEvent(event) && !isHistoryTestEvent(event)
}

func deliveryAuditRecordForTarget(cfg Config, event Event, sub Subscription, decision Decision, receivedAt, startedAt time.Time, status, reason, level string, elapsed time.Duration, err error) deliveryAuditRecord {
	server := normalizeBarkServer(sub.BarkServer, cfg)
	record := deliveryAuditRecord{
		EventID:            event.EventID,
		ReportNum:          event.ReportNum,
		Type:               event.Type,
		OriginTime:         formatBeijing(event.OriginTime, time.RFC3339),
		ReceivedAt:         formatBeijing(receivedAt, time.RFC3339Nano),
		FanoutStartedAt:    formatBeijing(startedAt, time.RFC3339Nano),
		RecordedAt:         formatBeijing(time.Now(), time.RFC3339Nano),
		Status:             status,
		Reason:             reason,
		BarkMasked:         maskKey(sub.BarkID),
		BarkHash:           hashKey(sub.BarkID),
		BarkServer:         server,
		NotifyLevel:        level,
		EstimatedIntensity: decision.EstimatedIntensity,
		DistanceKM:         round1(decision.DistanceKM),
		HypocentralKM:      round1(decision.HypocentralKM),
		SecondsToS:         decision.SecondsToS,
		LocationName:       sub.LocationName,
		Latitude:           math.Round(sub.Latitude*10000) / 10000,
		Longitude:          math.Round(sub.Longitude*10000) / 10000,
	}
	if elapsed > 0 {
		record.ElapsedMS = elapsed.Milliseconds()
	}
	if err != nil {
		record.Error = err.Error()
		if record.Reason == "" {
			record.Reason = "send_error"
		}
	}
	return record
}

func writeDeliveryAudit(cfg Config, event Event, receivedAt, startedAt, doneAt time.Time, totalSubs, officialCount, selfHostedCount, officialConcurrency, selfHostedConcurrency int, records []deliveryAuditRecord) error {
	if len(records) == 0 {
		return nil
	}
	auditPath := cfg.Server.AuditPath
	if auditPath == "" {
		auditPath = filepath.Join(filepath.Dir(cfg.Server.DataPath), "audit")
	}
	if err := os.MkdirAll(auditPath, 0o755); err != nil {
		return err
	}
	base := auditFileBase(event)
	detailPath := filepath.Join(auditPath, base+".jsonl")
	summaryPath := filepath.Join(auditPath, base+".summary.json")

	detail, err := os.Create(detailPath)
	if err != nil {
		return err
	}
	enc := json.NewEncoder(detail)
	for _, record := range records {
		if err := enc.Encode(record); err != nil {
			_ = detail.Close()
			return err
		}
	}
	if err := detail.Close(); err != nil {
		return err
	}

	summary := buildDeliveryAuditSummary(event, receivedAt, startedAt, doneAt, totalSubs, officialCount, selfHostedCount, officialConcurrency, selfHostedConcurrency, records, detailPath)
	data, err := json.MarshalIndent(summary, "", "  ")
	if err != nil {
		return err
	}
	if err := os.WriteFile(summaryPath, append(data, '\n'), 0o644); err != nil {
		return err
	}
	log.Printf("delivery audit written id=%s report=%d type=%s detail=%s summary=%s", event.EventID, event.ReportNum, event.Type, detailPath, summaryPath)
	return nil
}

func buildDeliveryAuditSummary(event Event, receivedAt, startedAt, doneAt time.Time, totalSubs, officialCount, selfHostedCount, officialConcurrency, selfHostedConcurrency int, records []deliveryAuditRecord, detailPath string) deliveryAuditSummary {
	statusCounts := map[string]int{}
	reasonCounts := map[string]int{}
	serverCounts := map[string]int{}
	levelCounts := map[string]int{}
	intensityCounts := map[int]int{}
	var elapsed []int64
	for _, record := range records {
		statusCounts[record.Status]++
		if record.Reason != "" {
			reasonCounts[record.Reason]++
		}
		if record.BarkServer != "" {
			serverCounts[record.BarkServer]++
		}
		if record.NotifyLevel != "" {
			levelCounts[record.NotifyLevel]++
		}
		intensityCounts[record.EstimatedIntensity]++
		if record.ElapsedMS > 0 {
			elapsed = append(elapsed, record.ElapsedMS)
		}
	}
	summary := deliveryAuditSummary{
		EventID:               event.EventID,
		ReportNum:             event.ReportNum,
		Type:                  event.Type,
		OriginTime:            formatBeijing(event.OriginTime, time.RFC3339),
		ReceivedAt:            formatBeijing(receivedAt, time.RFC3339Nano),
		FanoutStartedAt:       formatBeijing(startedAt, time.RFC3339Nano),
		FanoutDoneAt:          formatBeijing(doneAt, time.RFC3339Nano),
		DurationMS:            doneAt.Sub(startedAt).Milliseconds(),
		TotalSubscriptions:    totalSubs,
		Queued:                statusCounts["pushed"] + statusCounts["failed"] + statusCounts["skipped"],
		Pushed:                statusCounts["pushed"],
		Filtered:              statusCounts["filtered"],
		Skipped:               statusCounts["skipped"],
		Failed:                statusCounts["failed"],
		Official:              officialCount,
		SelfHosted:            selfHostedCount,
		OfficialConcurrency:   officialConcurrency,
		SelfHostedConcurrency: selfHostedConcurrency,
		StatusCounts:          statusCounts,
		ReasonCounts:          reasonCounts,
		ServerCounts:          serverCounts,
		NotifyLevelCounts:     levelCounts,
		IntensityCounts:       intensityCounts,
		DetailPath:            detailPath,
	}
	if len(elapsed) > 0 {
		sort.Slice(elapsed, func(i, j int) bool { return elapsed[i] < elapsed[j] })
		summary.ElapsedP50MS = percentileInt64(elapsed, 50)
		summary.ElapsedP90MS = percentileInt64(elapsed, 90)
		summary.ElapsedP99MS = percentileInt64(elapsed, 99)
	}
	return summary
}

func percentileInt64(sorted []int64, p int) int64 {
	if len(sorted) == 0 {
		return 0
	}
	if p <= 0 {
		return sorted[0]
	}
	if p >= 100 {
		return sorted[len(sorted)-1]
	}
	idx := int(math.Ceil(float64(p)/100*float64(len(sorted)))) - 1
	if idx < 0 {
		idx = 0
	}
	if idx >= len(sorted) {
		idx = len(sorted) - 1
	}
	return sorted[idx]
}

func auditFileBase(event Event) string {
	id := sanitizeFilePart(event.EventID)
	if id == "" {
		id = "unknown"
	}
	return fmt.Sprintf("%s-r%d-%s", id, event.ReportNum, sanitizeFilePart(event.Type))
}

func sanitizeFilePart(value string) string {
	value = strings.TrimSpace(value)
	var b strings.Builder
	for _, r := range value {
		if r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '-' || r == '_' || r == '.' {
			b.WriteRune(r)
		} else {
			b.WriteByte('_')
		}
	}
	return strings.Trim(b.String(), "._-")
}

func hashKey(value string) string {
	sum := sha256.Sum256([]byte(strings.TrimSpace(value)))
	return hex.EncodeToString(sum[:])
}

func sendFanoutGroup(ctx context.Context, notifier *Notifier, event Event, targets []fanoutTarget, concurrency int, out chan<- fanoutResult) {
	if len(targets) == 0 {
		return
	}
	if concurrency <= 0 {
		concurrency = 1
	}
	if concurrency > len(targets) {
		concurrency = len(targets)
	}
	jobs := make(chan fanoutTarget)
	var wg sync.WaitGroup
	for i := 0; i < concurrency; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for target := range jobs {
				out <- sendFanoutTarget(ctx, notifier, event, target)
			}
		}()
	}
	go func() {
		defer close(jobs)
		for _, target := range targets {
			select {
			case <-ctx.Done():
				return
			case jobs <- target:
			}
		}
	}()
	wg.Wait()
}

func sendFanoutTarget(ctx context.Context, notifier *Notifier, event Event, target fanoutTarget) fanoutResult {
	now := time.Now()
	server := normalizeBarkServer(target.Sub.BarkServer, Config{Bark: notifier.cfg})
	guardKey := server + "|" + target.Sub.BarkID
	useOfficialGuard := isOfficialBarkServer(server)
	if useOfficialGuard {
		if ok, reason, until := notifier.errorGuard.Allow(guardKey, now); !ok {
			return fanoutResult{Target: target, Skipped: true, Reason: reason, Until: until}
		}
	}
	start := time.Now()
	err := notifier.Send(ctx, server, target.Sub.BarkID, target.Title, target.Subtitle, target.Body, target.Params, target.Options)
	elapsed := time.Since(start)
	if useOfficialGuard {
		notifier.errorGuard.Record(guardKey, err, time.Now())
	}
	if err != nil {
		return fanoutResult{Target: target, Err: err, Elapsed: elapsed}
	}
	return fanoutResult{Target: target, Pushed: true, Elapsed: elapsed}
}

func splitFanoutTargets(cfg Config, targets []fanoutTarget) ([]fanoutTarget, []fanoutTarget) {
	official := make([]fanoutTarget, 0, len(targets))
	selfHosted := make([]fanoutTarget, 0, len(targets))
	for _, target := range targets {
		server := normalizeBarkServer(target.Sub.BarkServer, cfg)
		if isOfficialBarkServer(server) {
			official = append(official, target)
			continue
		}
		selfHosted = append(selfHosted, target)
	}
	return official, selfHosted
}

func fanoutServerCounts(cfg Config, targets []fanoutTarget) (int, int) {
	official, selfHosted := splitFanoutTargets(cfg, targets)
	return len(official), len(selfHosted)
}

func officialFanoutConcurrency(cfg Config, targetCount int) int {
	if targetCount <= 0 {
		return 0
	}
	concurrency := cfg.Alert.FanoutConcurrency
	if concurrency <= 0 {
		concurrency = 100
	}
	if concurrency > targetCount {
		concurrency = targetCount
	}
	if concurrency > 500 {
		concurrency = 500
	}
	return concurrency
}

func selfHostedFanoutConcurrency(cfg Config, targetCount int) int {
	if targetCount <= 0 {
		return 0
	}
	concurrency := cfg.Alert.SelfHostedConcurrency
	if concurrency <= 0 {
		concurrency = 1000
	}
	if concurrency > targetCount {
		concurrency = targetCount
	}
	return concurrency
}

func notifyPriority(level string) int {
	switch strings.ToLower(strings.TrimSpace(level)) {
	case "critical":
		return 0
	case "active", "timeSensitive":
		return 1
	default:
		return 2
	}
}

func dispatchOne(ctx context.Context, cfg Config, notifier *Notifier, alertCache *AlertCache, event Event, sub Subscription) (int, int) {
	sub, decision := nearestSubscriptionForEvent(cfg, sub, event)
	if cfg.Alert.MaxDistanceKM > 0 && decision.DistanceKM > cfg.Alert.MaxDistanceKM && !bypassDeliveryFilters(event) {
		return 0, 1
	}
	if notifyLevelForIntensity(sub, decision.EstimatedIntensity) == "" && !bypassDeliveryFilters(event) {
		return 0, 1
	}
	title, subtitle, body := formatAlert(event, decision, sub)
	mapURL := appleMapsDirectionsURL(sub, event)
	params := map[string]string{
		"url": clickURL(cfg, alertCache, event, decision, sub, mapURL),
	}
	server := normalizeBarkServer(sub.BarkServer, cfg)
	guardKey := server + "|" + sub.BarkID
	useOfficialGuard := isOfficialBarkServer(server)
	if useOfficialGuard {
		if ok, reason, until := notifier.errorGuard.Allow(guardKey, time.Now()); !ok {
			log.Printf("bark send skipped id=%s bark=%s type=%s reason=%s until=%s",
				event.EventID, maskKey(sub.BarkID), event.Type, reason, formatBeijing(until, time.RFC3339))
			return 0, 1
		}
	}
	err := notifier.Send(ctx, server, sub.BarkID, title, subtitle, body, params, pushOptions(cfg, sub, decision))
	if useOfficialGuard {
		notifier.errorGuard.Record(guardKey, err, time.Now())
	}
	if err != nil {
		log.Printf("bark send failed id=%s bark=%s type=%s: %v", event.EventID, maskKey(sub.BarkID), event.Type, err)
		return 0, 0
	}
	log.Printf("pushed one id=%s bark=%s type=%s M%.1f dist=%.1fkm intensity=%d eta_s=%ds",
		event.EventID, maskKey(sub.BarkID), event.Type, event.Magnitude, decision.DistanceKM,
		decision.EstimatedIntensity, decision.SecondsToS)
	return 1, 0
}

func parseEvent(payload []byte) (Event, bool, error) {
	var raw RawEvent
	if err := json.Unmarshal(payload, &raw); err != nil {
		return Event{}, false, err
	}
	typ := strings.TrimSpace(getString(raw, "type"))
	if typ == "" || typ == "heartbeat" || typ == "pong" || strings.HasSuffix(typ, "_eqlist") {
		return Event{}, false, nil
	}
	if !strings.Contains(typ, "eew") {
		return Event{}, false, nil
	}
	timeZoneSeconds := 8 * 3600
	if strings.Contains(strings.ToLower(typ), "jma") {
		timeZoneSeconds = 9 * 3600
	}

	event := Event{
		Type:          typ,
		EventID:       firstString(raw, "EventID", "ID", "event_id", "EventId"),
		ReportNum:     firstInt(raw, "ReportNum", "ReportNumber", "Serial", "AnnouncedNumber"),
		OriginTime:    parseTimeInZone(firstString(raw, "OriginTime", "OriginAt", "origin_time"), timeZoneSeconds),
		AnnouncedTime: parseTimeInZone(firstString(raw, "AnnouncedTime", "ReportTime", "UpdateTime", "announced_time"), timeZoneSeconds),
		Hypocenter:    firstString(raw, "HypoCenter", "Hypocenter", "Region", "Place", "Location"),
		Latitude:      firstFloat(raw, "Latitude", "latitude", "Lat"),
		Longitude:     firstFloat(raw, "Longitude", "longitude", "Lon", "Lng"),
		Magnitude:     firstFloat(raw, "Magnitude", "Magunitude", "M", "Mag"),
		DepthKM:       firstFloat(raw, "Depth", "DepthKM", "depth"),
		MaxIntensity:  firstValueString(raw, "MaxIntensity", "MaxInt", "max_intensity"),
		Final:         firstBool(raw, "isFinal", "Final", "IsFinal"),
		Cancel:        firstBool(raw, "isCancel", "Cancel", "IsCancel"),
		Training:      firstBool(raw, "isTraining", "Training", "IsTraining"),
		Serial:        firstValueString(raw, "Serial", "ReportNum", "ReportNumber"),
		Raw:           raw,
	}

	if event.EventID == "" {
		event.EventID = fmt.Sprintf("%s:%.3f:%.3f:%s", event.Type, event.Latitude, event.Longitude, event.OriginTime.Format(time.RFC3339))
	}
	if event.ReportNum <= 0 {
		event.ReportNum = 1
	}
	if event.Latitude == 0 && event.Longitude == 0 {
		return Event{}, false, errors.New("event has no coordinates")
	}
	if event.Magnitude <= 0 {
		return Event{}, false, errors.New("event has no magnitude")
	}
	return event, true, nil
}

func simulatedEvent(subs []Subscription, kind string) Event {
	if len(subs) == 0 {
		return Event{}
	}
	sub := subs[0]
	normalizeSubscription(&sub)
	now := time.Now()
	target := simulationTargetIntensity(sub, kind)
	magnitude := 3.6
	depth := 10.0
	switch kind {
	case "small":
		magnitude = 3.6
		depth = 12.0
	case "medium":
		magnitude = 4.6
		depth = 10.0
	case "large":
		magnitude = 6.2
		depth = 10.0
	}
	distance := simulationDistanceForIntensity(magnitude, depth, target)
	latOffset := distance / 111.32
	if sub.Latitude+latOffset > 89 {
		latOffset = -latOffset
	}
	return Event{
		Type:         "simulate_eew",
		EventID:      "SIM-" + now.Format("20060102150405"),
		ReportNum:    1,
		OriginTime:   now,
		Hypocenter:   "模拟震源（" + simulationKindLabel(kind) + "）",
		Latitude:     sub.Latitude + latOffset,
		Longitude:    sub.Longitude,
		Magnitude:    magnitude,
		DepthKM:      depth,
		MaxIntensity: strconv.Itoa(target),
	}
}

func simulationTargetIntensity(sub Subscription, kind string) int {
	rules := sub.NotifyRules
	if rules == (NotificationRules{}) {
		rules = defaultNotificationRules()
	}
	switch kind {
	case "small":
		return clampInt(rules.PassiveMax, 0, 7)
	case "medium":
		return clampInt((rules.PassiveMax+1+rules.ActiveMax)/2, rules.PassiveMax+1, rules.ActiveMax)
	case "large":
		return clampInt(rules.CriticalMin, 0, 7)
	default:
		return clampInt(rules.PassiveMax, 0, 7)
	}
}

func simulationDistanceForIntensity(magnitude, depth float64, target int) float64 {
	bestDistance := 1.0
	bestDelta := 99
	for distance := 1.0; distance <= 1500; distance += 1 {
		hypo := math.Sqrt(distance*distance + depth*depth)
		intensity := estimateIntensity(magnitude, hypo)
		delta := absInt(intensity - target)
		if delta < bestDelta {
			bestDistance = distance
			bestDelta = delta
		}
		if delta == 0 {
			return distance
		}
	}
	return bestDistance
}

func simulationKindLabel(kind string) string {
	switch kind {
	case "small":
		return "passive 测试"
	case "medium":
		return "active 测试"
	case "large":
		return "critical 测试"
	default:
		return "测试"
	}
}

func serveHistoryAPI(w http.ResponseWriter, r *http.Request, cfg Config, store *Store, pathBarkID string) {
	records, err := historyRecords(r.Context(), cfg, r.URL.Query().Get("refresh") == "1")
	if err != nil {
		log.Printf("history records failed: %v", err)
		writeJSON(w, http.StatusBadGateway, APIResponse{Success: false, Message: "历史地震数据获取失败"})
		return
	}

	barkID := strings.TrimSpace(pathBarkID)
	if barkID == "" {
		barkID = strings.TrimSpace(r.URL.Query().Get("bark_id"))
	}
	if barkID != "" {
		sub, ok := store.Get(barkID)
		if !ok {
			writeJSON(w, http.StatusNotFound, APIResponse{Success: false, Message: "未找到订阅"})
			return
		}
		records = annotateHistoryRecords(cfg, sub, records)
	}

	records = filterHistoryRecords(records, r.URL.Query())
	writeJSON(w, http.StatusOK, APIResponse{Success: true, Message: "ok", Data: records})
}

func simulationPreviews(cfg Config, sub Subscription) []SimulationPreview {
	kinds := []struct {
		kind  string
		label string
	}{
		{kind: "small", label: "passive 测试"},
		{kind: "medium", label: "active 测试"},
		{kind: "large", label: "critical 测试"},
	}
	previews := make([]SimulationPreview, 0, len(kinds))
	for _, item := range kinds {
		event := simulatedEvent([]Subscription{sub}, item.kind)
		selectedSub, decision := nearestSubscriptionForEvent(cfg, sub, event)
		level := notifyLevelForIntensity(selectedSub, decision.EstimatedIntensity)
		previews = append(previews, SimulationPreview{
			Kind:               item.kind,
			Label:              item.label,
			Magnitude:          event.Magnitude,
			MaxIntensity:       event.MaxIntensity,
			EstimatedIntensity: decision.EstimatedIntensity,
			NotifyLevel:        level,
			NotifyLabel:        notifyLabel(level),
			DistanceKM:         round1(decision.DistanceKM),
			HypocentralKM:      round1(decision.HypocentralKM),
		})
	}
	return previews
}

func fetchWolfxHistory(ctx context.Context) ([]HistoryRecord, error) {
	type source struct {
		name string
		url  string
	}
	sources := []source{
		{name: "cenc", url: "https://api.wolfx.jp/cenc_eqlist.json"},
		{name: "jma", url: "https://api.wolfx.jp/jma_eqlist.json"},
	}
	client := &http.Client{Timeout: 12 * time.Second}
	var records []HistoryRecord
	var fetchErrors []string
	for _, src := range sources {
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, src.url, nil)
		if err != nil {
			fetchErrors = append(fetchErrors, err.Error())
			continue
		}
		req.Header.Set("User-Agent", "eew-bark/1.0")
		resp, err := client.Do(req)
		if err != nil {
			fetchErrors = append(fetchErrors, fmt.Sprintf("%s: %v", src.name, err))
			continue
		}
		body, readErr := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		_ = resp.Body.Close()
		if readErr != nil {
			fetchErrors = append(fetchErrors, fmt.Sprintf("%s: %v", src.name, readErr))
			continue
		}
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			fetchErrors = append(fetchErrors, fmt.Sprintf("%s returned %s", src.name, resp.Status))
			continue
		}
		var raw map[string]json.RawMessage
		if err := json.Unmarshal(body, &raw); err != nil {
			fetchErrors = append(fetchErrors, fmt.Sprintf("%s: %v", src.name, err))
			continue
		}
		keys := make([]string, 0, len(raw))
		for key := range raw {
			if !strings.HasPrefix(strings.ToLower(key), "no") {
				continue
			}
			keys = append(keys, key)
		}
		sort.Slice(keys, func(i, j int) bool { return historyKeyNumber(keys[i]) < historyKeyNumber(keys[j]) })
		for _, key := range keys {
			var item RawEvent
			if err := json.Unmarshal(raw[key], &item); err != nil {
				continue
			}
			if record, ok := historyRecordFromRaw(src.name, key, item); ok {
				records = append(records, record)
			}
		}
	}
	if len(records) == 0 && len(fetchErrors) > 0 {
		return nil, errors.New(strings.Join(fetchErrors, "; "))
	}
	return records, nil
}

func historyRecords(ctx context.Context, cfg Config, forceRefresh bool) ([]HistoryRecord, error) {
	cache, cacheErr := loadHistoryCache(cfg.Server.HistoryPath)
	refreshAfter := time.Duration(cfg.Server.HistoryRefreshMinutes) * time.Minute
	cacheFresh := cacheErr == nil && len(cache.Records) > 0 && time.Since(time.Unix(cache.UpdatedAt, 0)) < refreshAfter
	if cacheFresh && !forceRefresh {
		return mergeHistoryRecords(builtinHistoryRecords(), cache.Records), nil
	}

	latest, err := fetchWolfxHistory(ctx)
	if err != nil {
		if cacheErr == nil && len(cache.Records) > 0 {
			log.Printf("history refresh failed, using cache: %v", err)
			return mergeHistoryRecords(builtinHistoryRecords(), cache.Records), nil
		}
		return builtinHistoryRecords(), nil
	}

	mergedRemote := mergeHistoryRecords(latest, cache.Records)
	_ = saveHistoryCache(cfg.Server.HistoryPath, HistoryCacheFile{
		UpdatedAt: time.Now().Unix(),
		Records:   mergedRemote,
	})
	return mergeHistoryRecords(builtinHistoryRecords(), mergedRemote), nil
}

func loadHistoryCache(path string) (HistoryCacheFile, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return HistoryCacheFile{}, err
	}
	var cache HistoryCacheFile
	if err := json.Unmarshal(data, &cache); err != nil {
		return HistoryCacheFile{}, err
	}
	return cache, nil
}

func saveHistoryCache(path string, cache HistoryCacheFile) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	data, err := json.MarshalIndent(cache, "", "  ")
	if err != nil {
		return err
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, data, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

func mergeHistoryRecords(groups ...[]HistoryRecord) []HistoryRecord {
	seen := make(map[string]bool)
	merged := []HistoryRecord{}
	for _, records := range groups {
		for _, record := range records {
			key := strings.ToLower(record.Source) + ":" + record.Key
			if key == ":" {
				key = strings.ToLower(record.EventID)
			}
			if key == "" || seen[key] {
				continue
			}
			seen[key] = true
			merged = append(merged, record)
		}
	}
	return merged
}

func filterHistoryRecords(records []HistoryRecord, values url.Values) []HistoryRecord {
	source := strings.ToLower(strings.TrimSpace(values.Get("source")))
	minMagnitude, _ := strconv.ParseFloat(strings.TrimSpace(values.Get("min_magnitude")), 64)
	limit, _ := strconv.Atoi(strings.TrimSpace(values.Get("limit")))
	if limit <= 0 || limit > 200 {
		limit = 100
	}
	offset, _ := strconv.Atoi(strings.TrimSpace(values.Get("offset")))
	if offset < 0 {
		offset = 0
	}
	filtered := make([]HistoryRecord, 0, len(records))
	for _, record := range records {
		if source == "" || source == "all" {
			if strings.EqualFold(record.Source, "major") {
				continue
			}
		} else {
			if !strings.EqualFold(record.Source, source) {
				continue
			}
		}
		if minMagnitude > 0 && record.Magnitude < minMagnitude {
			continue
		}
		filtered = append(filtered, record)
		if len(filtered) >= offset+limit {
			break
		}
	}
	if offset >= len(filtered) {
		return []HistoryRecord{}
	}
	return filtered[offset:]
}

func builtinHistoryRecords() []HistoryRecord {
	return []HistoryRecord{
		{
			Source:       "major",
			Key:          "wenchuan-2008",
			EventID:      "USGS-usp000g650",
			OriginTime:   "2008-05-12 14:28:01",
			Hypocenter:   "四川汶川地震",
			Latitude:     31.002,
			Longitude:    103.322,
			Magnitude:    7.9,
			DepthKM:      19,
			MaxIntensity: "XI",
			Note:         "2008年汶川地震，USGS Mw7.9；中国地震局常用表述为 Ms8.0。",
		},
		{
			Source:       "major",
			Key:          "tangshan-1976",
			EventID:      "USGS-Tangshan-1976",
			OriginTime:   "1976-07-28 03:42:00",
			Hypocenter:   "河北唐山地震",
			Latitude:     39.57,
			Longitude:    117.98,
			Magnitude:    7.8,
			DepthKM:      15,
			MaxIntensity: "XI",
			Note:         "1976年唐山地震，USGS 资料记录震级7.8、深度15km、震中烈度XI。",
		},
	}
}

func annotateHistoryRecords(cfg Config, sub Subscription, records []HistoryRecord) []HistoryRecord {
	annotated := make([]HistoryRecord, len(records))
	copy(annotated, records)
	for i := range annotated {
		event := historicalEvent(annotated[i])
		_, decision := nearestSubscriptionForEvent(cfg, sub, event)
		annotated[i].EstimatedIntensity = decision.EstimatedIntensity
		annotated[i].DistanceKM = round1(decision.DistanceKM)
		annotated[i].HypocentralKM = round1(decision.HypocentralKM)
	}
	return annotated
}

func historyRecordFromRaw(source, key string, raw RawEvent) (HistoryRecord, bool) {
	record := HistoryRecord{
		Source:       source,
		Key:          key,
		EventID:      firstString(raw, "EventID", "event_id", "ID"),
		Hypocenter:   firstString(raw, "location", "placeName", "HypoCenter", "Hypocenter"),
		Latitude:     firstFloat(raw, "latitude", "Latitude"),
		Longitude:    firstFloat(raw, "longitude", "Longitude"),
		Magnitude:    firstFloat(raw, "magnitude", "Magnitude", "Magunitude"),
		DepthKM:      parseDepthKM(firstValueString(raw, "depth", "Depth", "DepthKM")),
		MaxIntensity: firstValueString(raw, "intensity", "shindo", "MaxIntensity", "MaxInt"),
	}
	switch source {
	case "jma":
		record.OriginTime = firstString(raw, "time_full", "time")
	case "cenc":
		record.OriginTime = firstString(raw, "time", "OriginTime")
	default:
		record.OriginTime = firstString(raw, "time", "time_full", "OriginTime")
	}
	if record.EventID == "" {
		record.EventID = source + "-" + key
	}
	if record.Latitude == 0 && record.Longitude == 0 {
		return HistoryRecord{}, false
	}
	if record.Magnitude <= 0 {
		return HistoryRecord{}, false
	}
	return record, true
}

func findHistoryRecord(records []HistoryRecord, source, key string) (HistoryRecord, bool) {
	for _, record := range records {
		if strings.EqualFold(record.Source, source) && record.Key == key {
			return record, true
		}
	}
	return HistoryRecord{}, false
}

func historicalEvent(record HistoryRecord) Event {
	now := time.Now()
	return Event{
		Type:          "history_simulate_" + record.Source,
		EventID:       fmt.Sprintf("HIST-%s-%s-%s", strings.ToUpper(record.Source), record.EventID, now.Format("20060102150405")),
		ReportNum:     1,
		OriginTime:    now,
		AnnouncedTime: now,
		Hypocenter:    record.Hypocenter,
		Latitude:      record.Latitude,
		Longitude:     record.Longitude,
		Magnitude:     record.Magnitude,
		DepthKM:       record.DepthKM,
		MaxIntensity:  record.MaxIntensity,
		Raw: RawEvent{
			"source":               record.Source,
			"history_key":          record.Key,
			"original_event_id":    record.EventID,
			"original_origin_time": record.OriginTime,
		},
	}
}

func historyKeyNumber(key string) int {
	n, err := strconv.Atoi(strings.TrimPrefix(strings.ToLower(key), "no"))
	if err != nil {
		return 9999
	}
	return n
}

func parseDepthKM(value string) float64 {
	value = strings.TrimSpace(strings.ToLower(value))
	value = strings.TrimSuffix(value, "km")
	value = strings.TrimSuffix(value, "公里")
	value = strings.TrimSpace(value)
	if value == "" {
		return 0
	}
	depth, _ := strconv.ParseFloat(value, 64)
	return depth
}

func isTestEvent(event Event) bool {
	return strings.Contains(event.Type, "simulate") && !isHistoryTestEvent(event)
}

func isHistoryTestEvent(event Event) bool {
	return strings.Contains(event.Type, "history_simulate")
}

func bypassDeliveryFilters(event Event) bool {
	return isTestEvent(event) || isHistoryTestEvent(event)
}

func evaluate(cfg Config, sub Subscription, event Event) Decision {
	dist := haversineKM(sub.Latitude, sub.Longitude, event.Latitude, event.Longitude)
	hypo := math.Sqrt(dist*dist + event.DepthKM*event.DepthKM)
	origin := event.OriginTime
	if origin.IsZero() {
		origin = time.Now()
	}
	pSeconds, sSeconds := seismicTravelSeconds(cfg, dist, event.DepthKM)
	pArrival := origin.Add(time.Duration(pSeconds * float64(time.Second)))
	sArrival := origin.Add(time.Duration(sSeconds * float64(time.Second)))
	now := time.Now()
	return Decision{
		DistanceKM:         dist,
		HypocentralKM:      hypo,
		EstimatedIntensity: estimateIntensity(event.Magnitude, hypo),
		SArrival:           sArrival,
		PArrival:           pArrival,
		SecondsToS:         int(math.Round(sArrival.Sub(now).Seconds())),
		SecondsToP:         int(math.Round(pArrival.Sub(now).Seconds())),
	}
}

func nearestSubscriptionForEvent(cfg Config, sub Subscription, event Event) (Subscription, Decision) {
	normalizeSubscription(&sub)
	bestSub := sub
	bestDecision := evaluate(cfg, sub, event)
	for _, loc := range sub.Locations {
		candidate := sub
		candidate.LocationName = loc.Name
		candidate.Latitude = loc.Latitude
		candidate.Longitude = loc.Longitude
		decision := evaluate(cfg, candidate, event)
		if decision.DistanceKM < bestDecision.DistanceKM {
			bestSub = candidate
			bestDecision = decision
		}
	}
	return bestSub, bestDecision
}

type travelTimeSample struct {
	deltaDeg  float64
	pSeconds  float64
	spSeconds float64
}

var regionalTravelTimeTable = []travelTimeSample{
	{0.0, 5.4, 4.0},
	{0.5, 10.6, 7.8},
	{1.0, 17.7, 13.5},
	{1.5, 24.6, 19.0},
	{2.0, 31.4, 24.4},
	{2.5, 38.3, 29.9},
	{3.0, 45.2, 35.4},
	{3.5, 52.1, 40.9},
	{4.0, 58.9, 46.4},
	{4.5, 65.8, 51.9},
	{5.0, 72.7, 57.4},
	{5.5, 79.6, 62.8},
	{6.0, 86.4, 68.3},
	{6.5, 93.3, 73.8},
	{7.0, 100.2, 79.2},
	{7.5, 107.0, 84.7},
	{8.0, 113.9, 90.1},
	{8.5, 120.7, 95.6},
	{9.0, 127.6, 101.0},
	{9.5, 134.4, 106.5},
	{10.0, 141.3, 111.9},
	{11.0, 155.0, 122.7},
	{12.0, 168.7, 133.5},
	{13.0, 182.3, 144.3},
	{14.0, 196.0, 155.0},
	{15.0, 209.5, 165.8},
	{16.0, 222.5, 177.1},
	{17.0, 235.2, 188.7},
	{18.0, 247.5, 200.5},
	{19.0, 258.8, 213.4},
	{20.0, 269.7, 223.8},
	{21.0, 280.6, 232.9},
	{22.0, 291.3, 241.8},
	{23.0, 301.9, 249.2},
	{24.0, 311.6, 255.7},
	{25.0, 320.7, 262.6},
	{26.0, 329.8, 269.4},
	{27.0, 338.8, 276.2},
	{28.0, 347.7, 282.9},
	{29.0, 356.6, 289.8},
	{30.0, 365.5, 296.6},
}

func seismicTravelSeconds(cfg Config, distanceKM, depthKM float64) (float64, float64) {
	hypo := math.Sqrt(distanceKM*distanceKM + depthKM*depthKM)
	fixedP, fixedS := fixedWaveTravelSeconds(cfg, hypo)
	if distanceKM < 100 || len(regionalTravelTimeTable) < 2 {
		return fixedP, fixedS
	}
	degrees := distanceKM / 111.195
	tableP, tableS, ok := interpolatedRegionalTravelSeconds(degrees)
	if !ok {
		return fixedP, fixedS
	}
	depthAdjustment := depthTravelAdjustment(cfg, depthKM, 33)
	tableP += depthAdjustment.p
	tableS += depthAdjustment.s
	if tableP <= 0 || tableS <= tableP {
		return fixedP, fixedS
	}
	return tableP, tableS
}

func fixedWaveTravelSeconds(cfg Config, hypocentralKM float64) (float64, float64) {
	pSpeed := cfg.Alert.PWaveKMS
	if pSpeed <= 0 {
		pSpeed = 6.0
	}
	sSpeed := cfg.Alert.SWaveKMS
	if sSpeed <= 0 {
		sSpeed = 3.5
	}
	return hypocentralKM / pSpeed, hypocentralKM / sSpeed
}

type depthAdjustmentSeconds struct {
	p float64
	s float64
}

func depthTravelAdjustment(cfg Config, depthKM, referenceDepthKM float64) depthAdjustmentSeconds {
	if depthKM < 0 {
		depthKM = 0
	}
	pAtDepth, sAtDepth := fixedWaveTravelSeconds(cfg, depthKM)
	pAtReference, sAtReference := fixedWaveTravelSeconds(cfg, referenceDepthKM)
	return depthAdjustmentSeconds{p: pAtDepth - pAtReference, s: sAtDepth - sAtReference}
}

func interpolatedRegionalTravelSeconds(deltaDeg float64) (float64, float64, bool) {
	if deltaDeg < regionalTravelTimeTable[0].deltaDeg || deltaDeg > regionalTravelTimeTable[len(regionalTravelTimeTable)-1].deltaDeg {
		return 0, 0, false
	}
	for i := 1; i < len(regionalTravelTimeTable); i++ {
		prev := regionalTravelTimeTable[i-1]
		next := regionalTravelTimeTable[i]
		if deltaDeg > next.deltaDeg {
			continue
		}
		if next.deltaDeg == prev.deltaDeg {
			return next.pSeconds, next.pSeconds + next.spSeconds, true
		}
		ratio := (deltaDeg - prev.deltaDeg) / (next.deltaDeg - prev.deltaDeg)
		p := prev.pSeconds + ratio*(next.pSeconds-prev.pSeconds)
		sp := prev.spSeconds + ratio*(next.spSeconds-prev.spSeconds)
		return p, p + sp, true
	}
	return 0, 0, false
}

func formatAlert(event Event, d Decision, sub Subscription) (string, string, string) {
	eta := "已到达"
	if d.SecondsToS > 0 {
		eta = fmt.Sprintf("%d秒后到达", d.SecondsToS)
	}
	region := event.Hypocenter
	if region == "" {
		region = fmt.Sprintf("%.2f, %.2f", event.Latitude, event.Longitude)
	}

	prefix := "地震预警"
	switch {
	case isTestEvent(event):
		prefix = "地震预警测试"
	}
	title := fmt.Sprintf("%s %s", prefix, eta)
	subtitle := fmt.Sprintf("M%.1f 预计烈度%d 距%.0fkm", event.Magnitude, d.EstimatedIntensity, d.DistanceKM)
	lines := []string{}
	switch {
	case isTestEvent(event):
		lines = append(lines, "[测试] 这是一条模拟预警，不是真实地震。")
	}
	notifyLevel := notifyLevelForIntensity(sub, d.EstimatedIntensity)
	lines = append(lines,
		fmt.Sprintf("地点: %s", region),
		fmt.Sprintf("震源: %.2f, %.2f 深度%.0fkm", event.Latitude, event.Longitude, event.DepthKM),
		fmt.Sprintf("距离: 震中%.0fkm 震源%.0fkm", d.DistanceKM, d.HypocentralKM),
		fmt.Sprintf("预计: P波%+d秒 S波%+d秒 烈度%d", d.SecondsToP, d.SecondsToS, d.EstimatedIntensity),
		fmt.Sprintf("级别: %s（%s）", intensityBandLabel(notifyLevel), notifyLabel(notifyLevel)),
	)
	lines = append(lines,
		fmt.Sprintf("震级: M%.1f 最大烈度%s", event.Magnitude, fallback(event.MaxIntensity, "未知")),
		fmt.Sprintf("来源: %s 第%d报", alertSourceLabel(event), event.ReportNum),
	)
	lines = append(lines, "发震: "+alertOriginTimeLabel(event))
	return title, subtitle, strings.Join(lines, "\n")
}

func alertOriginTimeLabel(event Event) string {
	if isHistoryTestEvent(event) {
		original := strings.TrimSpace(getString(event.Raw, "original_origin_time"))
		if original != "" {
			return original
		}
	}
	return formatBeijing(event.OriginTime, "2006-01-02 15:04:05")
}

func formatBeijing(t time.Time, layout string) string {
	if t.IsZero() {
		return "未知"
	}
	return t.In(beijingTZ).Format(layout)
}

func alertSourceLabel(event Event) string {
	if isHistoryTestEvent(event) {
		source := strings.ToUpper(strings.TrimSpace(getString(event.Raw, "source")))
		if source != "" {
			return source
		}
	}
	return event.Type
}

func clickURL(cfg Config, alertCache *AlertCache, event Event, decision Decision, sub Subscription, mapURL string) string {
	if strings.TrimSpace(cfg.Alert.ClickURL) != "" {
		return strings.TrimSpace(cfg.Alert.ClickURL)
	}
	publicURL := strings.TrimRight(strings.TrimSpace(cfg.Server.PublicURL), "/")
	if publicURL == "" {
		return mapURL
	}
	token, err := alertCache.Put(AlertPage{
		Event:      event,
		Decision:   decision,
		Subscriber: sub,
		WeChatURL:  cfg.Alert.WeChatURL,
		MapURL:     mapURL,
	})
	if err != nil {
		log.Printf("create alert detail token failed: %v", err)
		return mapURL
	}
	return publicURL + "/alert/" + token
}

func pushOptions(cfg Config, sub Subscription, decision Decision) PushOptions {
	switch notifyLevelForIntensity(sub, decision.EstimatedIntensity) {
	case "critical":
		return PushOptions{Level: "critical", Sound: cfg.Bark.Sound, Volume: cfg.Bark.Volume, Call: true}
	case "active":
		return PushOptions{Level: "active"}
	case "passive":
		return PushOptions{Level: "passive"}
	}
	return PushOptions{
		Level:  cfg.Bark.Level,
		Sound:  cfg.Bark.Sound,
		Volume: cfg.Bark.Volume,
		Call:   cfg.Bark.Call,
	}
}

func NewNotifier(cfg BarkConfig, alerts ...AlertConfig) *Notifier {
	alert := AlertConfig{
		FanoutErrorBudget:   800,
		KeyFailureThreshold: 3,
		KeyQuarantineMinute: 24 * 60,
	}
	if len(alerts) > 0 {
		alert = alerts[0]
	}
	if alert.FanoutErrorBudget <= 0 {
		alert.FanoutErrorBudget = 800
	}
	if alert.KeyFailureThreshold <= 0 {
		alert.KeyFailureThreshold = 3
	}
	if alert.KeyQuarantineMinute <= 0 {
		alert.KeyQuarantineMinute = 24 * 60
	}
	transport := &http.Transport{
		Proxy:                 http.ProxyFromEnvironment,
		DialContext:           (&net.Dialer{Timeout: 3 * time.Second, KeepAlive: 30 * time.Second}).DialContext,
		TLSHandshakeTimeout:   3 * time.Second,
		ResponseHeaderTimeout: 3 * time.Second,
		ExpectContinueTimeout: 1 * time.Second,
		MaxIdleConns:          2000,
		MaxIdleConnsPerHost:   1200,
		IdleConnTimeout:       90 * time.Second,
		ForceAttemptHTTP2:     true,
	}
	return &Notifier{
		cfg: cfg,
		client: &http.Client{
			Timeout:   3 * time.Second,
			Transport: transport,
		},
		errorGuard: NewBarkErrorGuard(alert.FanoutErrorBudget, alert.KeyFailureThreshold, time.Duration(alert.KeyQuarantineMinute)*time.Minute),
	}
}

func NewBarkErrorGuard(budget, keyFailureThreshold int, keyQuarantine time.Duration) *BarkErrorGuard {
	if budget <= 0 {
		budget = 800
	}
	if keyFailureThreshold <= 0 {
		keyFailureThreshold = 3
	}
	if keyQuarantine <= 0 {
		keyQuarantine = 24 * time.Hour
	}
	return &BarkErrorGuard{
		window:              5 * time.Minute,
		budget:              budget,
		keyFailureThreshold: keyFailureThreshold,
		keyQuarantine:       keyQuarantine,
		keys:                make(map[string]keyFailure),
	}
}

func (g *BarkErrorGuard) Allow(key string, now time.Time) (bool, string, time.Time) {
	if g == nil {
		return true, "", time.Time{}
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	g.pruneLocked(now)
	if len(g.badRequests) >= g.budget {
		return false, "global_error_budget", now.Add(g.window)
	}
	state := g.keys[key]
	if !state.QuarantinedUntil.IsZero() && now.Before(state.QuarantinedUntil) {
		return false, "key_quarantined", state.QuarantinedUntil
	}
	return true, "", time.Time{}
}

func (g *BarkErrorGuard) Record(key string, err error, now time.Time) {
	if g == nil {
		return
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	g.pruneLocked(now)
	if err == nil {
		delete(g.keys, key)
		return
	}
	var statusErr *HTTPStatusError
	if !errors.As(err, &statusErr) {
		return
	}
	if statusErr.StatusCode >= 400 {
		g.badRequests = append(g.badRequests, now)
	}
	if statusErr.StatusCode != http.StatusBadRequest && statusErr.StatusCode != http.StatusNotFound {
		return
	}
	state := g.keys[key]
	state.Count++
	state.LastFailure = now
	if state.Count >= g.keyFailureThreshold {
		state.QuarantinedUntil = now.Add(g.keyQuarantine)
	}
	g.keys[key] = state
}

func (g *BarkErrorGuard) pruneLocked(now time.Time) {
	cutoff := now.Add(-g.window)
	keepFrom := 0
	for keepFrom < len(g.badRequests) && g.badRequests[keepFrom].Before(cutoff) {
		keepFrom++
	}
	if keepFrom > 0 {
		g.badRequests = append([]time.Time(nil), g.badRequests[keepFrom:]...)
	}
	for key, state := range g.keys {
		if !state.QuarantinedUntil.IsZero() {
			if now.After(state.QuarantinedUntil) {
				delete(g.keys, key)
			}
			continue
		}
		if now.Sub(state.LastFailure) > g.window {
			delete(g.keys, key)
		}
	}
}

func (n *Notifier) Send(ctx context.Context, server, key, title, subtitle, body string, extra map[string]string, options PushOptions) error {
	server = normalizeBarkServer(server, Config{Bark: n.cfg})
	endpoint := server + "/" + url.PathEscape(key)
	form := url.Values{}
	form.Set("title", title)
	form.Set("subtitle", subtitle)
	form.Set("body", body)
	form.Set("group", n.cfg.Group)
	level := fallback(options.Level, n.cfg.Level)
	form.Set("level", level)
	sound := options.Sound
	if sound == "" && level != "passive" {
		sound = n.cfg.Sound
	}
	if sound != "" {
		form.Set("sound", sound)
	}
	volume := options.Volume
	if volume == 0 {
		volume = n.cfg.Volume
	}
	if volume > 0 && level != "passive" {
		form.Set("volume", strconv.Itoa(volume))
	}
	call := n.cfg.Call
	if options.Level != "" {
		call = options.Call
	}
	if call && level != "passive" {
		form.Set("call", "1")
	}
	for k, v := range extra {
		if v != "" {
			form.Set(k, v)
		}
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewBufferString(form.Encode()))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("User-Agent", "eew-bark/1.0")

	res, err := n.client.Do(req)
	if err != nil {
		return err
	}
	defer res.Body.Close()
	if res.StatusCode < 200 || res.StatusCode >= 300 {
		body, _ := io.ReadAll(io.LimitReader(res.Body, 2048))
		return &HTTPStatusError{StatusCode: res.StatusCode, Body: strings.TrimSpace(string(body))}
	}
	return nil
}

func NewDeduper(keepFor time.Duration) *Deduper {
	return &Deduper{seen: make(map[string]seenEvent), keepFor: keepFor}
}

func (d *Deduper) Seen(key string, reportNum int, minGap int) bool {
	d.mu.Lock()
	defer d.mu.Unlock()
	now := time.Now()
	for k, v := range d.seen {
		if now.Sub(v.At) > d.keepFor {
			delete(d.seen, k)
		}
	}
	prev, ok := d.seen[key]
	if ok && reportNum-prev.ReportNum < minGap {
		return true
	}
	d.seen[key] = seenEvent{ReportNum: reportNum, At: now}
	return false
}

func (e Event) Key() string {
	return e.Type + ":" + e.EventID
}

func haversineKM(lat1, lon1, lat2, lon2 float64) float64 {
	const r = 6371.0088
	phi1 := lat1 * math.Pi / 180
	phi2 := lat2 * math.Pi / 180
	dPhi := (lat2 - lat1) * math.Pi / 180
	dLambda := (lon2 - lon1) * math.Pi / 180
	a := math.Sin(dPhi/2)*math.Sin(dPhi/2) + math.Cos(phi1)*math.Cos(phi2)*math.Sin(dLambda/2)*math.Sin(dLambda/2)
	return 2 * r * math.Atan2(math.Sqrt(a), math.Sqrt(1-a))
}

func appleMapsDirectionsURL(sub Subscription, event Event) string {
	startLat, startLon := wgs84ToGCJ02(sub.Latitude, sub.Longitude)
	endLat, endLon := wgs84ToGCJ02(event.Latitude, event.Longitude)
	name := fallback(event.Hypocenter, "地震震中")
	values := url.Values{}
	values.Set("saddr", fmt.Sprintf("%.6f,%.6f", startLat, startLon))
	values.Set("daddr", fmt.Sprintf("%.6f,%.6f", endLat, endLon))
	values.Set("q", name)
	values.Set("dirflg", "d")
	return "https://maps.apple.com/?" + values.Encode()
}

func wgs84ToGCJ02(lat, lon float64) (float64, float64) {
	if outOfChina(lat, lon) {
		return lat, lon
	}
	const a = 6378245.0
	const ee = 0.00669342162296594323
	dLat := transformLat(lon-105.0, lat-35.0)
	dLon := transformLon(lon-105.0, lat-35.0)
	radLat := lat / 180.0 * math.Pi
	magic := math.Sin(radLat)
	magic = 1 - ee*magic*magic
	sqrtMagic := math.Sqrt(magic)
	dLat = (dLat * 180.0) / ((a * (1 - ee)) / (magic * sqrtMagic) * math.Pi)
	dLon = (dLon * 180.0) / (a / sqrtMagic * math.Cos(radLat) * math.Pi)
	return lat + dLat, lon + dLon
}

func gcj02ToWGS84(lat, lon float64) (float64, float64) {
	if outOfChina(lat, lon) {
		return lat, lon
	}
	wgsLat, wgsLon := lat, lon
	for i := 0; i < 3; i++ {
		gcjLat, gcjLon := wgs84ToGCJ02(wgsLat, wgsLon)
		wgsLat -= gcjLat - lat
		wgsLon -= gcjLon - lon
	}
	return wgsLat, wgsLon
}

func outOfChina(lat, lon float64) bool {
	return lon < 72.004 || lon > 137.8347 || lat < 0.8293 || lat > 55.8271
}

func transformLat(x, y float64) float64 {
	ret := -100.0 + 2.0*x + 3.0*y + 0.2*y*y + 0.1*x*y + 0.2*math.Sqrt(math.Abs(x))
	ret += (20.0*math.Sin(6.0*x*math.Pi) + 20.0*math.Sin(2.0*x*math.Pi)) * 2.0 / 3.0
	ret += (20.0*math.Sin(y*math.Pi) + 40.0*math.Sin(y/3.0*math.Pi)) * 2.0 / 3.0
	ret += (160.0*math.Sin(y/12.0*math.Pi) + 320*math.Sin(y*math.Pi/30.0)) * 2.0 / 3.0
	return ret
}

func transformLon(x, y float64) float64 {
	ret := 300.0 + x + 2.0*y + 0.1*x*x + 0.1*x*y + 0.1*math.Sqrt(math.Abs(x))
	ret += (20.0*math.Sin(6.0*x*math.Pi) + 20.0*math.Sin(2.0*x*math.Pi)) * 2.0 / 3.0
	ret += (20.0*math.Sin(x*math.Pi) + 40.0*math.Sin(x/3.0*math.Pi)) * 2.0 / 3.0
	ret += (150.0*math.Sin(x/12.0*math.Pi) + 300.0*math.Sin(x/30.0*math.Pi)) * 2.0 / 3.0
	return ret
}

func estimateIntensity(magnitude, distanceKM float64) int {
	if magnitude <= 0 || distanceKM < 0 {
		return 0
	}
	if distanceKM < 1 {
		return clampInt(int(math.Round(magnitude*1.5-2.5)), 0, 7)
	}
	a, b, c, d := intensityCoefficients(magnitude)
	value := a*magnitude - b*math.Log10(distanceKM+c) + d
	return clampInt(int(math.Round(value)), 0, 7)
}

func intensityCoefficients(magnitude float64) (float64, float64, float64, float64) {
	type coefficients struct {
		a, b, c, d float64
	}
	small := coefficients{2.5, 3.8, 12.0, -1.2}
	medium := coefficients{2.5, 3.6, 10.0, -1.3}
	strong := coefficients{2.3, 3.7, 10.0, -1.0}
	major := coefficients{2.0, 3.8, 10.0, -0.8}

	switch {
	case magnitude < 4.8:
		return small.a, small.b, small.c, small.d
	case magnitude < 5.2:
		return blendCoefficients(small, medium, (magnitude-4.8)/0.4)
	case magnitude < 5.8:
		return medium.a, medium.b, medium.c, medium.d
	case magnitude < 6.2:
		return blendCoefficients(medium, strong, (magnitude-5.8)/0.4)
	case magnitude < 6.8:
		return strong.a, strong.b, strong.c, strong.d
	case magnitude < 7.2:
		return blendCoefficients(strong, major, (magnitude-6.8)/0.4)
	default:
		return major.a, major.b, major.c, major.d
	}
}

func blendCoefficients(a, b struct{ a, b, c, d float64 }, t float64) (float64, float64, float64, float64) {
	return lerp(a.a, b.a, t), lerp(a.b, b.b, t), lerp(a.c, b.c, t), lerp(a.d, b.d, t)
}

func lerp(a, b, t float64) float64 {
	return a + (b-a)*t
}

func round1(value float64) float64 {
	return math.Round(value*10) / 10
}

func parseTime(s string) time.Time {
	return parseTimeInZone(s, 8*3600)
}

func parseTimeInZone(s string, zoneOffsetSeconds int) time.Time {
	s = strings.TrimSpace(s)
	if s == "" {
		return time.Time{}
	}
	layouts := []string{
		"2006-01-02T15:04:05",
		"2006-01-02 15:04:05",
		"2006/01/02 15:04:05",
		"2006-01-02 15:04",
		"2006/01/02 15:04",
	}
	if t, err := time.Parse(time.RFC3339Nano, s); err == nil {
		return t
	}
	loc := time.FixedZone("WolfxLocal", zoneOffsetSeconds)
	for _, layout := range layouts {
		if t, err := time.ParseInLocation(layout, s, loc); err == nil {
			return t
		}
	}
	return time.Time{}
}

func getString(raw RawEvent, key string) string {
	v, ok := raw[key]
	if !ok || v == nil {
		return ""
	}
	if s, ok := v.(string); ok {
		return s
	}
	return fmt.Sprint(v)
}

func firstString(raw RawEvent, keys ...string) string {
	for _, key := range keys {
		if value := strings.TrimSpace(getString(raw, key)); value != "" {
			return value
		}
	}
	return ""
}

func firstValueString(raw RawEvent, keys ...string) string {
	for _, key := range keys {
		if value := strings.TrimSpace(getString(raw, key)); value != "" {
			return value
		}
	}
	return ""
}

func firstFloat(raw RawEvent, keys ...string) float64 {
	for _, key := range keys {
		v, ok := raw[key]
		if !ok || v == nil {
			continue
		}
		switch typed := v.(type) {
		case float64:
			return typed
		case string:
			if f, err := strconv.ParseFloat(strings.TrimSpace(typed), 64); err == nil {
				return f
			}
		}
	}
	return 0
}

func firstInt(raw RawEvent, keys ...string) int {
	for _, key := range keys {
		v, ok := raw[key]
		if !ok || v == nil {
			continue
		}
		switch typed := v.(type) {
		case float64:
			return int(typed)
		case string:
			if i, err := strconv.Atoi(strings.TrimSpace(typed)); err == nil {
				return i
			}
		}
	}
	return 0
}

func firstBool(raw RawEvent, keys ...string) bool {
	for _, key := range keys {
		v, ok := raw[key]
		if !ok || v == nil {
			continue
		}
		switch typed := v.(type) {
		case bool:
			return typed
		case string:
			value := strings.ToLower(strings.TrimSpace(typed))
			return value == "true" || value == "1" || value == "yes"
		case float64:
			return typed != 0
		}
	}
	return false
}

func validCoordinate(lat, lon float64) bool {
	return lat >= -90 && lat <= 90 && lon >= -180 && lon <= 180
}

func clampInt(value, minValue, maxValue int) int {
	if value < minValue {
		return minValue
	}
	if value > maxValue {
		return maxValue
	}
	return value
}

func absInt(value int) int {
	if value < 0 {
		return -value
	}
	return value
}

func fallback(value, defaultValue string) string {
	if strings.TrimSpace(value) == "" {
		return defaultValue
	}
	return value
}

func maskKey(value string) string {
	value = strings.TrimSpace(value)
	if len(value) <= 6 {
		return "***"
	}
	return value[:3] + "***" + value[len(value)-3:]
}

func compact(data []byte, max int) string {
	s := strings.Join(strings.Fields(string(data)), " ")
	if len(s) <= max {
		return s
	}
	return s[:max] + "..."
}

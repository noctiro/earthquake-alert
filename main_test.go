package main

import (
	"context"
	"fmt"
	"math"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestParseCencEvent(t *testing.T) {
	payload := []byte(`{
		"type": "cenc_eew",
		"EventID": "202607070001",
		"ReportNum": 2,
		"OriginTime": "2026-07-07 09:30:00",
		"HypoCenter": "四川阿坝",
		"Latitude": 31.9,
		"Longitude": 102.2,
		"Magnitude": 5.8,
		"Depth": 10,
		"MaxIntensity": 6
	}`)

	event, ok, err := parseEvent(payload)
	if err != nil {
		t.Fatal(err)
	}
	if !ok {
		t.Fatal("expected EEW event")
	}
	if event.Type != "cenc_eew" || event.EventID != "202607070001" || event.ReportNum != 2 {
		t.Fatalf("unexpected identity: %#v", event)
	}
	if event.Hypocenter != "四川阿坝" || event.Magnitude != 5.8 || event.DepthKM != 10 {
		t.Fatalf("unexpected event fields: %#v", event)
	}
	if event.MaxIntensity != "6" {
		t.Fatalf("unexpected max intensity: %q", event.MaxIntensity)
	}
}

func TestEvaluateETA(t *testing.T) {
	cfg := Config{
		Alert: AlertConfig{
			SWaveKMS: 3.5,
			PWaveKMS: 6.0,
		},
	}
	sub := Subscription{Latitude: 31.2304, Longitude: 121.4737}
	event := Event{
		Type:      "cenc_eew",
		EventID:   "x",
		Latitude:  31.2304,
		Longitude: 121.5737,
		Magnitude: 5.0,
		DepthKM:   10,
	}
	decision := evaluate(cfg, sub, event)
	if decision.DistanceKM <= 0 || decision.HypocentralKM <= decision.DistanceKM {
		t.Fatalf("bad distances: %#v", decision)
	}
	if decision.SArrival.Before(decision.PArrival) {
		t.Fatalf("S wave should not arrive before P wave: %#v", decision)
	}
}

func TestRegionalTravelTimeInterpolation(t *testing.T) {
	cfg := Config{Alert: AlertConfig{PWaveKMS: 6.0, SWaveKMS: 3.5}}
	p, s := seismicTravelSeconds(cfg, 1000, 15)
	fixedP, fixedS := fixedWaveTravelSeconds(cfg, math.Sqrt(1000*1000+15*15))
	if p >= fixedP || s >= fixedS {
		t.Fatalf("regional table should be faster than fixed crustal speed at 1000km, got p=%.1f s=%.1f fixedP=%.1f fixedS=%.1f", p, s, fixedP, fixedS)
	}
	if s <= p {
		t.Fatalf("S arrival must be after P arrival, p=%.1f s=%.1f", p, s)
	}
}

func TestEstimateIntensitySmoothsMagnitudeBoundary(t *testing.T) {
	if got := estimateIntensity(5.1, 280); got != 2 {
		t.Fatalf("expected M5.1 at 280km to stay at intensity 2 after coefficient smoothing, got %d", got)
	}
	if got := estimateIntensity(4.8, 280); got != 1 {
		t.Fatalf("expected M4.8 at 280km to remain intensity 1, got %d", got)
	}

	prev := estimateIntensity(4.9, 240)
	next := estimateIntensity(5.0, 240)
	if next-prev > 1 {
		t.Fatalf("unexpected M5.0 boundary jump at 240km: M4.9=%d M5.0=%d", prev, next)
	}
}

func TestWriteDeliveryAudit(t *testing.T) {
	dir := t.TempDir()
	cfg := Config{Server: ServerConfig{AuditPath: dir}, Bark: BarkConfig{Server: "https://api.day.app"}}
	event := Event{EventID: "test/event", ReportNum: 1, Type: "cenc_eew", OriginTime: time.Now(), Magnitude: 5.1}
	sub := Subscription{BarkID: "secretKey", BarkServer: "https://api.day.app", Latitude: 30.5, Longitude: 104.1}
	decision := Decision{EstimatedIntensity: 2, DistanceKM: 120.4, HypocentralKM: 121, SecondsToS: 20}
	started := time.Now()
	record := deliveryAuditRecordForTarget(cfg, event, sub, decision, started, started, "pushed", "", "active", 150*time.Millisecond, nil)
	if strings.Contains(record.BarkMasked, "secretKey") || record.BarkHash == "" {
		t.Fatalf("audit record should mask and hash bark key: %#v", record)
	}
	if err := writeDeliveryAudit(cfg, event, started, started, started.Add(time.Second), 1, 0, 1, 0, 1, []deliveryAuditRecord{record}); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(dir, "test_event-r1-cenc_eew.jsonl")); err != nil {
		t.Fatalf("expected detail audit file: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "test_event-r1-cenc_eew.summary.json")); err != nil {
		t.Fatalf("expected summary audit file: %v", err)
	}
}

func TestTravelTimeFallbackForTableOutOfRange(t *testing.T) {
	cfg := Config{Alert: AlertConfig{PWaveKMS: 6.0, SWaveKMS: 3.5}}
	p, s := seismicTravelSeconds(cfg, 5000, 10)
	fixedP, fixedS := fixedWaveTravelSeconds(cfg, math.Sqrt(5000*5000+10*10))
	if p != fixedP || s != fixedS {
		t.Fatalf("expected fixed-speed fallback outside table, got p=%.3f s=%.3f fixedP=%.3f fixedS=%.3f", p, s, fixedP, fixedS)
	}
}

func TestParseJMAEventUsesJST(t *testing.T) {
	payload := []byte(`{
		"type": "jma_eew",
		"EventID": "202607070001",
		"Serial": 1,
		"OriginTime": "2026-07-07 09:30:00",
		"Hypocenter": "岩手県沖",
		"Latitude": 40.1,
		"Longitude": 142.5,
		"Magunitude": 4.6,
		"Depth": 40,
		"MaxIntensity": "2"
	}`)

	event, ok, err := parseEvent(payload)
	if err != nil {
		t.Fatal(err)
	}
	if !ok {
		t.Fatal("expected EEW event")
	}
	want := time.Date(2026, 7, 7, 0, 30, 0, 0, time.UTC)
	if !event.OriginTime.Equal(want) {
		t.Fatalf("expected JST origin to equal %s, got %s", want, event.OriginTime)
	}
}

func TestHistoryRecordFromRaw(t *testing.T) {
	record, ok := historyRecordFromRaw("jma", "No1", RawEvent{
		"EventID":   "20260707061650",
		"time_full": "2026/07/07 06:16:50",
		"location":  "島根県西部",
		"magnitude": "3.2",
		"shindo":    "1",
		"depth":     "10km",
		"latitude":  "34.7",
		"longitude": "132.0",
	})
	if !ok {
		t.Fatal("expected valid history record")
	}
	if record.Source != "jma" || record.Key != "No1" || record.Hypocenter != "島根県西部" {
		t.Fatalf("unexpected identity: %#v", record)
	}
	if record.Magnitude != 3.2 || record.DepthKM != 10 || record.MaxIntensity != "1" {
		t.Fatalf("unexpected values: %#v", record)
	}
}

func TestSimulationAndHistoryPreviewsUseSubscriberLocation(t *testing.T) {
	cfg := Config{Alert: AlertConfig{SWaveKMS: 3.5, PWaveKMS: 6.0}}
	sub := Subscription{Latitude: 31.2304, Longitude: 121.4737}
	normalizeSubscription(&sub)

	previews := simulationPreviews(cfg, sub)
	if len(previews) != 3 {
		t.Fatalf("expected 3 simulation previews, got %d", len(previews))
	}
	for _, preview := range previews {
		if preview.Kind == "tiny" || preview.DistanceKM <= 0 || preview.EstimatedIntensity < 0 || preview.NotifyLevel == "" {
			t.Fatalf("bad preview: %#v", preview)
		}
	}

	records := annotateHistoryRecords(cfg, sub, []HistoryRecord{{
		Source:     "cenc",
		Key:        "No1",
		EventID:    "x",
		Hypocenter: "nearby",
		Latitude:   31.3,
		Longitude:  121.5,
		Magnitude:  4.5,
		DepthKM:    10,
	}})
	if len(records) != 1 || records[0].DistanceKM <= 0 || records[0].HypocentralKM <= records[0].DistanceKM {
		t.Fatalf("bad annotated history record: %#v", records)
	}
}

func TestNearestSubscriptionLocationForEvent(t *testing.T) {
	cfg := Config{Alert: AlertConfig{SWaveKMS: 3.5, PWaveKMS: 6.0}}
	sub := Subscription{
		BarkID:    "test",
		Latitude:  30.0,
		Longitude: 104.0,
		Locations: []SubscriptionLocation{
			{Name: "成都", Latitude: 30.0, Longitude: 104.0},
			{Name: "唐山", Latitude: 39.6, Longitude: 118.0},
		},
	}
	event := Event{
		Type:       "cenc_eew",
		EventID:    "near-tangshan",
		OriginTime: time.Now(),
		Hypocenter: "河北唐山",
		Latitude:   39.57,
		Longitude:  117.98,
		Magnitude:  5.0,
		DepthKM:    10,
	}
	selected, decision := nearestSubscriptionForEvent(cfg, sub, event)
	if selected.LocationName != "唐山" {
		t.Fatalf("expected nearest location Tangshan, got %#v", selected)
	}
	if decision.DistanceKM > 10 {
		t.Fatalf("expected near distance for selected location, got %.2fkm", decision.DistanceKM)
	}
}

func TestNotificationRules(t *testing.T) {
	sub := Subscription{}
	normalizeSubscription(&sub)
	if sub.NotifyRules != (NotificationRules{PassiveMax: 1, ActiveMax: 2, CriticalMin: 3}) {
		t.Fatalf("unexpected default rules: %#v", sub.NotifyRules)
	}
	if notifyLevelForIntensity(sub, 0) != "" || notifyLevelForIntensity(sub, 1) != "passive" || notifyLevelForIntensity(sub, 2) != "active" || notifyLevelForIntensity(sub, 3) != "critical" {
		t.Fatalf("unexpected level mapping")
	}
	if err := validateNotificationRules(NotificationRules{PassiveMax: 1, ActiveMax: 3, CriticalMin: 4}); err != nil {
		t.Fatalf("expected active range to be valid: %v", err)
	}
	sub.NotifyRules = NotificationRules{PassiveMax: 1, ActiveMax: 3, CriticalMin: 4}
	sub.NotifyBands = nil
	if notifyLevelForIntensity(sub, 3) != "active" || notifyLevelForIntensity(sub, 4) != "critical" {
		t.Fatalf("unexpected ranged level mapping")
	}
	if err := validateNotificationRules(NotificationRules{PassiveMax: 1, ActiveMax: 3, CriticalMin: 5}); err == nil {
		t.Fatal("expected non-contiguous critical range to fail")
	}
}

func TestNotificationBandsAllowDeletedRanges(t *testing.T) {
	sub := Subscription{NotifyBands: []NotificationBand{{Min: 3, Max: notificationOpenEndedMax, Level: "critical", Label: "高烈度"}}}
	normalizeSubscription(&sub)
	if notifyLevelForIntensity(sub, 2) != "" {
		t.Fatalf("expected uncovered intensity to be filtered")
	}
	if notifyLevelForIntensity(sub, 3) != "critical" {
		t.Fatalf("expected critical band for intensity 3")
	}
	if notifyLevelForIntensity(sub, 8) != "critical" {
		t.Fatalf("expected open-ended critical band for intensity above 7")
	}
	if err := validateNotificationBands([]NotificationBand{{Min: 0, Max: 2, Level: "passive"}, {Min: 2, Max: 4, Level: "active"}}); err == nil {
		t.Fatal("expected overlapping bands to fail")
	}
	if err := validateNotificationBands([]NotificationBand{{Min: 0, Max: 1, Level: "passive"}, {Min: 2, Max: 2, Level: "passive"}}); err == nil {
		t.Fatal("expected duplicate notification level to fail")
	}
	if err := validateNotificationBands([]NotificationBand{{Min: 0, Max: 1, Level: "passive"}, {Min: 2, Max: 2, Level: "active"}, {Min: 3, Max: notificationOpenEndedMax, Level: "critical"}, {Min: 6, Max: 6, Level: "active"}}); err == nil {
		t.Fatal("expected more than three notification bands to fail")
	}
	if err := validateNotificationBands([]NotificationBand{{Min: 0, Max: notificationOpenEndedMax, Level: "passive"}}); err == nil {
		t.Fatal("expected open-ended non-critical band to fail")
	}
}

func TestNotificationBandsMayStartAtZero(t *testing.T) {
	sub := Subscription{
		NotifyBands: []NotificationBand{
			{Min: 0, Max: 1, Level: "passive", Label: "低烈度"},
			{Min: 2, Max: 2, Level: "active", Label: "中等烈度"},
			{Min: 3, Max: notificationOpenEndedMax, Level: "critical", Label: "高烈度"},
		},
	}
	normalizeSubscription(&sub)
	if notifyLevelForIntensity(sub, 0) != "passive" {
		t.Fatalf("expected intensity 0 to match passive notification band")
	}
}

func TestValidateSubscriptionRejectsMissingOrZeroLocation(t *testing.T) {
	base := Subscription{
		BarkID:      "validKey",
		BarkServer:  "https://api.day.app",
		NotifyRules: defaultNotificationRules(),
	}
	if err := validateSubscription(base); err == nil {
		t.Fatal("expected missing location to be rejected")
	}
	base.Latitude = 0
	base.Longitude = 0
	base.Locations = []SubscriptionLocation{{Name: "zero", Latitude: 0, Longitude: 0}}
	if err := validateSubscription(base); err == nil {
		t.Fatal("expected 0,0 location to be rejected")
	}
	base.Latitude = 30.5
	base.Longitude = 104.1
	base.Locations = []SubscriptionLocation{{Name: "成都", Latitude: 30.5, Longitude: 104.1}}
	if err := validateSubscription(base); err != nil {
		t.Fatalf("expected real location to be accepted: %v", err)
	}
}

func TestStoreSkipsZeroLocationSubscriptionsOnLoad(t *testing.T) {
	path := filepath.Join(t.TempDir(), "subscriptions.json")
	data := []byte(`[
		{"bark_id":"badKey","bark_server":"https://api.day.app","latitude":0,"longitude":0},
		{"bark_id":"goodKey","bark_server":"https://api.day.app","latitude":30.5,"longitude":104.1}
	]`)
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
	store, err := NewStore(path)
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := store.Get("badKey"); ok {
		t.Fatal("expected zero-location subscription to be skipped")
	}
	if _, ok := store.Get("goodKey"); !ok {
		t.Fatal("expected valid subscription to load")
	}
}

func TestFanoutConcurrencyAndPriority(t *testing.T) {
	cfg := Config{Alert: AlertConfig{FanoutConcurrency: 1000}}
	if got := officialFanoutConcurrency(cfg, 10); got != 10 {
		t.Fatalf("expected concurrency capped by target count, got %d", got)
	}
	if got := officialFanoutConcurrency(cfg, 700); got != 500 {
		t.Fatalf("expected official hard cap at 500, got %d", got)
	}
	cfg.Alert.FanoutConcurrency = 0
	if got := officialFanoutConcurrency(cfg, 700); got != 100 {
		t.Fatalf("expected official default concurrency 100, got %d", got)
	}
	if got := selfHostedFanoutConcurrency(Config{}, 700); got != 700 {
		t.Fatalf("expected self-hosted concurrency capped only by target count, got %d", got)
	}
	cfg.Alert.SelfHostedConcurrency = 1500
	if got := selfHostedFanoutConcurrency(cfg, 1200); got != 1200 {
		t.Fatalf("expected self-hosted concurrency to follow configured value without 500 cap, got %d", got)
	}
	if notifyPriority("critical") >= notifyPriority("active") || notifyPriority("active") >= notifyPriority("passive") {
		t.Fatalf("unexpected notify priorities")
	}
}

func TestBarkErrorGuardQuarantinesBadKey(t *testing.T) {
	guard := NewBarkErrorGuard(10, 2, time.Hour)
	now := time.Date(2026, 7, 8, 8, 0, 0, 0, time.UTC)
	if ok, _, _ := guard.Allow("bad-key", now); !ok {
		t.Fatal("new key should be allowed")
	}
	guard.Record("bad-key", &HTTPStatusError{StatusCode: http.StatusBadRequest, Body: "bad key"}, now)
	if ok, _, _ := guard.Allow("bad-key", now.Add(time.Second)); !ok {
		t.Fatal("key should not be quarantined before threshold")
	}
	guard.Record("bad-key", &HTTPStatusError{StatusCode: http.StatusNotFound, Body: "missing"}, now.Add(2*time.Second))
	if ok, reason, until := guard.Allow("bad-key", now.Add(3*time.Second)); ok || reason != "key_quarantined" || until.IsZero() {
		t.Fatalf("expected key quarantine, ok=%v reason=%q until=%s", ok, reason, until)
	}
	if ok, _, _ := guard.Allow("bad-key", now.Add(2*time.Hour)); !ok {
		t.Fatal("key should be allowed after quarantine expires")
	}
}

func TestBarkErrorGuardGlobalBudget(t *testing.T) {
	guard := NewBarkErrorGuard(2, 10, time.Hour)
	now := time.Date(2026, 7, 8, 8, 0, 0, 0, time.UTC)
	guard.Record("a", &HTTPStatusError{StatusCode: http.StatusInternalServerError, Body: "x"}, now)
	guard.Record("b", &HTTPStatusError{StatusCode: http.StatusBadRequest, Body: "x"}, now.Add(time.Second))
	if ok, reason, _ := guard.Allow("c", now.Add(2*time.Second)); ok || reason != "global_error_budget" {
		t.Fatalf("expected global budget stop, ok=%v reason=%q", ok, reason)
	}
	if ok, _, _ := guard.Allow("c", now.Add(6*time.Minute)); !ok {
		t.Fatal("global budget should reset after window")
	}
}

func TestNormalizeBarkIDInput(t *testing.T) {
	key, err := normalizeBarkIDInput("vRvm9tubpnHJYsX7fE2EYQ")
	if err != nil || key != "vRvm9tubpnHJYsX7fE2EYQ" {
		t.Fatalf("unexpected key normalization: key=%q err=%v", key, err)
	}
	key, err = normalizeBarkIDInput("https://api.day.app/vRvm9tubpnHJYsX7fE2EYQ/")
	if err != nil || key != "vRvm9tubpnHJYsX7fE2EYQ" {
		t.Fatalf("unexpected url normalization: key=%q err=%v", key, err)
	}
	if _, err := normalizeBarkIDInput("https://example.com/vRvm9tubpnHJYsX7fE2EYQ/"); err == nil {
		t.Fatal("expected non-Bark URL to fail")
	}
	if _, err := normalizeBarkIDInput("https://api.day.app/vRvm9tubpnHJYsX7fE2EYQ/bad path"); err != nil {
		t.Fatalf("extra URL path segments should be ignored after key extraction: %v", err)
	}
	key, err = normalizeBarkIDInput("https://api.day.app/vRvm9tubpnHJYsX7fE2EYQ/%E8%BF%99%E9%87%8C%E6%94%B9%E6%88%90%E4%BD%A0%E8%87%AA%E5%B7%B1%E7%9A%84%E6%8E%A8%E9%80%81%E5%86%85%E5%AE%B9")
	if err != nil || key != "vRvm9tubpnHJYsX7fE2EYQ" {
		t.Fatalf("unexpected url with push content normalization: key=%q err=%v", key, err)
	}
}

func TestSimulationPreviewsFollowNotificationBands(t *testing.T) {
	cfg := Config{Alert: AlertConfig{SWaveKMS: 3.5, PWaveKMS: 6.0}}
	sub := Subscription{
		Latitude:    31.2304,
		Longitude:   121.4737,
		NotifyRules: NotificationRules{PassiveMax: 1, ActiveMax: 2, CriticalMin: 3},
	}
	normalizeSubscription(&sub)

	got := map[string]SimulationPreview{}
	for _, preview := range simulationPreviews(cfg, sub) {
		got[preview.Kind] = preview
	}
	if got["small"].NotifyLevel != "passive" {
		t.Fatalf("small test should be passive, got %#v", got["small"])
	}
	if got["medium"].NotifyLevel != "active" {
		t.Fatalf("medium test should be active, got %#v", got["medium"])
	}
	if got["large"].NotifyLevel != "critical" {
		t.Fatalf("large test should be critical, got %#v", got["large"])
	}
}

func TestHistoricalAlertFormatsLikeRealEvent(t *testing.T) {
	record := HistoryRecord{
		Source:       "cenc",
		Key:          "No1",
		EventID:      "hist-1",
		OriginTime:   "2024-01-02 03:04:05",
		Hypocenter:   "测试震中",
		Latitude:     31.2,
		Longitude:    121.4,
		Magnitude:    5.1,
		DepthKM:      10,
		MaxIntensity: "5",
	}
	event := historicalEvent(record)
	if time.Since(event.OriginTime) > time.Second {
		t.Fatalf("historical replay origin should be current for countdown, got %s", event.OriginTime)
	}
	sub := Subscription{Latitude: 31.0, Longitude: 121.0}
	normalizeSubscription(&sub)
	decision := evaluate(Config{Alert: AlertConfig{SWaveKMS: 3.5, PWaveKMS: 6.0}}, sub, event)
	title, _, body := formatAlert(event, decision, sub)
	if strings.Contains(title, "历史") || strings.Contains(title, "测试") || strings.Contains(body, "历史测试") || strings.Contains(body, "复现") || strings.Contains(body, "[测试]") || strings.Contains(body, "不是真实地震") {
		t.Fatalf("historical alert should look like a real alert, title=%q body=%q", title, body)
	}
	if !strings.Contains(body, "来源: CENC 第1报") || !strings.Contains(body, "发震: 2024-01-02 03:04:05") || !strings.Contains(body, "预计: P波+") || !strings.Contains(body, "S波+") {
		t.Fatalf("historical alert missing real source/time, body=%q", body)
	}
}

func TestAlertBodyIncludesConfiguredIntensityBand(t *testing.T) {
	event := Event{Type: "cenc_eew", EventID: "x", Hypocenter: "测试震中", Latitude: 30, Longitude: 104, Magnitude: 5.0, DepthKM: 10, ReportNum: 1, OriginTime: time.Now()}
	decision := Decision{DistanceKM: 120, HypocentralKM: 121, EstimatedIntensity: 3, SecondsToP: 10, SecondsToS: 20}
	sub := Subscription{NotifyRules: NotificationRules{PassiveMax: 1, ActiveMax: 3, CriticalMin: 4}}
	_, _, body := formatAlert(event, decision, sub)
	if !strings.Contains(body, "级别: 中等烈度（active）") {
		t.Fatalf("expected active intensity band in alert body, got %q", body)
	}
}

func TestAlertPageIncludesSubscriptionActions(t *testing.T) {
	rec := httptest.NewRecorder()
	now := time.Now()
	renderAlertPage(rec, AlertPage{
		Event: Event{
			Type:       "cenc_eew",
			EventID:    "x",
			ReportNum:  1,
			OriginTime: now,
			Hypocenter: "测试震中",
			Latitude:   30,
			Longitude:  104,
			Magnitude:  5,
			DepthKM:    10,
		},
		Decision: Decision{
			DistanceKM:         100,
			HypocentralKM:      101,
			EstimatedIntensity: 2,
			PArrival:           now.Add(10 * time.Second),
			SArrival:           now.Add(20 * time.Second),
			SecondsToP:         10,
			SecondsToS:         20,
		},
		Subscriber: Subscription{BarkID: "abc-123", Latitude: 30.5, Longitude: 104.1},
		CreatedAt:  now,
		WeChatURL:  "https://example.com/wechat",
		MapURL:     "https://example.com/map",
	})
	body := rec.Body.String()
	if !strings.Contains(body, `href="/manage/abc-123"`) || !strings.Contains(body, "取消订阅") || !strings.Contains(body, "/api/unsubscribe/") {
		t.Fatalf("alert page missing subscription actions: %q", body)
	}
}

func TestHistoricalReplayBypassesRealtimeDistanceFilter(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()

	cfg := Config{
		Bark: BarkConfig{Server: server.URL, Group: "test"},
		Alert: AlertConfig{
			SWaveKMS:      3.5,
			PWaveKMS:      6.0,
			MaxDistanceKM: 1000,
		},
	}
	sub := Subscription{
		BarkID:    "testKey",
		Latitude:  30.5774,
		Longitude: 103.9625,
	}
	normalizeSubscription(&sub)

	event := historicalEvent(HistoryRecord{
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
	})
	decision := evaluate(cfg, sub, event)
	if decision.DistanceKM <= cfg.Alert.MaxDistanceKM {
		t.Fatalf("test setup should exceed distance filter: %#v", decision)
	}

	pushed, skipped := dispatchOne(context.Background(), cfg, NewNotifier(cfg.Bark), NewAlertCache(time.Hour), event, sub)
	if pushed != 1 || skipped != 0 {
		t.Fatalf("historical replay should bypass realtime filters, pushed=%d skipped=%d", pushed, skipped)
	}
}

func TestFormatBeijingTime(t *testing.T) {
	utc := time.Date(2026, 7, 7, 2, 3, 4, 0, time.UTC)
	if got := formatBeijing(utc, "2006-01-02 15:04:05"); got != "2026-07-07 10:03:04" {
		t.Fatalf("unexpected Beijing time: %s", got)
	}
	if got := alertOriginTimeLabel(Event{OriginTime: utc}); got != "2026-07-07 10:03:04" {
		t.Fatalf("unexpected alert origin time: %s", got)
	}
}

func TestGeocodeAmapConvertsGCJ02ToWGS84(t *testing.T) {
	wantLat, wantLon := 30.5728, 104.0668
	gcjLat, gcjLon := wgs84ToGCJ02(wantLat, wantLon)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("key") != "test-key" || r.URL.Query().Get("keywords") != "成都市天府广场" {
			t.Fatalf("unexpected amap request: %s", r.URL.String())
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(fmt.Sprintf(`{
			"status":"1",
			"info":"OK",
			"pois":[{
				"name":"天府广场",
				"address":"人民南路一段",
				"location":"%.6f,%.6f",
				"pname":"四川省",
				"cityname":"成都市",
				"adname":"青羊区"
			}]
		}`, gcjLon, gcjLat)))
	}))
	defer server.Close()

	results, err := geocodeAddress(context.Background(), Config{Server: ServerConfig{
		GeocodeProvider: "amap",
		AmapKey:         "test-key",
		AmapPlaceURL:    server.URL,
	}}, "成都市天府广场")
	if err != nil {
		t.Fatal(err)
	}
	if len(results) != 1 {
		t.Fatalf("expected one result, got %#v", results)
	}
	if results[0].Name != "天府广场" || !strings.Contains(results[0].Address, "成都市") {
		t.Fatalf("unexpected result labels: %#v", results[0])
	}
	if math.Abs(results[0].Latitude-wantLat) > 0.00001 || math.Abs(results[0].Longitude-wantLon) > 0.00001 {
		t.Fatalf("expected WGS84 %.6f,%.6f, got %.6f,%.6f", wantLat, wantLon, results[0].Latitude, results[0].Longitude)
	}
}

func TestReverseGeocodeAmapUsesWGS84Input(t *testing.T) {
	wantLat, wantLon := 30.5728, 104.0668
	gcjLat, gcjLon := wgs84ToGCJ02(wantLat, wantLon)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("key") != "test-key" {
			t.Fatalf("unexpected key: %s", r.URL.RawQuery)
		}
		location := r.URL.Query().Get("location")
		wantLocation := fmt.Sprintf("%.6f,%.6f", gcjLon, gcjLat)
		if location != wantLocation {
			t.Fatalf("expected GCJ02 location %s, got %s", wantLocation, location)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{
			"status":"1",
			"info":"OK",
			"regeocode":{
				"formatted_address":"四川省成都市武侯区桂溪街道天府大道北段",
				"addressComponent":{"province":"四川省","city":"成都市","district":"武侯区","township":"桂溪街道"}
			}
		}`))
	}))
	defer server.Close()

	result, err := reverseGeocodeAmap(context.Background(), Config{Server: ServerConfig{
		AmapKey:        "test-key",
		AmapReverseURL: server.URL,
	}}, wantLat, wantLon)
	if err != nil {
		t.Fatal(err)
	}
	if result.Name != "四川省 成都市 武侯区 桂溪街道 天府大道北段" || result.Latitude != wantLat || result.Longitude != wantLon {
		t.Fatalf("unexpected reverse result: %#v", result)
	}
}

func TestBuiltinHistoryAndFilters(t *testing.T) {
	records := builtinHistoryRecords()
	if len(records) < 2 {
		t.Fatalf("expected builtin major history records, got %d", len(records))
	}

	filtered := filterHistoryRecords(records, url.Values{
		"source":        []string{"major"},
		"min_magnitude": []string{"7"},
	})
	if len(filtered) != 2 || filtered[0].Key != "wenchuan-2008" {
		t.Fatalf("unexpected filtered records: %#v", filtered)
	}

	defaultFiltered := filterHistoryRecords(records, url.Values{})
	if len(defaultFiltered) != 0 {
		t.Fatalf("expected default history filter to hide major records, got %#v", defaultFiltered)
	}

	page := filterHistoryRecords(records, url.Values{
		"source": []string{"major"},
		"limit":  []string{"1"},
		"offset": []string{"1"},
	})
	if len(page) != 1 || page[0].Key != "tangshan-1976" {
		t.Fatalf("unexpected paged records: %#v", page)
	}
}

func TestMergeHistoryRecordsDedupes(t *testing.T) {
	a := HistoryRecord{Source: "cenc", Key: "No1", EventID: "a", Magnitude: 4}
	b := HistoryRecord{Source: "cenc", Key: "No1", EventID: "b", Magnitude: 5}
	c := HistoryRecord{Source: "major", Key: "wenchuan-2008", EventID: "c", Magnitude: 7.9}
	merged := mergeHistoryRecords([]HistoryRecord{a, c}, []HistoryRecord{b})
	if len(merged) != 2 {
		t.Fatalf("expected deduped records, got %#v", merged)
	}
	if merged[0].EventID != "a" {
		t.Fatalf("expected first record to win, got %#v", merged[0])
	}
}

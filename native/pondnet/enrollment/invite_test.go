package enrollment

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/json"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
	"time"
)

func refusal(s *Service, envelope Envelope, from string) (int, string) {
	data, _ := json.Marshal(envelope)
	r := httptest.NewRequest("POST", "/v1/household", bytes.NewReader(data))
	r.RemoteAddr = from
	w := httptest.NewRecorder()
	s.ServeHTTP(w, r)
	var body struct {
		Error string `json:"error"`
	}
	_ = json.Unmarshal(w.Body.Bytes(), &body)
	return w.Code, body.Error
}

func TestANewHouseholdNeedsAnInviteTheOperatorIssued(t *testing.T) {
	service, backend := emptyService(t)
	public, key, _ := ed25519.GenerateKey(rand.Reader)
	source := 0
	attempt := func(invite string) (int, string) {
		body := registration(public)
		body.Invite = invite
		envelope, _ := SignHousehold(body, key)
		source++
		return refusal(service, envelope, "198.51.100."+string(rune('0'+source))+":1")
	}
	for _, c := range []struct{ invite, want string }{
		{"", inviteRequired},
		{"not-an-invite", inviteInvalid},
		{InvitePrefix + strings.Repeat("0", 26), inviteInvalid},
	} {
		if code, reason := attempt(c.invite); code != 403 || reason != c.want {
			t.Fatalf("%q: %d %s, want 403 %s", c.invite, code, reason, c.want)
		}
	}
	if backend.created != 0 || len(service.Store.value.Households) != 0 {
		t.Fatal("a refused registration reached the coordinator or the store")
	}

	expired, _, _ := service.Store.IssueInvite(time.Second, time.Now().Add(-time.Hour))
	if code, reason := attempt(expired); code != 403 || reason != inviteExpired {
		t.Fatalf("expired invite: %d %s", code, reason)
	}

	invite, _, _ := service.Store.IssueInvite(time.Hour, time.Now())
	// As a person might type it back: lower case, no dashes, O for 0 and L for 1.
	body := strings.ToLower(strings.ReplaceAll(invite[len(InvitePrefix):], "-", ""))
	typed := InvitePrefix + strings.NewReplacer("0", "o", "1", "l").Replace(body)
	if code, _ := attempt(typed); code != 200 {
		t.Fatalf("a correctly typed invite was refused: %d", code)
	}
	// Retrying with the same invite after it was spent succeeds for the same household.
	if code, _ := attempt(invite); code != 200 {
		t.Fatalf("a retry with the spent invite was refused: %d", code)
	}

	other, otherKey, _ := ed25519.GenerateKey(rand.Reader)
	stranger := registration(other)
	stranger.Invite = invite
	envelope, _ := SignHousehold(stranger, otherKey)
	if code, reason := refusal(service, envelope, "198.51.100.99:1"); code != 403 || reason != inviteUsed {
		t.Fatalf("a spent invite admitted another household: %d %s", code, reason)
	}
}

func TestAnInviteIsSpentOnlyWithTheHousehold(t *testing.T) {
	service, backend := emptyService(t)
	public, key, _ := ed25519.GenerateKey(rand.Reader)
	body := invited(t, service, public)
	backend.fail = true
	envelope, _ := SignHousehold(body, key)
	if code, _ := refusal(service, envelope, "198.51.100.1:1"); code != 503 {
		t.Fatalf("expected the coordinator outage, got %d", code)
	}
	for _, issued := range service.Store.value.Invites {
		if issued.ConsumedBy != "" {
			t.Fatal("an invite was spent on a registration that did not happen")
		}
	}
	backend.fail = false
	envelope, _ = SignHousehold(body, key)
	if code, _ := refusal(service, envelope, "198.51.100.2:1"); code != 200 {
		t.Fatalf("the invite did not work after the outage: %d", code)
	}
}

func TestOnlyADigestOfAnInviteIsKept(t *testing.T) {
	service, _ := emptyService(t)
	invite, expires, err := service.Store.IssueInvite(DefaultInviteLifetime, time.Now())
	if err != nil || !strings.HasPrefix(invite, InvitePrefix) || time.Until(expires) < 6*24*time.Hour {
		t.Fatalf("issued %q until %v: %v", invite, expires, err)
	}
	canonical, _ := canonicalInvite(invite)
	state, err := os.ReadFile(service.Store.directory + "/state.json")
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Contains(state, []byte(canonical)) {
		t.Fatal("the invite itself was written to the state file")
	}
	for _, lifetime := range []time.Duration{0, MaxInviteLifetime + time.Hour} {
		if _, _, err := service.Store.IssueInvite(lifetime, time.Now()); err == nil {
			t.Fatalf("an invite lasting %v was issued", lifetime)
		}
	}
}

func TestTheAdminHandlerIssuesInvites(t *testing.T) {
	service, _ := emptyService(t)
	handler := service.AdminHandler()
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, httptest.NewRequest("POST", "/v1/invite?lifetime=72h", nil))
	var issued struct{ Invite, Expires string }
	if err := json.Unmarshal(w.Body.Bytes(), &issued); w.Code != 200 || err != nil {
		t.Fatalf("%d %s", w.Code, w.Body.String())
	}
	if _, ok := canonicalInvite(issued.Invite); !ok {
		t.Fatalf("issued an unreadable invite: %q", issued.Invite)
	}
	for _, target := range []string{"/v1/invite?lifetime=soon", "/v1/household"} {
		w := httptest.NewRecorder()
		handler.ServeHTTP(w, httptest.NewRequest("POST", target, nil))
		if w.Code == 200 {
			t.Fatalf("%s answered 200", target)
		}
	}
}

func TestCorruptInvitesFailClosed(t *testing.T) {
	v := state{Households: map[string]Household{}, Devices: map[string]map[string]Device{}, Requests: map[string]int64{}}
	v.Invites = map[string]Invite{"short": {Expires: 1}}
	if validateState(v) == nil {
		t.Fatal("a malformed invite digest was accepted")
	}
	v.Invites = map[string]Invite{strings.Repeat("a", 64): {Expires: 1, ConsumedBy: "nobody-registered-this"}}
	if validateState(v) == nil {
		t.Fatal("an invite spent by an unknown household was accepted")
	}
}

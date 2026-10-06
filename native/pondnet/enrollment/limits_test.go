package enrollment

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"strings"
	"testing"
	"time"

	"golang.org/x/time/rate"
)

func TestInventoryIsStreamedAndBoundedByCount(t *testing.T) {
	node := `{"id":"%d","nodeKey":"nodekey:x","user":{"id":"1"},"ipAddresses":["100.64.0.1"]}`
	document := func(count int) string {
		nodes := make([]string, count)
		for i := range nodes {
			nodes[i] = fmt.Sprintf(node, i+1)
		}
		return `{"extra":{"ignored":[1,2]},"nodes":[` + strings.Join(nodes, ",") + `]}`
	}
	// Well past the old 1 MiB body limit, and still read.
	large := document(4000)
	if len(large) < 1<<18 {
		t.Fatalf("fixture too small to matter: %d bytes", len(large))
	}
	nodes, err := decodeNodes(strings.NewReader(large), 4000)
	if err != nil || len(nodes) != 4000 || nodes[3999].ID != "4000" || nodes[0].UserID != "1" {
		t.Fatalf("decoded %d nodes: %v", len(nodes), err)
	}
	if _, err := decodeNodes(strings.NewReader(document(11)), 10); !errors.Is(err, ErrInventoryTooLarge) {
		t.Fatalf("an oversized inventory was accepted: %v", err)
	}
	for _, malformed := range []string{`[]`, `{"nodes":{}}`, `{"nodes":[{"id":1}]}`, `{"nodes":[`} {
		if _, err := decodeNodes(strings.NewReader(malformed), 10); err == nil {
			t.Fatalf("accepted %s", malformed)
		}
	}
}

func TestUserLookupsAreFilteredByTheCoordinator(t *testing.T) {
	var queries []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		queries = append(queries, r.URL.RawQuery)
		io.WriteString(w, `{"users":[{"id":"7","name":"household-0123456789abcdef0123456789abcdef"}]}`)
	}))
	defer server.Close()
	backend, err := NewHeadscale(server.URL, "fixture-credential")
	if err != nil {
		t.Fatal(err)
	}
	id, err := backend.EnsureUser(context.Background(), "household-0123456789abcdef0123456789abcdef")
	if err != nil || id != "7" {
		t.Fatalf("lookup: %q %v", id, err)
	}
	if len(queries) != 1 || queries[0] != "name=household-0123456789abcdef0123456789abcdef" {
		t.Fatalf("unfiltered user listing: %v", queries)
	}
}

func TestEnrollmentStopsAtCapacity(t *testing.T) {
	s, key, backend := fixture(t)
	if code := invoke(s, approval(), key); code != 200 {
		t.Fatalf("pond enrollment: %d", code)
	}
	// Other households fill the service; this one still has room of its own.
	s.Store.mu.Lock()
	for i := 1; i < EnrollmentCapacity; i++ {
		household := fmt.Sprintf("filler%010d", i/16)
		if s.Store.value.Devices[household] == nil {
			s.Store.value.Devices[household] = map[string]Device{}
		}
		s.Store.value.Devices[household][fmt.Sprintf("device%010d", i)] = Device{Role: "phone", Status: "active"}
	}
	s.Store.mu.Unlock()
	phone := approval()
	phone.Device, phone.Role, phone.Nonce = "phone00000000001", "phone", "nonce00000000002"
	phone.MachineKey = "mkey:" + strings.Repeat("c", 64)
	calls := backend.calls
	if code := invoke(s, phone, key); code != 503 {
		t.Fatalf("enrolled past capacity: %d", code)
	}
	if backend.calls != calls {
		t.Fatal("the coordinator was asked to register past capacity")
	}
}

func TestAnAuthenticApprovalIsSpentEvenWhenRefused(t *testing.T) {
	s, key, _ := fixture(t)
	refused := approval()
	refused.Role = "phone" // No active pond yet, so this is refused after authentication.
	if code := invoke(s, refused, key); code != 409 {
		t.Fatalf("expected a refusal, got %d", code)
	}
	if code := invoke(s, refused, key); code != 409 {
		t.Fatalf("replayed: %d", code)
	}
	s.Store.mu.Lock()
	_, spent := s.Store.value.Requests["household00000001:"+refused.Nonce]
	s.Store.mu.Unlock()
	if !spent {
		t.Fatal("the refused approval was not recorded as used")
	}
}

func TestClientAddressTrustsOnlyTheConfiguredProxy(t *testing.T) {
	s := &Service{TrustedProxies: []netip.Prefix{netip.MustParsePrefix("172.31.250.0/28")}}
	request := func(remote, forwarded string) *http.Request {
		r := httptest.NewRequest("POST", "/v1/household", nil)
		r.RemoteAddr = remote
		if forwarded != "" {
			r.Header.Set("X-Forwarded-For", forwarded)
		}
		return r
	}
	for _, c := range []struct{ remote, forwarded, want string }{
		{"172.31.250.2:5000", "198.51.100.7", "198.51.100.7"},
		// Only the hop the proxy saw counts; the rest is the client's own claim.
		{"172.31.250.2:5000", "10.0.0.1, 198.51.100.7", "198.51.100.7"},
		{"203.0.113.9:5000", "198.51.100.7", "203.0.113.9"},
		{"172.31.250.2:5000", "not-an-address", "172.31.250.2"},
		{"172.31.250.2:5000", "2001:db8:1:2:3:4:5:6", "2001:db8:1:2::/64"},
		{"[2001:db8:9:9::1]:5000", "", "2001:db8:9:9::/64"},
	} {
		if got := s.client(request(c.remote, c.forwarded)); got != c.want {
			t.Errorf("%s via %q: got %s, want %s", c.remote, c.forwarded, got, c.want)
		}
	}
}

func TestAFullLimiterRefusesNewKeysInsteadOfForgettingEveryone(t *testing.T) {
	limits := newSources(rate.Limit(1.0/60.0), 1)
	limits.maximum = 2
	if !limits.allow("a") || limits.allow("a") {
		t.Fatal("the first key's budget is wrong")
	}
	limits.allow("b")
	if limits.allow("c") {
		t.Fatal("a new key was admitted to a full table")
	}
	if limits.allow("a") {
		t.Fatal("a flood reset an exhausted budget")
	}
	limits.seen["b"].last = time.Now().Add(-2 * idle)
	if !limits.allow("c") {
		t.Fatal("an idle key was not evicted to make room")
	}
}

func TestAnUnreachableCoordinatorLeavesTheServiceDegradedNotDown(t *testing.T) {
	store, err := Open(t.TempDir() + "/state")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	backend := &unreachable{}
	s, err := New(context.Background(), store, backend)
	if err != nil {
		t.Fatalf("startup failed instead of degrading: %v", err)
	}
	health := func() string {
		w := httptest.NewRecorder()
		s.ServeHTTP(w, httptest.NewRequest("GET", "/health", nil))
		if w.Code != 200 {
			t.Fatalf("health %d", w.Code)
		}
		return w.Body.String()
	}
	if !strings.Contains(health(), `"degraded":true`) {
		t.Fatal("degraded state not reported")
	}
	backend.up = true
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if strings.Contains(health(), "degraded") {
		t.Fatal("still degraded after reconciling")
	}
}

type unreachable struct {
	fakeBackend
	up bool
}

func (u *unreachable) Inventory(ctx context.Context) ([]Registered, error) {
	if !u.up {
		return nil, errors.New("coordinator unavailable")
	}
	return u.fakeBackend.Inventory(ctx)
}

func TestPolicyIsTheSameForTheSameStore(t *testing.T) {
	s, _, backend := fixture(t)
	s.Store.mu.Lock()
	defer s.Store.mu.Unlock()
	for i := 0; i < 5; i++ {
		id := fmt.Sprintf("household%08d", i+2)
		s.Store.value.Households[id] = Household{UserID: "1", Port: 4443}
		s.Store.value.Devices[id] = map[string]Device{}
		for j, role := range []string{"pond", "phone", "phone", "phone"} {
			address := fmt.Sprintf("100.64.%d.%d", i, j+1)
			node := Registered{ID: fmt.Sprint(i*10 + j + 1), Key: "k" + address, UserID: "1", Addresses: []string{address}}
			backend.nodes = append(backend.nodes, node)
			s.Store.value.Devices[id][fmt.Sprintf("device%010d", j)] = Device{Role: role, Status: "active", NodeID: node.ID, Key: node.Key, Address: address}
		}
	}
	first := ""
	for i := 0; i < 20; i++ {
		if err := s.policy(context.Background()); err != nil {
			t.Fatal(err)
		}
		got := fmt.Sprint(backend.rules)
		if first == "" {
			first = got
		} else if got != first {
			t.Fatalf("policy changed between identical runs:\n%s\n%s", first, got)
		}
	}
	if len(backend.rules) != 5 {
		t.Fatalf("expected one rule per household, got %d", len(backend.rules))
	}
}

package enrollment

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestHeadscaleDistinguishesRejectedAndAmbiguousRegistration(t *testing.T) {
	for _, outcome := range []struct {
		name                 string
		lookup, registration int
		rejected             bool
	}{
		{"invalid_pending_id", 200, 400, true},
		{"conflict", 200, 409, true},
		{"rate_limit", 200, 429, true},
		{"lookup_unavailable_before_mutation", 503, 0, true},
		{"server_failure_after_mutation_may_be_ambiguous", 200, 503, false},
	} {
		t.Run(outcome.name, func(t *testing.T) {
			writes := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.URL.Path == "/api/v1/user" {
					w.WriteHeader(outcome.lookup)
					io.WriteString(w, `{"users":[{"id":"1","name":"fixture"}]}`)
					return
				}
				if r.Method != "POST" || r.URL.Path != "/api/v1/auth/register" {
					t.Error("unexpected coordinator request")
				}
				writes++
				w.WriteHeader(outcome.registration)
			}))
			defer server.Close()
			backend, err := NewHeadscale(server.URL, "fixture-credential")
			if err != nil {
				t.Fatal(err)
			}
			_, err = backend.Register(context.Background(), "1", "fixture-pending-auth")
			if err == nil || errors.Is(err, ErrRegistrationRejected) != outcome.rejected {
				t.Fatalf("incorrect recovery classification: %v", err)
			}
			if (outcome.lookup != 200 && writes != 0) || writes > 1 {
				t.Fatal("registration mutation retried or sent after failed lookup")
			}
		})
	}
}

// The coordinator receives exactly the policy document the service built, with
// the network-map cache attribute in Headscale's nodeAttrs form.
func TestHeadscalePolicyCarriesNodeAttributes(t *testing.T) {
	var sent string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "PUT" || r.URL.Path != "/api/v1/policy" {
			t.Errorf("unexpected coordinator request %s %s", r.Method, r.URL.Path)
		}
		var body struct {
			Policy string `json:"policy"`
		}
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			t.Error(err)
		}
		sent = body.Policy
	}))
	defer server.Close()
	backend, err := NewHeadscale(server.URL, "fixture-credential")
	if err != nil {
		t.Fatal(err)
	}
	rules := []Rule{{Action: "accept", Src: []string{"100.64.0.2"}, Dst: []string{"100.64.0.1:4443"}}}
	attrs := []NodeAttr{{Target: []string{"100.64.0.2"}, Attr: []string{cacheNetworkMaps}}}
	if err := backend.Policy(context.Background(), rules, attrs); err != nil {
		t.Fatal(err)
	}
	want := `{"acls":[{"action":"accept","src":["100.64.0.2"],"dst":["100.64.0.1:4443"]}],` +
		`"nodeAttrs":[{"target":["100.64.0.2"],"attr":["cache-network-maps"]}],"ssh":[],"tagOwners":{}}`
	if sent != want {
		t.Fatalf("policy\n got %s\nwant %s", sent, want)
	}
	if err := backend.Policy(context.Background(), nil, []NodeAttr{}); err != nil {
		t.Fatal(err)
	}
	if want = `{"acls":null,"nodeAttrs":[],"ssh":[],"tagOwners":{}}`; sent != want {
		t.Fatalf("policy\n got %s\nwant %s", sent, want)
	}
}

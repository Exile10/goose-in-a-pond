package enrollment

import (
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/netip"
	"slices"
	"strings"
	"sync"
	"time"

	"golang.org/x/time/rate"
)

// HouseholdRegistration is a household's first contact. It is signed by the key
// it registers, which is the only thing it proves.
//
// That is deliberate. Admission is not what keeps households apart: the policy
// does, granting each phone its own Pond's HTTPS port and nothing else, so a
// stranger who registers gains a tailnet address and no reach into anyone's
// home. Requiring an operator to provision every household instead would mean
// nobody could set up a Pond without us.
type HouseholdRegistration struct {
	PublicKey string `json:"publicKey"`
	Port      uint16 `json:"port"`
	Expires   int64  `json:"expires"`
}

// householdDomain separates these signatures from enrollment approvals, so a
// signature captured from one can never be presented as the other.
const householdDomain = "goose-household-v1\x00"

// maxHouseholds bounds what open admission can consume. Provision used to be the
// only admission control; without a ceiling a stranger could enumerate keys and
// fill the store and the coordinator's address space.
const maxHouseholds = 10000

// sources rate-limits by a key: a client address, or a household.
type sources struct {
	mu      sync.Mutex
	seen    map[string]*source
	every   rate.Limit
	burst   int
	maximum int
}

type source struct {
	limiter *rate.Limiter
	last    time.Time
}

// idle is how long an untouched key is kept; by then its bucket has refilled anyway.
const idle = 10 * time.Minute

func newSources(every rate.Limit, burst int) *sources {
	return &sources{seen: map[string]*source{}, every: every, burst: burst, maximum: 4096}
}

func (s *sources) allow(key string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := time.Now()
	entry, ok := s.seen[key]
	if !ok {
		if len(s.seen) >= s.maximum {
			for other, candidate := range s.seen {
				if now.Sub(candidate.last) > idle {
					delete(s.seen, other)
				}
			}
		}
		// Refuse a new key rather than forget every budget: forgetting is what let a
		// flood of addresses reset everyone's limit.
		if len(s.seen) >= s.maximum {
			return false
		}
		entry = &source{limiter: rate.NewLimiter(s.every, s.burst)}
		s.seen[key] = entry
	}
	entry.last = now
	return entry.limiter.Allow()
}

// client is the address a request came from. Behind a trusted proxy that is the last
// X-Forwarded-For hop, the one the proxy itself saw; anything earlier is the client's
// own claim. IPv6 clients are keyed by /64, which one subscriber typically holds whole.
func (s *Service) client(r *http.Request) string {
	address := remoteAddress(r.RemoteAddr)
	if address.IsValid() && slices.ContainsFunc(s.TrustedProxies, func(p netip.Prefix) bool { return p.Contains(address) }) {
		hops := strings.Split(r.Header.Get("X-Forwarded-For"), ",")
		if forwarded, err := netip.ParseAddr(strings.TrimSpace(hops[len(hops)-1])); err == nil {
			address = forwarded.Unmap()
		}
	}
	if !address.IsValid() {
		return r.RemoteAddr
	}
	if address.Is6() {
		prefix, _ := address.Prefix(64)
		return prefix.String()
	}
	return address.String()
}

func remoteAddress(remote string) netip.Addr {
	if port, err := netip.ParseAddrPort(remote); err == nil {
		return port.Addr().Unmap()
	}
	address, _ := netip.ParseAddr(remote)
	return address.Unmap()
}

// HouseholdID is the household's name for itself: a digest of its public key. The
// server derives it rather than trusting the caller, so a registration can only
// ever name the household whose key signed it.
func HouseholdID(public ed25519.PublicKey) string {
	digest := sha256.Sum256(public)
	return hex.EncodeToString(digest[:16])
}

// SignHousehold signs a first-contact registration with the key it registers.
func SignHousehold(registration HouseholdRegistration, key ed25519.PrivateKey) (Envelope, error) {
	payload, err := json.Marshal(registration)
	if err != nil {
		return Envelope{}, err
	}
	return Envelope{
		Payload:   base64.StdEncoding.EncodeToString(payload),
		Signature: base64.StdEncoding.EncodeToString(ed25519.Sign(key, append([]byte(householdDomain), payload...))),
	}, nil
}

func (s *Service) registerHousehold(w http.ResponseWriter, r *http.Request) {
	if !s.registrations.allow(s.client(r)) {
		w.Header().Set("Retry-After", "60")
		http.Error(w, `{"error":"rate_limited"}`, 429)
		return
	}
	select {
	case s.slots <- struct{}{}:
		defer func() { <-s.slots }()
	default:
		w.Header().Set("Retry-After", "2")
		http.Error(w, `{"error":"busy"}`, 429)
		return
	}
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, 8192))
	if err != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	var envelope Envelope
	var registration HouseholdRegistration
	if strict(body, &envelope) != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	payload, err := base64.StdEncoding.DecodeString(envelope.Payload)
	if err != nil || strict(payload, &registration) != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	signature, err := base64.StdEncoding.DecodeString(envelope.Signature)
	public, keyErr := base64.StdEncoding.DecodeString(registration.PublicKey)
	now := s.Now().Unix()
	if err != nil || keyErr != nil || len(public) != ed25519.PublicKeySize ||
		!ed25519.Verify(public, append([]byte(householdDomain), payload...), signature) ||
		registration.Expires <= now || registration.Expires > now+300 || registration.Port == 0 {
		http.Error(w, `{"error":"unauthorized"}`, 403)
		return
	}

	id := HouseholdID(public)
	ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
	defer cancel()

	s.Store.mu.Lock()
	defer s.Store.mu.Unlock()
	if s.Store.failure != nil {
		http.Error(w, `{"error":"storage_unavailable"}`, 503)
		return
	}
	if existing, ok := s.Store.value.Households[id]; ok {
		// Registering again is how a household recovers from a lost response, so
		// it answers rather than conflicting. The key cannot differ: the id is a
		// digest of it.
		if existing.PublicKey != registration.PublicKey {
			http.Error(w, `{"error":"unauthorized"}`, 403)
			return
		}
		writeHousehold(w, id)
		return
	}
	if len(s.Store.value.Households) >= maxHouseholds {
		w.Header().Set("Retry-After", "3600")
		http.Error(w, `{"error":"capacity"}`, 503)
		return
	}
	user, err := s.Backend.EnsureUser(ctx, "household-"+id)
	if err != nil || user == "" {
		http.Error(w, `{"error":"coordinator_unavailable"}`, 503)
		return
	}
	if err := s.Store.provisionLocked(id, Household{PublicKey: registration.PublicKey, UserID: user, Port: registration.Port}); err != nil {
		http.Error(w, `{"error":"registration_failed"}`, 409)
		return
	}
	writeHousehold(w, id)
}

func writeHousehold(w http.ResponseWriter, id string) {
	_ = json.NewEncoder(w).Encode(map[string]string{"household": id})
}

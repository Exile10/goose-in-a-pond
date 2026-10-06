package enrollment

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"strings"
	"time"
)

// Invites admit new households. Registration proves only that a Pond holds a key, and
// open admission let anyone consume the coordinator's households and addresses, so a
// household's first registration now also spends an invite the operator issued. Only a
// digest of each invite is stored.

// InvitePrefix begins every invite, so one is recognisable wherever it is pasted.
const InvitePrefix = "giap-inv1-"

// crockford is Crockford's base32: no I, L, O or U, so an invite survives being read aloud.
const crockford = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"

const (
	// DefaultInviteLifetime applies when the operator names none.
	DefaultInviteLifetime = 7 * 24 * time.Hour
	// MaxInviteLifetime bounds how long an unspent invite stays usable.
	MaxInviteLifetime = 30 * 24 * time.Hour
	// maxOutstandingInvites bounds the unspent invites held at once.
	maxOutstandingInvites = 1000
)

// Invite is one issued invite, keyed in the state by its digest.
type Invite struct {
	Expires    int64  `json:"expires"`
	ConsumedBy string `json:"consumedBy,omitempty"`
}

// The closed set of refusals a registration can meet over its invite.
const (
	inviteRequired = "invite_required"
	inviteInvalid  = "invite_invalid"
	inviteExpired  = "invite_expired"
	inviteUsed     = "invite_used"
)

// newInvite returns an invite for display and its canonical form.
func newInvite() (string, string, error) {
	random := make([]byte, 16)
	if _, err := rand.Read(random); err != nil {
		return "", "", err
	}
	canonical := encodeCrockford(random)
	groups := make([]string, 0, 7)
	for start := 0; start < len(canonical); start += 4 {
		groups = append(groups, canonical[start:min(start+4, len(canonical))])
	}
	return InvitePrefix + strings.Join(groups, "-"), canonical, nil
}

func encodeCrockford(data []byte) string {
	var out strings.Builder
	buffer, bits := 0, 0
	for _, b := range data {
		buffer = buffer<<8 | int(b)
		bits += 8
		for bits >= 5 {
			bits -= 5
			out.WriteByte(crockford[(buffer>>bits)&31])
		}
	}
	if bits > 0 {
		out.WriteByte(crockford[(buffer<<(5-bits))&31])
	}
	return out.String()
}

// canonicalInvite reads an invite as a person may have typed it: any case, with or
// without its dashes and spaces, and with the letters Crockford's alphabet maps.
func canonicalInvite(text string) (string, bool) {
	text = strings.TrimSpace(text)
	if len(text) < len(InvitePrefix) || !strings.EqualFold(text[:len(InvitePrefix)], InvitePrefix) || len(text) > 64 {
		return "", false
	}
	var out strings.Builder
	for _, r := range strings.ToUpper(text[len(InvitePrefix):]) {
		switch r {
		case '-', ' ':
			continue
		case 'I', 'L':
			r = '1'
		case 'O':
			r = '0'
		}
		if !strings.ContainsRune(crockford, r) {
			return "", false
		}
		out.WriteRune(r)
	}
	if out.Len() != 26 {
		return "", false
	}
	return out.String(), true
}

func inviteDigest(canonical string) string {
	digest := sha256.Sum256([]byte("goose-invite-v1\x00" + canonical))
	return hex.EncodeToString(digest[:])
}

// IssueInvite records a new invite valid for lifetime and returns it for display. Only
// its digest is kept, so it can be shown to the operator once and never again.
func (s *Store) IssueInvite(lifetime time.Duration, now time.Time) (string, time.Time, error) {
	if lifetime <= 0 || lifetime > MaxInviteLifetime {
		return "", time.Time{}, errors.New("invite lifetime must be between a second and 30 days")
	}
	display, canonical, err := newInvite()
	if err != nil {
		return "", time.Time{}, err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.failure != nil {
		return "", time.Time{}, s.failure
	}
	if s.value.Invites == nil {
		s.value.Invites = map[string]Invite{}
	}
	outstanding := 0
	for digest, invite := range s.value.Invites {
		if invite.ConsumedBy == "" && invite.Expires <= now.Unix() {
			delete(s.value.Invites, digest)
		} else if invite.ConsumedBy == "" {
			outstanding++
		}
	}
	if outstanding >= maxOutstandingInvites {
		return "", time.Time{}, errors.New("too many unspent invites; let some expire first")
	}
	expires := now.Add(lifetime)
	s.value.Invites[inviteDigest(canonical)] = Invite{Expires: expires.Unix()}
	if err = s.save(); err != nil {
		return "", time.Time{}, err
	}
	return display, expires, nil
}

// admitLocked decides whether invite admits household id, returning its digest or the
// refusal. An invite the same household already spent admits it again, so a registration
// whose response was lost can be retried.
func (s *Store) admitLocked(invite, id string, now int64) (string, string) {
	if invite == "" {
		return "", inviteRequired
	}
	canonical, ok := canonicalInvite(invite)
	if !ok {
		return "", inviteInvalid
	}
	digest := inviteDigest(canonical)
	issued, ok := s.value.Invites[digest]
	switch {
	case !ok:
		return "", inviteInvalid
	case issued.ConsumedBy == id:
		return digest, ""
	case issued.ConsumedBy != "":
		return "", inviteUsed
	case issued.Expires <= now:
		return "", inviteExpired
	}
	return digest, ""
}

// RevokeInvite withdraws an invite nobody has spent yet, so a code sent to the wrong
// person, or pasted somewhere it should not have been, admits nobody. One already spent
// cannot be withdrawn: the household it admitted holds its own key.
func (s *Store) RevokeInvite(text string) error {
	canonical, ok := canonicalInvite(text)
	if !ok {
		return errors.New("that is not an invite")
	}
	digest := inviteDigest(canonical)
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.failure != nil {
		return s.failure
	}
	invite, ok := s.value.Invites[digest]
	switch {
	case !ok:
		return errors.New("no such invite was issued, or it has already expired")
	case invite.ConsumedBy != "":
		return errors.New("that invite already admitted a household and cannot be withdrawn")
	}
	delete(s.value.Invites, digest)
	if err := s.save(); err != nil {
		s.value.Invites[digest] = invite
		return err
	}
	return nil
}

// AdminHandler issues and withdraws invites and revokes device certificates. Serve it
// only on the private administrative socket.
func (s *Service) AdminHandler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if r.Method == http.MethodPost && r.URL.Path == "/v1/invite/revoke" {
			if err := s.Store.RevokeInvite(r.URL.Query().Get("invite")); err != nil {
				slog.Warn("invite not withdrawn", "error", err)
				writeAdminError(w, err)
				return
			}
			slog.Info("invite withdrawn")
			json.NewEncoder(w).Encode(map[string]bool{"revoked": true})
			return
		}
		if r.Method == http.MethodPost && r.URL.Path == "/v1/device/revoke" {
			serial := r.URL.Query().Get("serial")
			if err := s.Store.RevokeDevice(serial, s.Now().Unix()); err != nil {
				slog.Warn("device certificate not revoked", "error", err)
				writeAdminError(w, err)
				return
			}
			slog.Info("device certificate revoked", "serial", serial)
			json.NewEncoder(w).Encode(map[string]bool{"revoked": true})
			return
		}
		if r.Method != http.MethodPost || r.URL.Path != "/v1/invite" {
			http.NotFound(w, r)
			return
		}
		lifetime := DefaultInviteLifetime
		if text := r.URL.Query().Get("lifetime"); text != "" {
			parsed, err := time.ParseDuration(text)
			if err != nil {
				http.Error(w, `{"error":"invalid_lifetime"}`, http.StatusBadRequest)
				return
			}
			lifetime = parsed
		}
		invite, expires, err := s.Store.IssueInvite(lifetime, s.Now())
		if err != nil {
			slog.Warn("invite not issued", "error", err)
			http.Error(w, `{"error":"invite_not_issued"}`, http.StatusConflict)
			return
		}
		slog.Info("invite issued", "expires", expires.UTC().Format(time.RFC3339))
		json.NewEncoder(w).Encode(map[string]string{"invite": invite, "expires": expires.UTC().Format(time.RFC3339)})
	})
}

// writeAdminError reports a refused administrative request with its reason. The socket
// is reachable only by the operator, so the reason is shown rather than closed.
func writeAdminError(w http.ResponseWriter, err error) {
	w.WriteHeader(http.StatusConflict)
	json.NewEncoder(w).Encode(map[string]string{"error": err.Error()})
}

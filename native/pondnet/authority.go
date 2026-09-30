package pondnet

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
	"golang.org/x/sys/unix"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// Authority is independent of the Pond TLS key and its WireGuard identity.
type Authority struct {
	Household string `json:"household"`
	PublicKey string `json:"publicKey"`
	key       ed25519.PrivateKey
}

// LoadAuthority creates only on explicit first activation; a corrupt identity never rotates itself.
func LoadAuthority(directory string) (Authority, error) {
	var out Authority
	if !filepath.IsAbs(directory) {
		return out, errors.New("authority directory must be absolute")
	}
	created := false
	if err := os.Mkdir(directory, 0700); err == nil {
		created = true
	} else if !os.IsExist(err) {
		return out, err
	}
	info, err := os.Lstat(directory)
	if err != nil {
		return out, err
	}
	if !info.IsDir() || info.Mode().Perm()&0077 != 0 {
		return out, errors.New("authority directory is not private")
	}
	fd, err := unix.Open(filepath.Join(directory, "identity.lock"), unix.O_CREAT|unix.O_RDWR|unix.O_NOFOLLOW, 0600)
	if err != nil {
		return out, err
	}
	lock := os.NewFile(uintptr(fd), "identity.lock")
	defer lock.Close()
	if err = unix.Flock(fd, unix.LOCK_EX|unix.LOCK_NB); err != nil {
		return out, err
	}
	path := filepath.Join(directory, "identity.json")
	var stored struct {
		Seed string `json:"seed"`
	}
	if created {
		seed := make([]byte, ed25519.SeedSize)
		if _, err = rand.Read(seed); err != nil {
			return out, err
		}
		stored.Seed = base64.StdEncoding.EncodeToString(seed)
		data, _ := json.Marshal(stored)
		f, e := os.CreateTemp(directory, ".identity-")
		if e != nil {
			return out, e
		}
		defer os.Remove(f.Name())
		_, err = f.Write(data)
		if err == nil {
			err = f.Sync()
		}
		closeErr := f.Close()
		if err == nil {
			err = closeErr
		}
		if err == nil {
			err = os.Rename(f.Name(), path)
		}
		if err != nil {
			return out, err
		}
		dir, e := os.Open(directory)
		if e != nil {
			return out, e
		}
		err = dir.Sync()
		dir.Close()
		if err != nil {
			return out, err
		}
	} else {
		info, err = os.Lstat(path)
		if err != nil {
			return out, err
		}
		if !info.Mode().IsRegular() || info.Mode().Perm()&0077 != 0 || info.Size() > 4096 {
			return out, errors.New("invalid authority identity")
		}
		data, e := os.ReadFile(path)
		if e != nil {
			return out, e
		}
		decoder := json.NewDecoder(bytes.NewReader(data))
		decoder.DisallowUnknownFields()
		if err = decoder.Decode(&stored); err != nil {
			return out, err
		}
		if decoder.Decode(new(any)) != io.EOF {
			return out, errors.New("trailing identity data")
		}
	}
	seed, err := base64.StdEncoding.DecodeString(stored.Seed)
	if err != nil || len(seed) != ed25519.SeedSize {
		return out, errors.New("invalid authority seed")
	}
	out.key = ed25519.NewKeyFromSeed(seed)
	public := out.key.Public().(ed25519.PublicKey)
	digest := sha256.Sum256(public)
	out.Household = hex.EncodeToString(digest[:16])
	out.PublicKey = base64.StdEncoding.EncodeToString(public)
	return out, nil
}

// Register introduces this household to the enrollment service, so that a
// household can be set up without an operator creating it by hand.
//
// It proves possession of the household key and nothing else. That is enough,
// because the coordinator's policy grants each phone its own Pond and nothing
// else, so a household that is not yours gives you no reach into one that is.
// The service names the household from the key rather than trusting what is
// sent, so this cannot claim another household.
//
// It is safe to repeat: the service answers with the same household rather than
// conflicting, which is how a lost response is recovered.
func (a Authority) Register(ctx context.Context, origin string, port uint16) (string, error) {
	u, e := url.Parse(origin)
	if e != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || (u.Path != "" && u.Path != "/") {
		return "", errors.New("enrollment requires an HTTPS origin")
	}
	if port == 0 {
		return "", errors.New("a companion port is required")
	}
	envelope, e := enrollment.SignHousehold(enrollment.HouseholdRegistration{
		PublicKey: a.PublicKey,
		Port:      port,
		Expires:   time.Now().Add(2 * time.Minute).Unix(),
	}, a.key)
	if e != nil {
		return "", e
	}
	body, _ := json.Marshal(envelope)
	request, e := http.NewRequestWithContext(ctx, "POST", strings.TrimSuffix(origin, "/")+"/v1/household", bytes.NewReader(body))
	if e != nil {
		return "", e
	}
	request.Header.Set("Content-Type", "application/json")
	response, e := coordinatorClient().Do(request)
	if e != nil {
		return "", fmt.Errorf("enrollment service unavailable: %w", e)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return "", &Refused{Status: response.StatusCode, Reason: refusal(response.Body), Action: "registration"}
	}
	var result struct {
		Household string `json:"household"`
	}
	if e = json.NewDecoder(io.LimitReader(response.Body, 4096)).Decode(&result); e != nil {
		return "", e
	}
	// The service must have named the household this key owns; anything else
	// means we are not talking to the coordinator we think we are.
	if result.Household != a.Household {
		return "", errors.New("coordinator named a different household")
	}
	return result.Household, nil
}

// Submit sends a narrowly scoped approval to the configured enrollment origin.
// Callers must authorize the device locally before invoking this operation.
func (a Authority) Submit(ctx context.Context, origin string, approval enrollment.Approval) (enrollment.Device, error) {
	var result enrollment.Device
	u, e := url.Parse(origin)
	if e != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || (u.Path != "" && u.Path != "/") {
		return result, errors.New("enrollment requires an HTTPS origin")
	}
	approval.Household = a.Household
	nonce := make([]byte, 24)
	if _, e = rand.Read(nonce); e != nil {
		return result, e
	}
	approval.Nonce = base64.RawURLEncoding.EncodeToString(nonce)
	approval.Expires = time.Now().Add(2 * time.Minute).Unix()
	envelope, e := enrollment.Sign(approval, a.key)
	if e != nil {
		return result, e
	}
	body, _ := json.Marshal(envelope)
	request, e := http.NewRequestWithContext(ctx, "POST", strings.TrimSuffix(origin, "/")+"/v1/approval", bytes.NewReader(body))
	if e != nil {
		return result, e
	}
	request.Header.Set("Content-Type", "application/json")
	response, e := coordinatorClient().Do(request)
	if e != nil {
		return result, fmt.Errorf("enrollment service unavailable: %w", e)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		// The status is the whole diagnosis: 401 and 403 mean the coordinator
		// rejected this household's signature, 404 that the route is not
		// deployed, 429 that it is shedding load, 5xx that it is unwell. Losing
		// it left "enrollment was not completed", which names the outcome
		// everybody already knew and none of the causes.
		return result, &Refused{Status: response.StatusCode, Reason: refusal(response.Body), Action: approval.Action}
	}
	decoder := json.NewDecoder(io.LimitReader(response.Body, 4096))
	decoder.DisallowUnknownFields()
	if e = decoder.Decode(&result); e != nil {
		return result, e
	}
	if decoder.Decode(new(any)) != io.EOF {
		return result, errors.New("coordinator answered with trailing data")
	}
	return result, answered(approval, result)
}

// answered checks the coordinator's account of the device against what was asked, so a
// proxy or a confused service cannot report a different device as this one's outcome.
func answered(approval enrollment.Approval, device enrollment.Device) error {
	known := map[string]bool{"pending": true, "active": true, "revoking": true, "revoked": true, "failed": true}
	if !known[device.Status] || (device.Role != "pond" && device.Role != "phone") {
		return errors.New("coordinator answered with an unknown device state")
	}
	if len(device.Revision) > 80 || strings.ContainsFunc(device.Revision, func(r rune) bool {
		return !(r >= 'A' && r <= 'Z' || r >= 'a' && r <= 'z' || r >= '0' && r <= '9' || r == '-' || r == '_')
	}) {
		return errors.New("coordinator answered with an invalid revision")
	}
	switch approval.Action {
	case "enroll", "replace":
		if device.Role != approval.Role || device.MachineKey != approval.MachineKey {
			return errors.New("coordinator answered for a different device")
		}
	case "revoke":
		if device.Status != "revoked" {
			return errors.New("coordinator did not confirm the revocation")
		}
	}
	return nil
}

// coordinatorClient ignores proxy settings in the environment: the helper talks to the
// coordinator directly or not at all. Redirects are not followed.
func coordinatorClient() *http.Client {
	return &http.Client{
		Timeout:       20 * time.Second,
		Transport:     &http.Transport{Proxy: nil, TLSHandshakeTimeout: 10 * time.Second, ForceAttemptHTTP2: true},
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
}

// refusal reads the coordinator's own account of why it said no.
//
// Its errors are a closed set of identifiers - approval_used,
// identity_already_enrolled, pond_registration_required and the rest - and each
// one calls for something different. The status alone cannot tell them apart:
// this service has six distinct 409s. Bounded, because it is a remote party's
// bytes going into a local log.
func refusal(body io.Reader) string {
	var answer struct {
		Error string `json:"error"`
	}
	raw, e := io.ReadAll(io.LimitReader(body, 512))
	if e != nil || len(raw) == 0 {
		return "with no explanation"
	}
	if json.Unmarshal(raw, &answer) == nil && answer.Error != "" {
		return answer.Error
	}
	// Not the shape we expect, so report that rather than nothing - a proxy
	// answering in place of the coordinator looks exactly like this.
	return "an unrecognised answer"
}

// Refused is a coordinator answer that is a decision, not a fault: the request
// reached it, it understood it, and it said no. The status and the service's
// own identifier are carried so a caller can act on WHICH no it was -- an
// already-enrolled device wants the replacement flow, an unreachable
// coordinator wants a retry, and telling a user the wrong one sends them to
// the wrong control.
type Refused struct {
	Status int
	Reason string
	Action string
}

func (r *Refused) Error() string {
	return fmt.Sprintf("the coordinator answered %d to %s: %s", r.Status, r.Action, r.Reason)
}

package enrollment

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// provisioned is one imaged Pond: its device key and the certificate an operator signed.
type provisioned struct {
	key         ed25519.PrivateKey
	certificate Envelope
	serial      string
}

func provision(t *testing.T, signer ed25519.PrivateKey) provisioned {
	t.Helper()
	public, key, _ := ed25519.GenerateKey(rand.Reader)
	serial := make([]byte, 16)
	rand.Read(serial)
	certificate, err := SignDeviceCertificate(DeviceCertificate{
		Version:         1,
		Serial:          hex.EncodeToString(serial),
		DevicePublicKey: base64.StdEncoding.EncodeToString(public),
		Issued:          time.Now().Unix(),
	}, signer)
	if err != nil {
		t.Fatal(err)
	}
	return provisioned{key: key, certificate: certificate, serial: hex.EncodeToString(serial)}
}

// withDevice is a registration for the household key, carrying device's proof.
func withDevice(public ed25519.PublicKey, device provisioned) HouseholdRegistration {
	body := registration(public)
	proof := ProveDevice(device.certificate, device.key, body.PublicKey, body.Expires)
	body.Device = &proof
	return body
}

var sourceCounter int

// nextSource is a fresh client address, so the per-source registration limit never
// decides a case these tests mean to decide on admission.
func nextSource() string {
	sourceCounter++
	return fmt.Sprintf("203.0.113.%d:1", sourceCounter%250+1)
}

func trustingService(t *testing.T) (*Service, *fakeBackend, ed25519.PrivateKey) {
	t.Helper()
	service, backend := emptyService(t)
	public, signer, _ := ed25519.GenerateKey(rand.Reader)
	service.ProvisioningKeys = []ed25519.PublicKey{public}
	return service, backend, signer
}

func TestAProvisionedPondRegistersItsHouseholdWithoutAnInvite(t *testing.T) {
	service, backend, signer := trustingService(t)
	device := provision(t, signer)
	public, key, _ := ed25519.GenerateKey(rand.Reader)

	envelope, _ := SignHousehold(withDevice(public, device), key)
	code, household := register(service, envelope, nextSource())
	if code != http.StatusOK || household != HouseholdID(public) {
		t.Fatalf("a provisioned Pond was not admitted: %d %q", code, household)
	}
	if backend.created != 1 {
		t.Fatalf("no coordinator user was created: %v", backend.created)
	}
	if service.Store.value.DeviceAdmissions[device.serial] != household {
		t.Fatal("the admission was not recorded against the certificate's serial")
	}
	// The same household registering again, as after a lost response, is answered.
	envelope, _ = SignHousehold(withDevice(public, device), key)
	if code, _ := register(service, envelope, nextSource()); code != http.StatusOK {
		t.Fatalf("a repeated registration was refused: %d", code)
	}
}

func TestADeviceCertificateIsRefusedWhenItCannotBeTrusted(t *testing.T) {
	service, _, signer := trustingService(t)
	_, stranger, _ := ed25519.GenerateKey(rand.Reader)
	other, _, _ := ed25519.GenerateKey(rand.Reader)

	// Each case registers its own household, so one wrongly admitted cannot make the
	// others look admitted through the already-registered path.
	cases := map[string]func(public ed25519.PublicKey) HouseholdRegistration{
		"signed by a key the service does not trust": func(public ed25519.PublicKey) HouseholdRegistration {
			return withDevice(public, provision(t, stranger))
		},
		"bound to a different household": func(public ed25519.PublicKey) HouseholdRegistration {
			device := provision(t, signer)
			body := registration(public)
			proof := ProveDevice(device.certificate, device.key, base64.StdEncoding.EncodeToString(other), body.Expires)
			body.Device = &proof
			return body
		},
		"bound by a key that is not the certificate's": func(public ed25519.PublicKey) HouseholdRegistration {
			device := provision(t, signer)
			_, impostor, _ := ed25519.GenerateKey(rand.Reader)
			body := registration(public)
			proof := ProveDevice(device.certificate, impostor, body.PublicKey, body.Expires)
			body.Device = &proof
			return body
		},
		"bound to another registration's expiry": func(public ed25519.PublicKey) HouseholdRegistration {
			device := provision(t, signer)
			body := registration(public)
			proof := ProveDevice(device.certificate, device.key, body.PublicKey, body.Expires+60)
			body.Device = &proof
			return body
		},
		"naming another device key than the one it was signed for": func(public ed25519.PublicKey) HouseholdRegistration {
			// A real certificate with an attacker's device key swapped in, not re-signed.
			device := provision(t, signer)
			swapped, attacker, _ := ed25519.GenerateKey(rand.Reader)
			read, _ := ReadDeviceCertificate(device.certificate)
			read.DevicePublicKey = base64.StdEncoding.EncodeToString(swapped)
			payload, _ := json.Marshal(read)
			forged := Envelope{Payload: base64.StdEncoding.EncodeToString(payload), Signature: device.certificate.Signature}
			body := registration(public)
			proof := ProveDevice(forged, attacker, body.PublicKey, body.Expires)
			body.Device = &proof
			return body
		},
	}
	for name, build := range cases {
		public, key, _ := ed25519.GenerateKey(rand.Reader)
		envelope, _ := SignHousehold(build(public), key)
		if code, reason := refusal(service, envelope, nextSource()); code != http.StatusForbidden || reason != deviceCertificateInvalid {
			t.Errorf("%s: got %d %q, want 403 %q", name, code, reason, deviceCertificateInvalid)
		}
	}
	if len(service.Store.value.Households) != 0 {
		t.Fatal("an untrusted certificate admitted a household")
	}
}

func TestOneCertificateAdmitsOneHousehold(t *testing.T) {
	service, _, signer := trustingService(t)
	device := provision(t, signer)
	first, firstKey, _ := ed25519.GenerateKey(rand.Reader)
	second, secondKey, _ := ed25519.GenerateKey(rand.Reader)

	envelope, _ := SignHousehold(withDevice(first, device), firstKey)
	if code, _ := register(service, envelope, nextSource()); code != http.StatusOK {
		t.Fatalf("the first household was refused: %d", code)
	}
	envelope, _ = SignHousehold(withDevice(second, device), secondKey)
	if code, reason := refusal(service, envelope, nextSource()); code != http.StatusForbidden || reason != deviceUsed {
		t.Fatalf("a used certificate admitted a second household: %d %q", code, reason)
	}
	// An operator's invite still admits that second household.
	body := withDevice(second, device)
	body.Invite = invited(t, service, second).Invite
	envelope, _ = SignHousehold(body, secondKey)
	if code, _ := register(service, envelope, nextSource()); code != http.StatusOK {
		t.Fatalf("an invite did not admit a household whose certificate was used: %d", code)
	}
}

func TestARevokedCertificateAdmitsNobody(t *testing.T) {
	service, _, signer := trustingService(t)
	device := provision(t, signer)
	if err := service.Store.RevokeDevice(device.serial, time.Now().Unix()); err != nil {
		t.Fatal(err)
	}
	public, key, _ := ed25519.GenerateKey(rand.Reader)
	envelope, _ := SignHousehold(withDevice(public, device), key)
	if code, reason := refusal(service, envelope, nextSource()); code != http.StatusForbidden || reason != deviceRevoked {
		t.Fatalf("a revoked certificate was not refused: %d %q", code, reason)
	}
}

func TestAServiceTrustingNoProvisioningKeyIgnoresCertificates(t *testing.T) {
	service, _ := emptyService(t)
	_, signer, _ := ed25519.GenerateKey(rand.Reader)
	public, key, _ := ed25519.GenerateKey(rand.Reader)
	envelope, _ := SignHousehold(withDevice(public, provision(t, signer)), key)
	if code, reason := refusal(service, envelope, nextSource()); code != http.StatusForbidden || reason != inviteRequired {
		t.Fatalf("got %d %q, want the invite path's %q", code, reason, inviteRequired)
	}
}

func TestACertificateIsSpentOnlyWithTheHousehold(t *testing.T) {
	service, _, signer := trustingService(t)
	device := provision(t, signer)
	public, key, _ := ed25519.GenerateKey(rand.Reader)
	// The household's state cannot be written, so it is not created and the certificate
	// must not be recorded as spent either.
	service.Store.directory = filepath.Join(t.TempDir(), "gone")
	envelope, _ := SignHousehold(withDevice(public, device), key)
	if code, _ := register(service, envelope, nextSource()); code != http.StatusConflict {
		t.Fatalf("an unwritable registration did not fail: %d", code)
	}
	if _, spent := service.Store.value.DeviceAdmissions[device.serial]; spent {
		t.Fatal("the certificate was recorded as spent without its household")
	}
}

func TestDeviceStateIsValidatedOnLoad(t *testing.T) {
	directory := filepath.Join(t.TempDir(), "state")
	store, err := Open(directory)
	if err != nil {
		t.Fatal(err)
	}
	store.Close()
	for name, state := range map[string]string{
		"an admission for no household": `{"households":{},"devices":{},"requests":{},"deviceAdmissions":{"` + strings.Repeat("a", 32) + `":"missing"}}`,
		"a malformed serial":            `{"households":{},"devices":{},"requests":{},"revokedDevices":{"short":1}}`,
	} {
		if err := os.WriteFile(filepath.Join(directory, "state.json"), []byte(state), 0600); err != nil {
			t.Fatal(err)
		}
		if reopened, err := Open(directory); err == nil {
			reopened.Close()
			t.Errorf("%s: corrupt device state was accepted", name)
		}
	}
}

func TestTheAdminHandlerRevokesDevicesAndWithdrawsInvites(t *testing.T) {
	service, _ := emptyService(t)
	admin := service.AdminHandler()
	post := func(path string) int {
		w := httptest.NewRecorder()
		admin.ServeHTTP(w, httptest.NewRequest("POST", path, nil))
		return w.Code
	}
	serial := strings.Repeat("ab", 16)
	if code := post("/v1/device/revoke?serial=" + serial); code != http.StatusOK {
		t.Fatalf("revoking a device answered %d", code)
	}
	if _, revoked := service.Store.value.RevokedDevices[serial]; !revoked {
		t.Fatal("the revocation was not recorded")
	}
	if code := post("/v1/device/revoke?serial=nope"); code != http.StatusConflict {
		t.Fatalf("a malformed serial answered %d", code)
	}

	invite, _, err := service.Store.IssueInvite(time.Hour, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	if code := post("/v1/invite/revoke?invite=" + invite); code != http.StatusOK {
		t.Fatalf("withdrawing an invite answered %d", code)
	}
	public, key, _ := ed25519.GenerateKey(rand.Reader)
	body := registration(public)
	body.Invite = invite
	envelope, _ := SignHousehold(body, key)
	if code, reason := refusal(service, envelope, nextSource()); code != http.StatusForbidden || reason != inviteInvalid {
		t.Fatalf("a withdrawn invite still admitted: %d %q", code, reason)
	}
	if code := post("/v1/invite/revoke?invite=" + invite); code != http.StatusConflict {
		t.Fatalf("withdrawing it twice answered %d", code)
	}
}

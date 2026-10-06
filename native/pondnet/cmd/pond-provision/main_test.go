package main

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
)

func TestAProvisioningKeySignsCertificatesTheServiceTrusts(t *testing.T) {
	key := filepath.Join(t.TempDir(), "provisioning.key")
	var out bytes.Buffer
	if err := run([]string{"keygen", "--key", key}, &out); err != nil {
		t.Fatal(err)
	}
	public := strings.SplitN(out.String(), "\n", 2)[0]
	if info, err := os.Stat(key); err != nil || info.Mode().Perm() != 0600 {
		t.Fatalf("the provisioning key is not private: %v %v", info.Mode(), err)
	}
	if err := run([]string{"keygen", "--key", key}, &bytes.Buffer{}); err == nil {
		t.Fatal("an existing provisioning key was overwritten")
	}
	out.Reset()
	if err := run([]string{"public", "--key", key}, &out); err != nil || strings.TrimSpace(out.String()) != public {
		t.Fatalf("public printed %q, keygen printed %q: %v", out.String(), public, err)
	}

	devicePublic, device, _ := ed25519.GenerateKey(rand.Reader)
	out.Reset()
	if err := run([]string{"sign", "--key", key, "--device-public-key", base64.StdEncoding.EncodeToString(devicePublic)}, &out); err != nil {
		t.Fatal(err)
	}
	var certificate enrollment.Envelope
	if err := json.Unmarshal(out.Bytes(), &certificate); err != nil {
		t.Fatal(err)
	}
	trusted, _ := base64.StdEncoding.DecodeString(public)
	household := base64.StdEncoding.EncodeToString(make([]byte, ed25519.PublicKeySize))
	expires := time.Now().Add(time.Minute).Unix()
	proof := enrollment.ProveDevice(certificate, device, household, expires)
	if serial, refusal := enrollment.VerifyDeviceProof(&proof, []ed25519.PublicKey{trusted}, household, expires); refusal != "" || !enrollment.ValidSerial(serial) {
		t.Fatalf("the service would refuse a certificate this tool signed: %q", refusal)
	}
}

func TestSigningRefusesWhatIsNotADeviceKey(t *testing.T) {
	key := filepath.Join(t.TempDir(), "provisioning.key")
	if err := run([]string{"keygen", "--key", key}, &bytes.Buffer{}); err != nil {
		t.Fatal(err)
	}
	if err := run([]string{"sign", "--key", key, "--device-public-key", "not-a-key"}, &bytes.Buffer{}); err == nil {
		t.Fatal("a certificate was signed for something that is not a device key")
	}
	if err := os.Chmod(key, 0644); err != nil {
		t.Fatal(err)
	}
	if err := run([]string{"public", "--key", key}, &bytes.Buffer{}); err == nil {
		t.Fatal("a provisioning key readable by others was used")
	}
}

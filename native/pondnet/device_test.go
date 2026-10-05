package pondnet

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
)

func signedFor(t *testing.T, signer ed25519.PrivateKey, devicePublic string) enrollment.Envelope {
	t.Helper()
	certificate, err := enrollment.SignDeviceCertificate(enrollment.DeviceCertificate{
		Version: 1, Serial: strings.Repeat("0f", 16), DevicePublicKey: devicePublic, Issued: time.Now().Unix(),
	}, signer)
	if err != nil {
		t.Fatal(err)
	}
	return certificate
}

func TestAPondThatWasNeverProvisionedHasNoDevice(t *testing.T) {
	device, err := LoadDevice(filepath.Join(t.TempDir(), "device"))
	if err != nil || device != nil {
		t.Fatalf("got %v, %v; want no device and no error", device, err)
	}
}

func TestADeviceKeyIsMadeOnceAndNeedsItsOwnCertificate(t *testing.T) {
	directory := filepath.Join(t.TempDir(), "device")
	public, err := CreateDevice(directory)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = CreateDevice(directory); err == nil {
		t.Fatal("an existing device key was replaced")
	}
	if _, err = LoadDevice(directory); err == nil {
		t.Fatal("a key without a certificate loaded as a provisioned device")
	}
	_, signer, _ := ed25519.GenerateKey(rand.Reader)
	stranger, _, _ := ed25519.GenerateKey(rand.Reader)
	if err = InstallDeviceCertificate(directory, signedFor(t, signer, base64.StdEncoding.EncodeToString(stranger))); err == nil {
		t.Fatal("a certificate for another device key was installed")
	}
	if err = InstallDeviceCertificate(directory, signedFor(t, signer, public)); err != nil {
		t.Fatal(err)
	}
	device, err := LoadDevice(directory)
	if err != nil || device == nil {
		t.Fatalf("a provisioned device did not load: %v", err)
	}
	for _, name := range []string{deviceKeyFile, deviceCertificateFile} {
		info, err := os.Stat(filepath.Join(directory, name))
		if err != nil || info.Mode().Perm() != 0600 {
			t.Fatalf("%s is not private: %v %v", name, info.Mode(), err)
		}
	}
}

func TestADeviceFileOthersCanReadIsRefused(t *testing.T) {
	directory := filepath.Join(t.TempDir(), "device")
	public, err := CreateDevice(directory)
	if err != nil {
		t.Fatal(err)
	}
	_, signer, _ := ed25519.GenerateKey(rand.Reader)
	if err = InstallDeviceCertificate(directory, signedFor(t, signer, public)); err != nil {
		t.Fatal(err)
	}
	if err = os.Chmod(filepath.Join(directory, deviceKeyFile), 0644); err != nil {
		t.Fatal(err)
	}
	if _, err = LoadDevice(directory); err == nil {
		t.Fatal("a device key readable by others was used")
	}
}

// Register sends a proof the service accepts: it is bound to this household and this
// registration, which is the whole of what makes it safe to send over the network.
func TestRegisterSendsAProofTheServiceAccepts(t *testing.T) {
	authority, err := LoadAuthority(filepath.Join(t.TempDir(), "authority"))
	if err != nil {
		t.Fatal(err)
	}
	directory := filepath.Join(t.TempDir(), "device")
	public, err := CreateDevice(directory)
	if err != nil {
		t.Fatal(err)
	}
	provisioningPublic, signer, _ := ed25519.GenerateKey(rand.Reader)
	if err = InstallDeviceCertificate(directory, signedFor(t, signer, public)); err != nil {
		t.Fatal(err)
	}
	device, err := LoadDevice(directory)
	if err != nil {
		t.Fatal(err)
	}

	sent := authority.registration(4443, "", device, time.Now())
	if sent.Device == nil {
		t.Fatal("a provisioned Pond registered without its device proof")
	}
	// The service's own check, over exactly what Register signs.
	serial, refusal := enrollment.VerifyDeviceProof(sent.Device, []ed25519.PublicKey{provisioningPublic}, sent.PublicKey, sent.Expires)
	if refusal != "" || serial == "" {
		t.Fatalf("the service would refuse the proof Register sends: %q", refusal)
	}
	if unprovisioned := authority.registration(4443, "", nil, time.Now()); unprovisioned.Device != nil {
		t.Fatal("a Pond that was never provisioned sent a device proof")
	}
}

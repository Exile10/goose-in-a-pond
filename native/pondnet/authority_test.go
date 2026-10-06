package pondnet

import (
	"crypto/ed25519"
	"encoding/base64"
	"encoding/json"
	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestAuthorityPersistenceAndSignature(t *testing.T) {
	path := filepath.Join(t.TempDir(), "authority")
	first, err := LoadAuthority(path)
	if err != nil {
		t.Fatal(err)
	}
	restored, err := LoadAuthority(path)
	if err != nil {
		t.Fatal(err)
	}
	if first.Household != restored.Household || first.PublicKey != restored.PublicKey {
		t.Fatal("authority changed after restart")
	}
	envelope, err := enrollment.Sign(enrollment.Approval{Household: first.Household, Device: "paired-device-0001", Action: "enroll"}, restored.key)
	if err != nil {
		t.Fatal(err)
	}
	payload, _ := base64.StdEncoding.DecodeString(envelope.Payload)
	signature, _ := base64.StdEncoding.DecodeString(envelope.Signature)
	public, _ := base64.StdEncoding.DecodeString(first.PublicKey)
	if !ed25519.Verify(public, append([]byte("goose-enrollment-v1\x00"), payload...), signature) {
		t.Fatal("restored authority cannot sign")
	}
	if ed25519.Verify(public, payload, signature) {
		t.Fatal("signature lacks domain separation")
	}
	exposed, _ := json.Marshal(first)
	var fields map[string]any
	json.Unmarshal(exposed, &fields)
	if len(fields) != 2 || fields["publicKey"] == nil || fields["household"] == nil {
		t.Fatal("private authority leaked")
	}
	info, _ := os.Stat(filepath.Join(path, "identity.json"))
	if info.Mode().Perm()&0077 != 0 {
		t.Fatal("identity is not private")
	}
}
func TestAuthorityRefusesDamagedState(t *testing.T) {
	for _, damage := range []string{"missing", "truncated", "permissive", "symlink", "trailing", "seed"} {
		t.Run(damage, func(t *testing.T) {
			dir := filepath.Join(t.TempDir(), "authority")
			if _, err := LoadAuthority(dir); err != nil {
				t.Fatal(err)
			}
			p := filepath.Join(dir, "identity.json")
			switch damage {
			case "missing":
				os.Remove(p)
			case "truncated":
				os.WriteFile(p, []byte("{"), 0600)
			case "permissive":
				os.Chmod(p, 0644)
			case "symlink":
				os.Rename(p, p+".backup")
				os.Symlink(p+".backup", p)
			case "trailing":
				f, _ := os.OpenFile(p, os.O_APPEND|os.O_WRONLY, 0600)
				f.WriteString("{}")
				f.Close()
			case "seed":
				os.WriteFile(p, []byte(`{"seed":"AA=="}`), 0600)
			}
			if _, err := LoadAuthority(dir); err == nil {
				t.Fatal("damaged identity accepted")
			}
		})
	}
}

func TestACrashBeforeTheFirstAuthorityIsRecoverable(t *testing.T) {
	parent := t.TempDir()
	directory := filepath.Join(parent, "authority")
	// What an interrupted creation leaves: a staging directory, never the real one.
	if err := os.Mkdir(directory+".new-interrupted", 0700); err != nil {
		t.Fatal(err)
	}
	first, err := LoadAuthority(directory)
	if err != nil {
		t.Fatal(err)
	}
	if leftovers, _ := filepath.Glob(filepath.Join(parent, "authority.new-*")); len(leftovers) != 0 {
		t.Fatalf("staging directories left behind: %v", leftovers)
	}
	again, err := LoadAuthority(directory)
	if err != nil || again.Household != first.Household {
		t.Fatalf("the created authority was not the one loaded again: %v", err)
	}
	// An existing directory whose identity is gone was lost, not interrupted.
	if err := os.Remove(filepath.Join(directory, "identity.json")); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadAuthority(directory); err == nil {
		t.Fatal("a lost authority was silently replaced")
	}
}

func TestCoordinatorAnswersMustDescribeTheDeviceAsked(t *testing.T) {
	machine := "mkey:" + strings.Repeat("a", 64)
	enroll := enrollment.Approval{Action: "enroll", Role: "phone", MachineKey: machine}
	good := enrollment.Device{Role: "phone", Status: "pending", MachineKey: machine, Revision: "ABCDEFGHIJKLMNOPQRSTUVWXYZ"}
	if err := answered(enroll, good); err != nil {
		t.Fatal(err)
	}
	for name, device := range map[string]enrollment.Device{
		"unknown status": {Role: "phone", Status: "granted", MachineKey: machine},
		"unknown role":   {Role: "admin", Status: "pending", MachineKey: machine},
		"other device":   {Role: "phone", Status: "pending", MachineKey: "mkey:" + strings.Repeat("b", 64)},
		"other role":     {Role: "pond", Status: "pending", MachineKey: machine},
		"odd revision":   {Role: "phone", Status: "pending", MachineKey: machine, Revision: "a/b"},
		"long revision":  {Role: "phone", Status: "pending", MachineKey: machine, Revision: strings.Repeat("a", 81)},
	} {
		if answered(enroll, device) == nil {
			t.Errorf("%s accepted", name)
		}
	}
	revoke := enrollment.Approval{Action: "revoke", Role: "phone"}
	if answered(revoke, enrollment.Device{Role: "phone", Status: "revoking"}) == nil {
		t.Error("an unconfirmed revocation accepted")
	}
	if err := answered(revoke, enrollment.Device{Role: "phone", Status: "revoked"}); err != nil {
		t.Fatal(err)
	}
}

func TestTheCoordinatorClientIgnoresProxySettings(t *testing.T) {
	t.Setenv("HTTPS_PROXY", "http://127.0.0.1:1")
	transport, ok := coordinatorClient().Transport.(*http.Transport)
	if !ok || transport.Proxy != nil {
		t.Fatal("the coordinator client would use an environment proxy")
	}
}

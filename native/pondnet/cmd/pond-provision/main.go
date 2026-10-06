// pond-provision is the operator's provisioning tool. It holds the provisioning key that
// signs device certificates, so it runs on the operator's own machine and never on a Pond
// or on the coordination service; the service is given only the public half.
//
//	pond-provision keygen --key provisioning.key
//	pond-provision public --key provisioning.key
//	pond-provision sign   --key provisioning.key --device-public-key <base64>
package main

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"time"

	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
)

func main() {
	if err := run(os.Args[1:], os.Stdout); err != nil {
		fmt.Fprintln(os.Stderr, "pond-provision:", err)
		os.Exit(1)
	}
}

func run(args []string, out io.Writer) error {
	if len(args) == 0 {
		return errors.New("usage: pond-provision keygen|public|sign --key FILE [--device-public-key KEY]")
	}
	flags := flag.NewFlagSet(args[0], flag.ContinueOnError)
	keyFile := flags.String("key", "", "the provisioning key file")
	device := flags.String("device-public-key", "", "the device public key a Pond printed when it was imaged")
	if err := flags.Parse(args[1:]); err != nil {
		return err
	}
	if *keyFile == "" {
		return errors.New("--key is required")
	}
	switch args[0] {
	case "keygen":
		public, err := keygen(*keyFile)
		if err != nil {
			return err
		}
		fmt.Fprintf(out, "%s\nkeep %s offline; give the coordination service only the line above, as --provisioning-key\n", public, *keyFile)
		return nil
	case "public":
		key, err := load(*keyFile)
		if err != nil {
			return err
		}
		fmt.Fprintln(out, base64.StdEncoding.EncodeToString(key.Public().(ed25519.PublicKey)))
		return nil
	case "sign":
		key, err := load(*keyFile)
		if err != nil {
			return err
		}
		certificate, err := sign(key, *device, time.Now())
		if err != nil {
			return err
		}
		return json.NewEncoder(out).Encode(certificate)
	}
	return fmt.Errorf("unknown command %q", args[0])
}

// keygen writes a new provisioning key, readable only by this account, and refuses to
// overwrite one: every certificate signed by the old key depends on it.
func keygen(path string) (string, error) {
	seed := make([]byte, ed25519.SeedSize)
	if _, err := rand.Read(seed); err != nil {
		return "", err
	}
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		if os.IsExist(err) {
			return "", fmt.Errorf("%s already exists; a provisioning key is never overwritten", path)
		}
		return "", err
	}
	data, _ := json.Marshal(struct {
		Seed string `json:"seed"`
	}{base64.StdEncoding.EncodeToString(seed)})
	_, err = file.Write(data)
	if err == nil {
		err = file.Sync()
	}
	if closed := file.Close(); err == nil {
		err = closed
	}
	if err != nil {
		os.Remove(path)
		return "", err
	}
	return base64.StdEncoding.EncodeToString(ed25519.NewKeyFromSeed(seed).Public().(ed25519.PublicKey)), nil
}

func load(path string) (ed25519.PrivateKey, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Mode().Perm()&0077 != 0 {
		return nil, fmt.Errorf("%s must be a regular file only this account can read", path)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var stored struct {
		Seed string `json:"seed"`
	}
	if err = json.Unmarshal(data, &stored); err != nil {
		return nil, errors.New("not a provisioning key")
	}
	seed, err := base64.StdEncoding.DecodeString(stored.Seed)
	if err != nil || len(seed) != ed25519.SeedSize {
		return nil, errors.New("not a provisioning key")
	}
	return ed25519.NewKeyFromSeed(seed), nil
}

// sign issues a certificate for device under a fresh random serial, the name the service
// records the admission under and the one an operator revokes.
func sign(key ed25519.PrivateKey, device string, now time.Time) (enrollment.Envelope, error) {
	serial := make([]byte, 16)
	if _, err := rand.Read(serial); err != nil {
		return enrollment.Envelope{}, err
	}
	return enrollment.SignDeviceCertificate(enrollment.DeviceCertificate{
		Version:         1,
		Serial:          hex.EncodeToString(serial),
		DevicePublicKey: device,
		Issued:          now.Unix(),
	}, key)
}

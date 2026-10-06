package pondnet

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"

	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
	"golang.org/x/sys/unix"
)

// Device is the key a Pond was given when it was imaged, and the certificate whoever
// imaged it signed for that key. It admits the Pond's household to a coordination service
// that trusts the signer, with no invite. It is separate from the household key, which is
// the household's own, and from the TLS and WireGuard keys.
//
// Only the first registration uses it, so it is not part of a backup: a Pond restored
// onto new hardware registers a household the service already knows, which needs nothing.
type Device struct {
	key         ed25519.PrivateKey
	Certificate enrollment.Envelope
}

const (
	deviceKeyFile         = "key.json"
	deviceCertificateFile = "certificate.json"
	maxDeviceFile         = 8192
)

// DeviceDirectory is where a Pond keeps its device key, beside its household authority.
func DeviceDirectory(authority string) string {
	return filepath.Join(filepath.Dir(authority), "device")
}

// CreateDevice makes this Pond's device key and returns its public half for the operator
// to sign. It runs once, when the Pond is imaged: an existing key is never replaced,
// because a certificate already issued for it would stop working.
func CreateDevice(directory string) (string, error) {
	if !filepath.IsAbs(directory) {
		return "", errors.New("device directory must be absolute")
	}
	if err := os.Mkdir(directory, 0700); err != nil {
		if os.IsExist(err) {
			return "", errors.New("this Pond already has a device key; it is never replaced")
		}
		return "", err
	}
	seed := make([]byte, ed25519.SeedSize)
	if _, err := rand.Read(seed); err != nil {
		return "", err
	}
	data, _ := json.Marshal(struct {
		Seed string `json:"seed"`
	}{base64.StdEncoding.EncodeToString(seed)})
	if err := writePrivate(directory, deviceKeyFile, data); err != nil {
		// Leave nothing behind, or every later attempt would refuse an empty directory.
		os.Remove(directory)
		return "", err
	}
	return base64.StdEncoding.EncodeToString(ed25519.NewKeyFromSeed(seed).Public().(ed25519.PublicKey)), nil
}

// InstallDeviceCertificate stores the certificate the operator signed for this Pond's key.
// It refuses one issued for any other key, which is what a certificate meant for another
// Pond would be. A newer certificate for the same key replaces the old one.
func InstallDeviceCertificate(directory string, certificate enrollment.Envelope) error {
	key, err := readDeviceKey(directory)
	if err != nil {
		return err
	}
	read, err := enrollment.ReadDeviceCertificate(certificate)
	if err != nil {
		return err
	}
	if read.DevicePublicKey != base64.StdEncoding.EncodeToString(key.Public().(ed25519.PublicKey)) {
		return errors.New("this certificate was issued for a different device key")
	}
	data, err := json.Marshal(certificate)
	if err != nil {
		return err
	}
	return writePrivate(directory, deviceCertificateFile, data)
}

// LoadDevice returns this Pond's device key and certificate, or nil when it was never
// provisioned. A key without a certificate, or files that are not private, are errors.
func LoadDevice(directory string) (*Device, error) {
	info, err := os.Lstat(directory)
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if !info.IsDir() || info.Mode().Perm()&0077 != 0 {
		return nil, errors.New("device directory is not private")
	}
	key, err := readDeviceKey(directory)
	if err != nil {
		return nil, err
	}
	data, err := readPrivate(filepath.Join(directory, deviceCertificateFile))
	if err != nil {
		return nil, errors.New("this Pond has a device key but no certificate installed for it")
	}
	var certificate enrollment.Envelope
	if err = decodeStrict(data, &certificate); err != nil {
		return nil, err
	}
	return &Device{key: key, Certificate: certificate}, nil
}

// Prove binds this device's certificate to the household registering, until expires.
func (d *Device) Prove(householdPublicKey string, expires int64) *enrollment.DeviceProof {
	proof := enrollment.ProveDevice(d.Certificate, d.key, householdPublicKey, expires)
	return &proof
}

func readDeviceKey(directory string) (ed25519.PrivateKey, error) {
	data, err := readPrivate(filepath.Join(directory, deviceKeyFile))
	if err != nil {
		return nil, err
	}
	var stored struct {
		Seed string `json:"seed"`
	}
	if err = decodeStrict(data, &stored); err != nil {
		return nil, err
	}
	seed, err := base64.StdEncoding.DecodeString(stored.Seed)
	if err != nil || len(seed) != ed25519.SeedSize {
		return nil, errors.New("invalid device key")
	}
	return ed25519.NewKeyFromSeed(seed), nil
}

// readPrivate reads a small regular file only this account can read, without following a
// symlink, checking the file it actually opened rather than the path.
func readPrivate(path string) ([]byte, error) {
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_NOFOLLOW|unix.O_NONBLOCK|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	file := os.NewFile(uintptr(fd), path)
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Mode().Perm()&0077 != 0 || info.Size() > maxDeviceFile {
		return nil, errors.New("device file is not a small private regular file: " + filepath.Base(path))
	}
	return io.ReadAll(io.LimitReader(file, maxDeviceFile+1))
}

// writePrivate replaces name in directory atomically with a 0600 file.
func writePrivate(directory, name string, data []byte) error {
	file, err := os.CreateTemp(directory, "."+name+"-")
	if err != nil {
		return err
	}
	defer os.Remove(file.Name())
	if err = file.Chmod(0600); err == nil {
		_, err = file.Write(data)
	}
	if err == nil {
		err = file.Sync()
	}
	if closed := file.Close(); err == nil {
		err = closed
	}
	if err == nil {
		err = os.Rename(file.Name(), filepath.Join(directory, name))
	}
	if err != nil {
		return err
	}
	dir, err := os.Open(directory)
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}

func decodeStrict(data []byte, into any) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(into); err != nil {
		return err
	}
	if decoder.Decode(new(any)) != io.EOF {
		return errors.New("trailing data")
	}
	return nil
}

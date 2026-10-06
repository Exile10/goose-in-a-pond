package enrollment

import (
	"crypto/ed25519"
	"encoding/base64"
	"encoding/json"
	"errors"
	"regexp"
	"strconv"
)

// Device certificates admit a household without an invite. Whoever images a Pond gives
// it a device key and signs that key, offline, with a provisioning key this service
// trusts; the Pond's first registration then carries the certificate, and a signature by
// the device key over the household it is registering. Nothing is typed and nothing
// transferable is handed to a person: an invite is a code that can leak, while using a
// certificate needs the device key, which never leaves the Pond.
//
// A certificate admits one household. It does not separate households, any more than an
// invite does: the policy does that.

// DeviceCertificate is what a provisioning key signs: one Pond's device key and the serial
// it is known by.
type DeviceCertificate struct {
	Version         int    `json:"v"`
	Serial          string `json:"serial"`
	DevicePublicKey string `json:"devicePublicKey"`
	Issued          int64  `json:"issued"`
}

// DeviceProof travels in a household's first registration: the signed certificate, and
// the device key's signature binding it to the household being registered, so a proof
// captured on the way can never admit any other household.
type DeviceProof struct {
	Certificate Envelope `json:"certificate"`
	Signature   string   `json:"signature"`
}

// Separate domains, so a certificate signature, a device binding and a household
// registration can never be presented as one another.
const (
	deviceCertificateDomain  = "goose-device-cert-v1\x00"
	deviceRegistrationDomain = "goose-device-registration-v1\x00"
)

// The closed set of refusals a registration can meet over its device certificate. Every
// way a certificate can fail to verify is one refusal, so a probe learns nothing about
// which check it failed.
const (
	deviceCertificateInvalid = "device_certificate_invalid"
	deviceRevoked            = "device_revoked"
	deviceUsed               = "device_used"
)

var serialPattern = regexp.MustCompile(`^[0-9a-f]{32}$`)

// ValidSerial reports whether serial has the form a device certificate's serial takes.
func ValidSerial(serial string) bool { return serialPattern.MatchString(serial) }

// SignDeviceCertificate signs certificate with a provisioning key. Run it on the machine
// that holds that key, never on a Pond or on this service.
func SignDeviceCertificate(certificate DeviceCertificate, provisioning ed25519.PrivateKey) (Envelope, error) {
	if certificate.Version != 1 || !ValidSerial(certificate.Serial) {
		return Envelope{}, errors.New("a device certificate needs version 1 and a 32-hex-digit serial")
	}
	if key, err := base64.StdEncoding.DecodeString(certificate.DevicePublicKey); err != nil || len(key) != ed25519.PublicKeySize {
		return Envelope{}, errors.New("the device public key is not an Ed25519 key")
	}
	payload, err := json.Marshal(certificate)
	if err != nil {
		return Envelope{}, err
	}
	return Envelope{
		Payload:   base64.StdEncoding.EncodeToString(payload),
		Signature: base64.StdEncoding.EncodeToString(ed25519.Sign(provisioning, append([]byte(deviceCertificateDomain), payload...))),
	}, nil
}

// ReadDeviceCertificate decodes certificate without checking who signed it; a Pond uses it
// to confirm a certificate names its own key before installing it.
func ReadDeviceCertificate(certificate Envelope) (DeviceCertificate, error) {
	var out DeviceCertificate
	payload, err := base64.StdEncoding.DecodeString(certificate.Payload)
	if err != nil || strict(payload, &out) != nil || out.Version != 1 || !ValidSerial(out.Serial) {
		return DeviceCertificate{}, errors.New("not a device certificate")
	}
	return out, nil
}

// ProveDevice binds certificate to the household registering, until expires.
func ProveDevice(certificate Envelope, device ed25519.PrivateKey, householdPublicKey string, expires int64) DeviceProof {
	return DeviceProof{
		Certificate: certificate,
		Signature:   base64.StdEncoding.EncodeToString(ed25519.Sign(device, deviceBinding(householdPublicKey, expires))),
	}
}

func deviceBinding(householdPublicKey string, expires int64) []byte {
	return []byte(deviceRegistrationDomain + householdPublicKey + "\x00" + strconv.FormatInt(expires, 10))
}

// VerifyDeviceProof returns the serial that proof's certificate admits under any of keys,
// or the refusal. The binding is checked against the registration it arrived in, whose own
// signature and expiry the caller has already verified.
func VerifyDeviceProof(proof *DeviceProof, keys []ed25519.PublicKey, householdPublicKey string, expires int64) (string, string) {
	payload, err := base64.StdEncoding.DecodeString(proof.Certificate.Payload)
	if err != nil {
		return "", deviceCertificateInvalid
	}
	signature, err := base64.StdEncoding.DecodeString(proof.Certificate.Signature)
	if err != nil {
		return "", deviceCertificateInvalid
	}
	signed := append([]byte(deviceCertificateDomain), payload...)
	trusted := false
	for _, key := range keys {
		if ed25519.Verify(key, signed, signature) {
			trusted = true
			break
		}
	}
	certificate, err := ReadDeviceCertificate(proof.Certificate)
	if !trusted || err != nil {
		return "", deviceCertificateInvalid
	}
	device, err := base64.StdEncoding.DecodeString(certificate.DevicePublicKey)
	if err != nil || len(device) != ed25519.PublicKeySize {
		return "", deviceCertificateInvalid
	}
	binding, err := base64.StdEncoding.DecodeString(proof.Signature)
	if err != nil || !ed25519.Verify(device, deviceBinding(householdPublicKey, expires), binding) {
		return "", deviceCertificateInvalid
	}
	return certificate.Serial, ""
}

// admitDeviceLocked decides whether serial may admit household id. A serial the same
// household already used admits it again, so a registration whose response was lost can
// be retried.
func (s *Store) admitDeviceLocked(serial, id string) string {
	if _, revoked := s.value.RevokedDevices[serial]; revoked {
		return deviceRevoked
	}
	if household, used := s.value.DeviceAdmissions[serial]; used && household != id {
		return deviceUsed
	}
	return ""
}

// RevokeDevice stops serial admitting a household from now on. A household it already
// admitted keeps its registration, since it holds its own key: remove that household
// separately if the device itself is lost.
func (s *Store) RevokeDevice(serial string, now int64) error {
	if !ValidSerial(serial) {
		return errors.New("a device serial is 32 hexadecimal digits")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.failure != nil {
		return s.failure
	}
	if s.value.RevokedDevices == nil {
		s.value.RevokedDevices = map[string]int64{}
	}
	if _, already := s.value.RevokedDevices[serial]; already {
		return nil
	}
	s.value.RevokedDevices[serial] = now
	if err := s.save(); err != nil {
		delete(s.value.RevokedDevices, serial)
		return err
	}
	return nil
}

// spendAdmissionLocked records that serial, or else the invite with digest, admitted
// household id, and returns what undoes it if the write that creates the household fails.
func (s *Store) spendAdmissionLocked(serial, digest, id string) func() {
	if serial != "" {
		if s.value.DeviceAdmissions == nil {
			s.value.DeviceAdmissions = map[string]string{}
		}
		previous, had := s.value.DeviceAdmissions[serial]
		s.value.DeviceAdmissions[serial] = id
		return func() {
			if had {
				s.value.DeviceAdmissions[serial] = previous
			} else {
				delete(s.value.DeviceAdmissions, serial)
			}
		}
	}
	invite := s.value.Invites[digest]
	spent := invite
	spent.ConsumedBy = id
	s.value.Invites[digest] = spent
	return func() { s.value.Invites[digest] = invite }
}

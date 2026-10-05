// pond-enrollment serves signed pilot registrations; provision is an offline operator command.
package main

import (
	"context"
	"crypto/ed25519"
	"encoding/base64"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

func main() {
	if err := run(); err != nil {
		slog.Error("enrollment service failed", "error", err)
		os.Exit(1)
	}
}
func run() error {
	directory := flag.String("state", "", "private state directory")
	listen := flag.String("listen", "127.0.0.1:8081", "private HTTP listener behind HTTPS proxy")
	origin := flag.String("headscale", "", "private Headscale origin")
	secret := flag.String("credential-file", "", "Headscale admin credential file")
	provision := flag.String("provision", "", "operator-provisioned household identifier")
	public := flag.String("public-key", "", "household Ed25519 public key in base64")
	user := flag.String("user-id", "", "operator-created Headscale user ID")
	port := flag.Uint("https-port", 4443, "household companion port")
	health := flag.Bool("health-check", false, "check the local enrollment listener")
	proxies := flag.String("trusted-proxy", "", "comma-separated CIDRs of the reverse proxies whose X-Forwarded-For is believed")
	issue := flag.Bool("issue-invite", false, "ask the running service for a household invite and print it")
	expires := flag.Duration("expires", enrollment.DefaultInviteLifetime, "how long an issued invite stays usable (at most 720h)")
	socket := flag.String("admin-socket", "", "private socket for issuing invites (default: admin.sock in the state directory)")
	revokeInvite := flag.String("revoke-invite", "", "ask the running service to withdraw an unspent invite")
	revokeDevice := flag.String("revoke-device", "", "ask the running service to stop a device certificate serial admitting a household")
	var provisioning []ed25519.PublicKey
	flag.Func("provisioning-key", "base64 Ed25519 public key whose device certificates admit a household; repeat to trust more than one", func(text string) error {
		key, err := base64.StdEncoding.DecodeString(strings.TrimSpace(text))
		if err != nil || len(key) != ed25519.PublicKeySize {
			return errors.New("a provisioning key is a base64 Ed25519 public key")
		}
		provisioning = append(provisioning, ed25519.PublicKey(key))
		return nil
	})
	flag.Parse()
	if *health {
		client := http.Client{Timeout: 2 * time.Second}
		r, e := client.Get("http://127.0.0.1:8081/health")
		if e != nil {
			return e
		}
		r.Body.Close()
		if r.StatusCode != 200 {
			return errors.New("enrollment unhealthy")
		}
		return nil
	}
	administering := *issue || *revokeInvite != "" || *revokeDevice != ""
	if *socket == "" {
		if *directory == "" && administering {
			// Otherwise the socket is looked for in the current directory, and the error
			// is a bare "no such file", which says nothing about what to pass.
			return errors.New("pass --state with the running service's state directory (in the compose stack, --state /state), or --admin-socket")
		}
		*socket = filepath.Join(*directory, "admin.sock")
	}
	switch {
	case *issue:
		return issueInvite(*socket, *expires)
	case *revokeInvite != "":
		if err := adminRequest(*socket, "/v1/invite/revoke?invite="+url.QueryEscape(*revokeInvite), nil); err != nil {
			return err
		}
		fmt.Println("invite withdrawn; it admits nobody now")
		return nil
	case *revokeDevice != "":
		if err := adminRequest(*socket, "/v1/device/revoke?serial="+url.QueryEscape(*revokeDevice), nil); err != nil {
			return err
		}
		fmt.Println("device certificate revoked; it admits no household from now on")
		return nil
	}
	store, err := enrollment.Open(*directory)
	if err != nil {
		return err
	}
	defer store.Close()
	if *provision != "" {
		if *port == 0 || *port > 65535 {
			return &configError{}
		}
		return store.Provision(*provision, enrollment.Household{PublicKey: *public, UserID: *user, Port: uint16(*port)})
	}
	credential, err := os.ReadFile(*secret)
	if err != nil {
		return err
	}
	backend, err := enrollment.NewHeadscale(*origin, string(credential))
	if err != nil {
		return err
	}
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	trusted, err := parsePrefixes(*proxies)
	if err != nil {
		return err
	}
	handler, err := enrollment.New(ctx, store, backend)
	if err != nil {
		return err
	}
	handler.TrustedProxies = trusted
	handler.ProvisioningKeys = provisioning
	if len(provisioning) == 0 {
		slog.Info("no --provisioning-key: only invites admit a new household")
	} else {
		slog.Info("device certificates admit a new household", "provisioning_keys", len(provisioning))
	}
	if len(trusted) == 0 {
		slog.Warn("no --trusted-proxy: behind a reverse proxy every client shares one rate-limit budget")
	}
	go func() {
		ticker := time.NewTicker(30 * time.Second)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				job, stop := context.WithTimeout(ctx, 15*time.Second)
				if handler.Reconcile(job) != nil {
					slog.Warn("network enrollment reconciliation deferred")
				}
				stop()
			}
		}
	}()
	admin, err := listenAdmin(*socket)
	if err != nil {
		return err
	}
	adminServer := &http.Server{Handler: handler.AdminHandler(), ReadHeaderTimeout: 5 * time.Second}
	go adminServer.Serve(admin)
	defer adminServer.Close()
	server := &http.Server{Addr: *listen, Handler: handler, ReadHeaderTimeout: 5 * time.Second, ReadTimeout: 10 * time.Second, WriteTimeout: 20 * time.Second, IdleTimeout: 30 * time.Second, MaxHeaderBytes: 8192}
	go func() {
		<-ctx.Done()
		shutdown, stop := context.WithTimeout(context.Background(), 5*time.Second)
		defer stop()
		server.Shutdown(shutdown)
	}()
	slog.Info("enrollment service ready")
	err = server.ListenAndServe()
	if err == http.ErrServerClosed {
		return nil
	}
	return err
}

// listenAdmin serves invite issuance on a Unix socket, mode 0600, so only the account
// running the service can use it.
func listenAdmin(path string) (net.Listener, error) {
	// A socket path is bounded by sockaddr_un: 104 bytes on macOS, 108 on Linux.
	if len(path) > 100 {
		return nil, fmt.Errorf("admin socket path is too long for a Unix socket; pass a shorter --admin-socket: %s", path)
	}
	if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	listener, err := net.Listen("unix", path)
	if err != nil {
		return nil, err
	}
	if err = os.Chmod(path, 0600); err != nil {
		listener.Close()
		return nil, err
	}
	return listener, nil
}

// issueInvite asks the running service, which holds the state lock, for an invite.
func issueInvite(socket string, lifetime time.Duration) error {
	var issued struct {
		Invite  string `json:"invite"`
		Expires string `json:"expires"`
	}
	if err := adminRequest(socket, "/v1/invite?lifetime="+url.QueryEscape(lifetime.String()), &issued); err != nil {
		return err
	}
	fmt.Printf("%s\nexpires %s; it is shown once and admits one household\n", issued.Invite, issued.Expires)
	return nil
}

// adminRequest posts to the running service's administrative socket and decodes its
// answer into out, when out is not nil.
func adminRequest(socket, path string, out any) error {
	client := http.Client{Timeout: 10 * time.Second, Transport: &http.Transport{
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			return (&net.Dialer{}).DialContext(ctx, "unix", socket)
		},
	}}
	response, err := client.Post("http://admin"+path, "application/json", nil)
	if err != nil {
		return fmt.Errorf("no enrollment service is listening on %s; pass --state with the running service's state directory (in the compose stack, --state /state), or --admin-socket: %w", socket, err)
	}
	defer response.Body.Close()
	body, err := io.ReadAll(io.LimitReader(response.Body, 4096))
	if err != nil {
		return err
	}
	if response.StatusCode != http.StatusOK {
		var refused struct {
			Error string `json:"error"`
		}
		_ = json.Unmarshal(body, &refused)
		return fmt.Errorf("the enrollment service refused: %s", refused.Error)
	}
	if out == nil {
		return nil
	}
	return json.Unmarshal(body, out)
}

func parsePrefixes(list string) ([]netip.Prefix, error) {
	var prefixes []netip.Prefix
	for _, entry := range strings.Split(list, ",") {
		if entry = strings.TrimSpace(entry); entry == "" {
			continue
		}
		prefix, err := netip.ParsePrefix(entry)
		if err != nil {
			return nil, fmt.Errorf("invalid --trusted-proxy %q: %w", entry, err)
		}
		prefixes = append(prefixes, prefix.Masked())
	}
	return prefixes, nil
}

type configError struct{}

func (*configError) Error() string { return "invalid companion port" }

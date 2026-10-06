# Draft upstream request: let a tsnet embedder turn off the DNS fallback

Status: draft for the maintainers to review and file with tailscale/tailscale. Not
filed.

## Title

tsnet: option to disable `dnsfallback` for self-hosted control servers

## Body

We embed tsnet (v1.102.4) in a home server and a mobile app that use a self-hosted
Headscale control server and our own DERP map. When the system resolver cannot
resolve the control server's host name, the control client falls back to
`net/dnsfallback`:

- `control/controlclient/direct.go:337`:
  `LookupIPFallback: dnsfallback.MakeLookupFunc(opts.Logf, netMon)`
- `control/controlhttp/client.go:374`: the same, for the Noise dialer.

`dnsfallback.GetDERPMap` always starts from the static map compiled into the module
and merges the cached map into it, so the query goes to the bootstrap-DNS endpoint
of Tailscale's own DERP servers (`https://derpN.tailscale.com/bootstrap-dns`) even
when the control server's DERP map omits the default regions. For a deployment that
has chosen not to depend on Tailscale infrastructure, that sends the client's
address and the control server's host name to a third party whenever local DNS
fails.

We could find no envknob, `tsnet.Server` field or build tag to turn this off.
`OmitDefaultRegions` in the DERP map does not affect it (the TODO at
`net/dnsfallback/dnsfallback.go` GetDERPMap notes the question).

Would you accept one of:

1. a `tsnet.Server` field (or an envknob) that disables `LookupIPFallback` for the
   control client, or
2. making `dnsfallback` honour `OmitDefaultRegions` in the cached DERP map, so it
   only asks the operator's own DERP servers?

We are happy to send a patch for whichever you prefer.

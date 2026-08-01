# Deployment and TLS termination

Wayfinder serves HTTP only. It deliberately does not load certificates or terminate TLS:
certificate issuance, renewal, reload, and protocol policy belong at a reverse proxy such as
Caddy, nginx, or Traefik. Keeping that lifecycle outside Wayfinder preserves the one-binary search
service while using established TLS tooling.

For any client outside the Wayfinder host, terminate TLS at the proxy and keep the
proxy-to-Wayfinder hop on loopback. HTTP Basic credentials are plaintext-equivalent and must not
cross an untrusted plaintext network.

## Caddy example

Start Wayfinder on its default loopback listener:

```sh
wayfinder schema.toml data/ 127.0.0.1:8983
```

Replace the hostname and core name in this `Caddyfile`:

```caddyfile
search.example.com {
    # Wayfinder intentionally leaves these health endpoints unauthenticated.
    # Do not publish them unless external health monitoring requires them.
    @public_health path /ui/ping /solr/content/admin/ping
    respond @public_health 404

    reverse_proxy 127.0.0.1:8983
}
```

Caddy obtains and renews the public certificate automatically when the hostname resolves to the
proxy and ports 80 and 443 are reachable. It forwards the `Authorization` header to Wayfinder
without additional configuration.

Verify the HTTPS route with the configured Basic-auth username (curl prompts for its password):

```sh
curl -u operator 'https://search.example.com/solr/content/select?q=*:*&rows=0'
```

Keep port 8983 blocked from external access. If the proxy cannot run on the same host, use only a
trusted private network or an encrypted tunnel for the upstream hop; never send Basic credentials
over an untrusted HTTP hop. In container deployments, expose Wayfinder only on an internal
container network rather than publishing its HTTP port.

The example returns 404 for `/ui/ping` and `/solr/content/admin/ping` at the public proxy because
Wayfinder exempts those exact routes from authentication. If external monitoring needs them,
remove that matcher only deliberately and restrict access with the proxy or firewall.

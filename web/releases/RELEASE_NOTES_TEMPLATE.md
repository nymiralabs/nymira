# Web release <version> (<profile>)

- Commit: `<full commit hash>`
- Bundle hash: `<bundle_hash>`
- Header-set hash: `<header_set_hash>`
- Signing key fingerprint (SHA-256 of the public key): `<fingerprint>`
- Signed manifest: `web/releases/<profile>-<version>/manifest.json` and `manifest.json.minisig`
- Reproducible build: `web-integrity / reproducible` run <link>
- Independent rebuild: <link to attestation, or "none yet">

Check it: `nymirad verify-web`, or rebuild with `web/scripts/rebuild-in-container.sh <tag> <profile> <version>`
(see `web/INTEGRITY.md`).

What this does not cover: a compromised host could serve a different app. `verify-web` catches that
only if you run it and the host does not show it a clean copy.

Changes since the last release: <list>

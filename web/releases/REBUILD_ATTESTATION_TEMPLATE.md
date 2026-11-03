# Rebuild attestation for web release <version> (<profile>)

Written by someone outside the Nymira team, and published where the team cannot edit it.

- Release tag and commit: `<tag>` / `<full commit hash>`
- Date, and the machine (CPU architecture, operating system, Docker version):
- Command run: `web/scripts/rebuild-in-container.sh <tag> <profile> <version>`
- Container base image digest as printed by the build: `sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6`
- `bundle_hash` I got: `<hash>`
- `header_set_hash` I got: `<hash>`
- `manifest.json` byte-for-byte equal to the signed one (`cmp` result): yes / no
- Signature verified with `minisign -V -H -p <key.pub> -m manifest.json`, key fingerprint
  `<fingerprint>` read from <where I read it, outside the project's accounts>: yes / no
- What I did not check:

Signed by me: <name or key>, <signature or link>

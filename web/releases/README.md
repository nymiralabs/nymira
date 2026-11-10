# Signed releases

[README](../../README.md) · [Website](https://nymira.pages.dev) · [Documentation](https://nymira.pages.dev/#docs) · [Try it in 20 minutes](../../JUDGES.md) · [Threat model](../../THREAT_MODEL.md) · [Releases](https://github.com/nymiralabs/nymira/releases)

One directory per release, `<profile>-<version>/`, holding `manifest.json` and `manifest.json.minisig`
exactly as signed offline (rules: `web/INTEGRITY.md`). The deploy workflow copies them into the
site and refuses to publish without them. Signed so far: testnet 0.1.0, 0.1.1, 0.1.2 and 0.1.3 (0.1.3 is live). The two templates in this folder are for release notes and for an independent rebuild attestation.

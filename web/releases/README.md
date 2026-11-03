# Signed releases

One directory per release, `<profile>-<version>/`, holding `manifest.json` and `manifest.json.minisig`
exactly as signed offline (rules: `web/INTEGRITY.md`). The deploy workflow copies them into the
site and refuses to publish without them. No directory exists until the first release is signed.

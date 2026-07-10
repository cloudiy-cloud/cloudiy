# Code signing & notarization

The release binaries are **unsigned by default**. Signing is optional and turns
on automatically once you add the secrets below — the `Release` workflow's
signing steps are gated on their presence, so an unsigned build always works.

## Does the CLI need it?

Mostly no, for the primary install path:

- **macOS + `curl … | sh`** — `curl` does not set the `com.apple.quarantine`
  attribute, so Gatekeeper does **not** prompt for a binary installed this way.
  The "unverified developer" prompt only appears if a user downloads the
  `.tar.gz` in a **browser** and double-clicks it.
- **Windows + `irm … | iex`** — `Invoke-WebRequest` marks the download with the
  Mark-of-the-Web, so **SmartScreen may warn** on first run. Authenticode
  signing removes that.

Signing/notarization pays off most for the **desktop app (step 3)** — a
browser-downloaded `.dmg`/`.exe` installer — so full notarization is best wired
there. The steps here cover the CLI in the meantime.

## Enable it (GitHub → Settings → Secrets and variables → Actions)

**macOS (Developer ID Application cert, requires an Apple Developer account, $99/yr):**

| Secret | Value |
| --- | --- |
| `APPLE_CERT_P12` | base64 of your `.p12` (`base64 -i cert.p12 \| pbcopy`) |
| `APPLE_CERT_PASSWORD` | the `.p12` export password |
| `APPLE_SIGN_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |

**Windows (Authenticode / code-signing cert):**

| Secret | Value |
| --- | --- |
| `WINDOWS_CERT_PFX` | base64 of your `.pfx` |
| `WINDOWS_CERT_PASSWORD` | the `.pfx` password |

With these set, the next tagged release signs the binaries; without them it
builds unsigned exactly as today.

## Full macOS notarization (for the app stage)

Signing (above) is not the same as notarization. To clear Gatekeeper on a
**browser-downloaded** artifact, after `codesign` you also:

```sh
xcrun notarytool submit cloudiy-<target>.zip \
  --apple-id "$APPLE_ID" --team-id "$TEAM_ID" --password "$APP_SPECIFIC_PW" --wait
# then staple the ticket — for a .dmg/.app/.pkg (a bare CLI can't be stapled):
xcrun stapler staple Cloudiy.dmg
```

This is why notarization is deferred to the desktop-app installer, where there
is a `.dmg`/`.app` to staple.

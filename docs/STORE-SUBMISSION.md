# Microsoft Store — Submission Guide (RDOC SquadLink Lite)

> **Decision: the project now ships via MSIX (Weg B).** Microsoft signs the package,
> so **no own code-signing certificate is needed**. See
> [MSIX-PACKAGING.md](MSIX-PACKAGING.md) for the actual build/pack/submit steps.
> The unpackaged EXE/MSI route below is kept for reference only.

---

Distribution path: **unpackaged EXE/MSI** ("EXE or MSI app" product type in Partner
Center). Chosen over MSIX because it keeps the existing setup working with minimal
rework:

| Concern                         | Unpackaged (chosen) | MSIX |
| ------------------------------- | ------------------- | ---- |
| `squadlink://` via registry     | works               | must move to AppxManifest `uap:Protocol` |
| GitHub self-update prompt       | allowed             | forbidden (Store policy) — must remove |
| Raw Input global PTT hook       | works               | works, but flagged more often |
| Code signing                    | **you must sign**   | Microsoft signs |
| Tauri native build target       | yes (NSIS/MSI)      | none — extra tooling |

The only hard purchase: an **Authenticode code-signing certificate**.

---

## 0. What only you can do (start these first — they have lead time)

1. **Reserve the app name** in Partner Center → Apps & Games → *New product* →
   *EXE or MSI app* → reserve "RDOC SquadLink Lite". Note the assigned **Publisher**
   (`CN=...`) and **Store ID**.
2. **Get a code-signing certificate.** The cert *Subject* must match the Partner
   Center Publisher exactly.
   - **Recommended: Azure Trusted Signing** (~10 USD/month, Microsoft-managed, no
     hardware token, validated identity, works with `signtool`). Best fit for CI.
   - Alternatives: OV cert (file `.pfx`, cheaper but more SmartScreen warmup) or EV
     (token, not required here).
3. **Host a privacy policy** (mandatory — the app uses the microphone + network).
   Use [PRIVACY-POLICY.md](PRIVACY-POLICY.md), publish it at a stable URL
   (e.g. `https://squadlink.raumdock.org/privacy`).

---

## 1. Sign the installers (CI)

Store rejects unsigned EXE/MSI. Sign the NSIS + MSI bundles **before** they are
uploaded / released.

With **Azure Trusted Signing**, Tauri signs during bundling via `bundle.windows.signCommand`.
Add to `apps/companion/src-tauri/tauri.conf.json` (do NOT commit secrets — the
command reads them from env in CI):

```jsonc
"bundle": {
  "windows": {
    // Run signtool with the Trusted Signing dlib. %1 = file Tauri passes in.
    "signCommand": "signtool sign /v /debug /fd SHA256 /tr http://timestamp.acs.microsoft.com /td SHA256 /dlib \"%TS_DLIB%\" /dmdf \"%TS_METADATA%\" \"%1\""
  }
}
```

CI prerequisites (GitHub Actions secrets → env in the build step):
- `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` (service principal with
  the *Trusted Signing Certificate Profile Signer* role).
- Install the Trusted Signing dlib: `dotnet tool install --global Azure.CodeSigning.Dlib`
  (or the `azure/trusted-signing-action`), set `TS_DLIB` to its path and `TS_METADATA`
  to a JSON with your `Endpoint` / `CodeSigningAccountName` / `CertificateProfileName`.

With a **`.pfx` OV cert** instead, simpler config:
```jsonc
"bundle": { "windows": { "certificateThumbprint": "<THUMBPRINT>", "digestAlgorithm": "sha256", "timestampUrl": "http://timestamp.digicert.com" } }
```
(import the pfx into the runner's cert store first).

> Update `.github/workflows/build-companion.yml`: drop the "UNSIGNED" notes, inject
> the signing secrets into the `tauri-action` step. Verify with
> `signtool verify /pa /v <installer>`.

---

## 2. Installer must support silent install/uninstall

Partner Center asks for the silent switches and return codes. Tauri bundles already
support them — pick one target to submit.

**NSIS** (recommended — can install per-user, no UAC):
- Set in `tauri.conf.json`: `"bundle": { "windows": { "nsis": { "installMode": "currentUser" } } }`
- Silent install: `RDOC SquadLink Lite_x64-setup.exe /S`
- Silent uninstall: `"%LOCALAPPDATA%\...\uninstall.exe" /S`
- Success exit code: `0`.

**MSI** (per-machine, needs admin):
- Silent install: `msiexec /i "RDOC SquadLink Lite_x64_en-US.msi" /qn /norestart`
- Silent uninstall: `msiexec /x {PRODUCT-CODE-GUID} /qn`
- ProductCode is in the MSI: `Get-MsiProductCode` or open with Orca. Changes per
  version — re-read on each release.

**Test before submitting** (clean VM): run the silent install, confirm app launches,
run silent uninstall, confirm clean removal + exit code 0.

---

## 3. Partner Center listing

- **Packages**: upload the signed installer (or its download URL). Enter the silent
  install + uninstall commands + success codes from §2.
- **Properties**: category (e.g. *Social* / *Utilities & tools*), supports keyboard/mouse.
- **Age rating**: complete the IARC questionnaire (no objectionable content, but
  declare user-generated communication = voice/chat → likely Teen).
- **Privacy policy URL**: required (from §0.3).
- **Store listing**: DE + EN description, ≥1 screenshot (≥1366×768 PNG), app icon.
- **Notes for certification** (free text to reviewer): explain the global keyboard
  hook = configurable in-game push-to-talk; explain microphone = voice chat; explain
  `squadlink://` deep link = one-click join from the Fleetplanner.

---

## 4. App-side cleanups before submit

- **Self-update prompt**: keep it (unpackaged allowed). Make sure
  `open_download` points at the official page only.
- **Microphone disclosure**: ensure the listing + privacy policy both state mic use.
- **No admin if possible**: prefer NSIS `installMode: currentUser` (§2) to avoid the
  UAC prompt the reviewer sees.
- Confirm `productName` / `identifier` / version in `tauri.conf.json` match the
  Partner Center product.

---

## 5. Submit → review

Certification runs ~1–3 business days. EXE/MSI apps also get an automated malware +
signature scan; an unsigned or mismatched-publisher installer fails immediately.

## Open decisions for you

- Which cert: **Azure Trusted Signing** (recommended) vs OV `.pfx`.
- Which installer to submit: **NSIS per-user** (recommended) vs MSI per-machine.

Once you pick, I can wire the signing into `build-companion.yml` + `tauri.conf.json`
and set `installMode`.

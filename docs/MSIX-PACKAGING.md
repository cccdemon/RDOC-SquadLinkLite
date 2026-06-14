# MSIX Packaging — Microsoft Store (Weg B)

Chosen path: **MSIX (packaged)**. Microsoft signs the package on submission, so no
own code-signing certificate is needed for the Store. Tradeoffs already handled in
code:

- `squadlink://` is declared in the **package manifest** (not the registry) —
  [`apps/companion/msix/AppxManifest.xml`](../apps/companion/msix/AppxManifest.xml).
  `main.rs` skips `register_all()` when it detects it runs inside a package.
- The **self-update prompt is hidden** in packaged builds (`is_store_build` command →
  the webview suppresses the update banner). The Store handles updates.

> Note: MSIX is submitted as an **"MSIX/PWA app"** product in Partner Center and the
> package is **uploaded** (Microsoft hosts + signs it). The "package URL" field of the
> EXE/MSI flow does **not** apply here.

> **Switching from an existing EXE/MSI submission:** the product type is fixed at
> creation. To reuse the reserved name "RDOC SquadLink Lite", delete the EXE/MSI
> product, then create a new **App** product (which accepts MSIX) and reserve the same
> name.

## Quick path (script)

Once `AppxManifest.xml` placeholders are filled (§2), one command builds + packs:

```powershell
# from repo root
pwsh apps/companion/msix/pack.ps1            # → apps/companion/msix/RDOCSquadLinkLite_<ver>_x64.msix
pwsh apps/companion/msix/pack.ps1 -SelfSign  # + self-sign for local install testing
```

`pack.ps1` builds the exe, generates the visual assets from the logo, stages, and runs
`makeappx`. Upload the **unsigned** `.msix` to the Store. The manual steps below explain
what it does.

---

## Prerequisites (one-time)

- **Windows SDK** (for `makeappx.exe`, `signtool.exe`, `makecert`/PowerShell pki).
  Typically `C:\Program Files (x86)\Windows Kits\10\bin\<ver>\x64`.
- **Partner Center** product reserved → note **Identity Name** + **Publisher (CN=...)**
  + **PublisherDisplayName** (App management → Product identity).
- Visual assets (PNG) generated at the required MSIX sizes (see §3).

---

## 1. Build the app binary

```powershell
cd apps/companion
pnpm install --frozen-lockfile
pnpm tauri build
```

The release binary lands in `apps/companion/src-tauri/target/release/rdoc-squadlink-lite.exe`.
The frontend is embedded in the exe; the only external runtime dependency is the
WebView2 runtime (Evergreen — present on Windows 11; do not bundle).

## 2. Fill the manifest

Edit [`apps/companion/msix/AppxManifest.xml`](../apps/companion/msix/AppxManifest.xml):
- `Identity/@Name` ← Partner Center Identity Name
- `Identity/@Publisher` ← Partner Center Publisher (`CN=...`, exact match)
- `Identity/@Version` ← `MAJOR.MINOR.PATCH.0` in sync with `tauri.conf.json`
- `PublisherDisplayName`

## 3. Visual assets

Place PNGs in `msix-staging/Assets/` (filenames must match the manifest):
| File | Size |
| ---- | ---- |
| `StoreLogo.png`          | 50×50  |
| `Square44x44Logo.png`    | 44×44  |
| `Square150x150Logo.png`  | 150×150 |
| `Wide310x150Logo.png`    | 310×150 |

Generate from `src/Squad_Link_Lite.png` (any image tool / `pnpm tauri icon` outputs a
superset you can resize).

## 4. Stage + pack

```powershell
$stage = "msix-staging"
Remove-Item $stage -Recurse -Force -ErrorAction Ignore
New-Item -ItemType Directory $stage, "$stage\Assets" | Out-Null

Copy-Item apps/companion/msix/AppxManifest.xml $stage\
Copy-Item apps/companion/src-tauri/target/release/rdoc-squadlink-lite.exe $stage\
# WebView2Loader.dll only if your build emits one next to the exe:
# Copy-Item apps/companion/src-tauri/target/release/WebView2Loader.dll $stage\ -ErrorAction Ignore
Copy-Item path\to\assets\*.png $stage\Assets\

& "$env:WindowsSdkVerBinPath\x64\makeappx.exe" pack /d $stage /p RDOCSquadLinkLite.msix /o
```

## 5. Local test (self-signed — Store submission does NOT need this)

The Store signs the package; to **install and test locally** you must self-sign with a
cert whose Subject exactly equals the manifest `Publisher`:

```powershell
$pub = "CN=REPLACE-WITH-PARTNER-CENTER-PUBLISHER"
$cert = New-SelfSignedCertificate -Type Custom -Subject $pub -KeyUsage DigitalSignature `
  -FriendlyName SquadLinkTest -CertStoreLocation "Cert:\CurrentUser\My" `
  -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3","2.5.29.19={text}")

& signtool sign /fd SHA256 /a /sha1 $cert.Thumbprint RDOCSquadLinkLite.msix

# Trust the cert, then install:
Export-Certificate -Cert $cert -FilePath squadlink-test.cer
Import-Certificate -FilePath squadlink-test.cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople
Add-AppxPackage RDOCSquadLinkLite.msix
```

Test checklist:
- App launches, mic consent prompt appears, voice + chat work.
- `start squadlink://connect?ws=...&room=...&token=...` activates the app + auto-connects.
- Update banner is **absent** (Store build).

## 6. WACK (certification self-check)

Run **Windows App Certification Kit** against the `.msix` and fix any failures before
submitting:
```powershell
& "$env:WindowsSdkVerBinPath\x86\appcert.exe" reset
# or use the WACK GUI → "Validate a desktop / packaged app"
```

## 7. Submit

Partner Center → your product (MSIX/PWA app) → **Packages** → upload the **unsigned**
`.msix` (Store re-signs). Complete: privacy policy URL ([PRIVACY-POLICY.md](PRIVACY-POLICY.md)),
age rating (declare voice/text comms), listing, screenshots. In **Notes for
certification** explain: global keyboard hook = in-game push-to-talk; microphone =
voice chat; `squadlink://` = one-click join from the Fleetplanner.

---

## CI (optional, later)

The pack + (test-)sign steps can move into `.github/workflows/build-companion.yml` as a
separate `msix` job that runs after `pnpm tauri build`, uploading the `.msix` as an
artifact. The Store upload itself can be automated via the Partner Center submission
API, or done manually per release.

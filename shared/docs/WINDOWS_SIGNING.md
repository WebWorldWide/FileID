# Windows release signing

FileID's release build is provider-neutral. It can sign with either:

1. a code-signing certificate already available in the Windows certificate store, or
2. a PowerShell adapter that delegates each file to a managed/cloud signing provider.

No provider is currently configured in GitHub Actions. Tagged pushes therefore run an unsigned validation build and upload clearly named CI artifacts, but the workflow cannot publish. Maintainers may attach those artifacts only to an explicitly unsigned prerelease; a thumbprint secret is never treated as a private key.

## What the release build enforces

`platforms/windows/build/publish-bundle.ps1` signs in this order:

1. every unsigned `.exe` and `.dll` in each published app tree; valid Microsoft/vendor signatures are preserved and verified,
2. each architecture MSI,
3. the detached WiX Burn engine,
4. the reattached `FileIDSetup.exe` bundle.

Every newly signed file must:

- pass `signtool verify /pa /all /v`,
- have `Get-AuthenticodeSignature.Status == Valid`,
- match the configured signer subject,
- match the independently configured signer public-key SHA-256 identity, and
- carry a timestamp that Windows/SignTool trusts. Local signing requests RFC 3161 with SHA-256; managed adapters are required to do the same. The common verifier confirms timestamp presence and trust but does not independently parse the token's digest algorithm.

The signed app embeds both the expected signer subject and the approved public-key identity as assembly metadata. At runtime it verifies the app assembly and engine against that independent key pin. Development builds remain able to run unsigned local engines. Subject text is audit/display metadata, not the cryptographic identity boundary. Certificate rotation is an explicit release-policy update, not an automatic subject-based acceptance.

## Local certificate-store signing

The private key must already be available to SignTool through `Cert:\CurrentUser\My` or `Cert:\LocalMachine\My`.

```powershell
pwsh platforms/windows/build/publish-bundle.ps1 `
  -SignThumbprint "0123456789ABCDEF..." `
  -SignerSubject "CN=Your verified publisher, O=Your organization, C=US"
```

When `-SignerSubject` is omitted in local-store mode, the script reads it from the matched certificate. A thumbprint is public metadata, not a private key; putting only a thumbprint in a GitHub secret cannot make hosted CI signing work.

## Managed-provider adapter

The adapter is a PowerShell script with this contract:

```powershell
param(
    [Parameter(Mandatory=$true)][string]$Path,
    [Parameter(Mandatory=$true)][string]$TimestampServer,
    [Parameter(Mandatory=$true)][string]$Description
)

# Authenticate using the provider's protected CI mechanism, sign $Path with
# SHA-256 plus the supplied RFC 3161 timestamp server, and exit non-zero on
# any failure.
```

Invoke it with:

```powershell
pwsh platforms/windows/build/publish-bundle.ps1 `
  -SigningAdapter .\provider-sign.ps1 `
  -SignerSubject "CN=Publisher name shown by the signing certificate" `
  -SignerPublicKeySha256 "64_HEX_DIGITS_FOR_THE_APPROVED_KEY"
```

The adapter never decides whether verification passed. The release script independently verifies the resulting signature, trusted timestamp, publisher, and approved public-key identity after every call. Set `FILEID_SIGNER_PUBLIC_KEY_SHA256` (or the parameter above) from a certificate inspected out-of-band. A provider must keep one approved signer key for all files in a release invocation; rotation requires an explicit reviewed pin change.

## Low-cost provider paths

### SignPath Foundation

Potentially free for qualifying open-source projects. FileID's Apache-2.0 license and public build appear compatible, but acceptance is discretionary. Requirements include a verifiable public build, MFA, manual release approval, signing/privacy policies, and an entirely qualifying open-source distribution. The displayed Windows publisher is normally `SignPath Foundation`.

Use this route only after written acceptance **and confirmation that FileID receives a project-specific or otherwise independently approvable signer key/policy identity**. A subject/key shared with unrelated projects is not sufficient for FileID's runtime pin. Add SignPath's authentication/submission step to the protected release environment only if that requirement is met, then preserve the same PE → MSI → Burn-engine → bundle order.

### Azure Artifact Signing

The practical fallback if SignPath does not accept the project. Public Trust Basic is roughly US$10/month in eligible regions and supports GitHub OIDC, so no PFX, client secret, or exportable private key is needed. The provider-specific workflow should use a protected GitHub environment, `id-token: write` only in the signing job, least-privilege Azure RBAC, and an exact repository/tag federated identity.

### OV/EV cloud HSM

Use only when independent publisher identity or procurement requirements justify the higher annual cost. EV no longer guarantees immediate SmartScreen bypass. Avoid USB-token automation on a general-purpose runner; if required, use a physically controlled self-hosted runner dedicated to protected release jobs.

## GitHub Actions onboarding checklist

1. Protect the release environment and require human approval.
2. Add provider authentication/setup before the Windows bundle build.
3. Keep signing credentials unavailable to pull requests and forks.
4. Supply the provider adapter path, exact certificate subject, and out-of-band verified public-key SHA-256 identity to `publish-bundle.ps1`.
5. Set `CI_RELEASE=true`; this forbids `-SkipSign` and `-SkipPrivacyGate`.
6. Sign the Windows engine/CLI/TUI PEs before packaging their archives; current workflow artifacts are deliberately named `unsigned-tools-*` and excluded from publication.
7. Move `contents: write` into a separate final publication job; ordinary build/sign jobs remain read-only.
8. Publish only after the common signature checks, privacy scan, and checksums pass.
9. Test a disposable prerelease tag on a clean Windows 11 VM with Smart App Control evaluation enabled.

Do not store a base64 PFX and password in ordinary repository secrets unless no managed-key option exists. Prefer OIDC or a provider-held non-exportable key.

## Verification commands

```powershell
signtool verify /pa /all /v platforms\windows\dist\installer\FileIDSetup.exe
signtool verify /pa /all /v platforms\windows\dist\installer\FileID-x64.msi
signtool verify /pa /all /v platforms\windows\dist\installer\FileID-arm64.msi
Get-AuthenticodeSignature platforms\windows\dist\installer\FileIDSetup.exe | Format-List *
```

Signing establishes publisher identity and tamper evidence. SmartScreen reputation is separate and cannot be guaranteed for a brand-new file hash, even with EV signing.

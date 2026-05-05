// WinVerifyTrustChecker — Authenticode integrity check on the engine binary.
//
// Mirror of macOS EngineClient.swift's SecCode/SecStaticCode validation. On
// every spawn, the app verifies that FileIDEngine.exe's Authenticode chain
// is intact AND chains to a publisher we trust. Refuses to spawn on
// mismatch — same threat model as macOS (a malicious replacement engine
// next to FileID.exe should not be loadable).
//
// Phase 1: the check runs and surfaces the verdict via the
// IntegrityVerdict enum. For dev builds (unsigned binaries) the verdict
// is `Unsigned`; the EngineClient logs a warning and proceeds. Phase 11
// (ship) tightens this: signed releases must verify against an EV cert
// thumbprint we pin here.
//
// References:
//   docs.microsoft.com/en-us/windows/win32/api/wintrust/nf-wintrust-winverifytrust

using System.Runtime.InteropServices;

namespace FileID.Services;

internal enum IntegrityVerdict
{
    /// <summary>Signature present, chain valid, publisher trusted (or skipped per policy).</summary>
    Trusted,

    /// <summary>Binary is unsigned. Acceptable in dev builds; rejected on shipped EV-signed releases.</summary>
    Unsigned,

    /// <summary>Signature is present but failed verification (revoked cert, tamper, etc).</summary>
    Untrusted,

    /// <summary>The file does not exist or cannot be opened.</summary>
    NotFound,
}

internal static class WinVerifyTrustChecker
{
    /// <summary>
    /// Verify the Authenticode signature on a file. The optional
    /// <paramref name="expectedThumbprintHex"/> pins the publisher cert SHA-1
    /// thumbprint; pass null to accept any trusted publisher. For Phase 1 we
    /// don't have an EV cert yet, so call sites pass null and act on the
    /// `Trusted` / `Unsigned` distinction; Phase 11 supplies the thumbprint.
    /// </summary>
    public static IntegrityVerdict Verify(string path, string? expectedThumbprintHex = null)
    {
        if (!System.IO.File.Exists(path))
        {
            return IntegrityVerdict.NotFound;
        }

        // SEC-4: WTD_REVOCATION_CHECK_CHAIN in dwProvFlags is a no-op
        // unless fdwRevocationChecks asks for it. The previous version
        // had REVOKE_NONE so revocation never actually ran. Set
        // WHOLECHAIN to validate every cert in the chain (including the
        // root CA) against published CRL/OCSP. This is what blocks a
        // signed-but-revoked binary from spawning.
        var fileInfo = new WinTrustFileInfo
        {
            cbStruct = (uint)Marshal.SizeOf<WinTrustFileInfo>(),
            pszFilePath = path,
            hFile = IntPtr.Zero,
            pgKnownSubject = IntPtr.Zero,
        };

        IntPtr fileInfoPtr = Marshal.AllocHGlobal((int)fileInfo.cbStruct);
        IntPtr trustDataPtr = IntPtr.Zero;
        try
        {
            Marshal.StructureToPtr(fileInfo, fileInfoPtr, fDeleteOld: false);

            var trustData = new WinTrustData
            {
                cbStruct = (uint)Marshal.SizeOf<WinTrustData>(),
                pPolicyCallbackData = IntPtr.Zero,
                pSIPClientData = IntPtr.Zero,
                dwUIChoice = WTD_UI_NONE,
                fdwRevocationChecks = WTD_REVOKE_WHOLECHAIN,
                dwUnionChoice = WTD_CHOICE_FILE,
                pInfoStruct = fileInfoPtr,
                dwStateAction = WTD_STATEACTION_VERIFY,
                hWVTStateData = IntPtr.Zero,
                pwszURLReference = null,
                dwProvFlags = WTD_REVOCATION_CHECK_CHAIN,
                dwUIContext = 0,
                pSignatureSettings = IntPtr.Zero,
            };
            trustDataPtr = Marshal.AllocHGlobal((int)trustData.cbStruct);
            Marshal.StructureToPtr(trustData, trustDataPtr, fDeleteOld: false);

            int hr = NativeWinVerifyTrust(IntPtr.Zero, ref WINTRUST_ACTION_GENERIC_VERIFY_V2, trustDataPtr);

            // Always issue a Close to release the WVT state, regardless of hr.
            trustData.dwStateAction = WTD_STATEACTION_CLOSE;
            Marshal.StructureToPtr(trustData, trustDataPtr, fDeleteOld: true);
            _ = NativeWinVerifyTrust(IntPtr.Zero, ref WINTRUST_ACTION_GENERIC_VERIFY_V2, trustDataPtr);

            return InterpretResult(hr, expectedThumbprintHex, path);
        }
        finally
        {
            if (trustDataPtr != IntPtr.Zero) Marshal.FreeHGlobal(trustDataPtr);
            Marshal.FreeHGlobal(fileInfoPtr);
        }
    }

    private static IntegrityVerdict InterpretResult(int hr, string? expectedThumbprintHex, string path)
    {
        // S_OK (0) = Trusted. TRUST_E_NOSIGNATURE = Unsigned.
        // Anything else = Untrusted (revoked, tampered, expired, etc).
        const int TRUST_E_NOSIGNATURE = unchecked((int)0x800B0100);
        const int TRUST_E_BAD_DIGEST  = unchecked((int)0x80096010);

        if (hr == 0)
        {
            // Optional cert-pinning. If a thumbprint is supplied, walk the
            // certificate chain and confirm the leaf matches.
            if (!string.IsNullOrWhiteSpace(expectedThumbprintHex))
            {
                if (!CertificateThumbprintMatches(path, expectedThumbprintHex))
                {
                    DebugLog.Warn($"WinVerifyTrust: signed but thumbprint mismatch for {PathRedactor.Redact(path)}");
                    return IntegrityVerdict.Untrusted;
                }
            }
            return IntegrityVerdict.Trusted;
        }
        if (hr == TRUST_E_NOSIGNATURE)
        {
            return IntegrityVerdict.Unsigned;
        }
        DebugLog.Warn($"WinVerifyTrust: hr=0x{hr:X8} for {PathRedactor.Redact(path)}");
        if (hr == TRUST_E_BAD_DIGEST)
        {
            return IntegrityVerdict.Untrusted;
        }
        return IntegrityVerdict.Untrusted;
    }

    private static bool CertificateThumbprintMatches(string path, string expectedHex)
    {
        try
        {
            var cert = System.Security.Cryptography.X509Certificates.X509Certificate.CreateFromSignedFile(path);
            using var cert2 = new System.Security.Cryptography.X509Certificates.X509Certificate2(cert);
            var actualHex = cert2.Thumbprint;
            return string.Equals(actualHex, expectedHex.Replace(" ", "").Replace(":", ""), StringComparison.OrdinalIgnoreCase);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Cert thumbprint check failed: " + ex.Message);
            return false;
        }
    }

    // ─── Win32 interop ─────────────────────────────────────────────────────

    private const uint WTD_UI_NONE                = 2;
    private const uint WTD_REVOKE_NONE            = 0;
    private const uint WTD_REVOKE_WHOLECHAIN      = 1; // SEC-4: actually checks revocation
    private const uint WTD_CHOICE_FILE            = 1;
    private const uint WTD_STATEACTION_VERIFY     = 1;
    private const uint WTD_STATEACTION_CLOSE      = 2;
    private const uint WTD_REVOCATION_CHECK_CHAIN = 0x00000040;

    private static Guid WINTRUST_ACTION_GENERIC_VERIFY_V2 = new("00AAC56B-CD44-11D0-8CC2-00C04FC295EE");

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WinTrustFileInfo
    {
        public uint cbStruct;
        [MarshalAs(UnmanagedType.LPWStr)] public string pszFilePath;
        public IntPtr hFile;
        public IntPtr pgKnownSubject;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WinTrustData
    {
        public uint cbStruct;
        public IntPtr pPolicyCallbackData;
        public IntPtr pSIPClientData;
        public uint dwUIChoice;
        public uint fdwRevocationChecks;
        public uint dwUnionChoice;
        public IntPtr pInfoStruct;
        public uint dwStateAction;
        public IntPtr hWVTStateData;
        [MarshalAs(UnmanagedType.LPWStr)] public string? pwszURLReference;
        public uint dwProvFlags;
        public uint dwUIContext;
        public IntPtr pSignatureSettings;
    }

    [DllImport("wintrust.dll", EntryPoint = "WinVerifyTrust", CharSet = CharSet.Unicode, SetLastError = false)]
    private static extern int NativeWinVerifyTrust(IntPtr hWnd, ref Guid pgActionID, IntPtr pWVTData);
}

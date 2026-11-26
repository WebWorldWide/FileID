// Authenticode integrity check on the engine binary, run on every spawn: the signature chain must be intact and chain to a trusted publisher, so a replacement engine placed next to FileID.exe is refused.
// Dev builds are unsigned, so the verdict is `Unsigned` and EngineClient logs a warning and proceeds; signed releases verify against a publisher subject embedded at build time.

using System.Runtime.InteropServices;

namespace FileID.Services;

internal enum IntegrityVerdict
{
    Trusted,

    Unsigned,

    Untrusted,

    NotFound,
}

internal static class WinVerifyTrustChecker
{
    /// <summary>Verifies a file's Authenticode signature. The optional thumbprint pins the leaf certificate, the signer subject checks publisher identity, and the public-key hash binds companion FileID artifacts to the same signer.</summary>
    public static IntegrityVerdict Verify(
        string path,
        string? expectedThumbprintHex = null,
        string? expectedSignerSubject = null,
        string? expectedSignerPublicKeySha256 = null)
    {
        // File.Exists is wrapped because invalid-char paths or a denied parent ACL would throw.
        bool exists;
        try { exists = System.IO.File.Exists(path); }
        catch (System.IO.IOException) { exists = false; }
        catch (System.UnauthorizedAccessException) { exists = false; }
        if (!exists)
        {
            return IntegrityVerdict.NotFound;
        }

        // WTD_REVOCATION_CHECK_CHAIN in dwProvFlags does nothing unless fdwRevocationChecks asks for it. WHOLECHAIN validates every cert in the chain (including the root) so a signed-but-revoked binary can't spawn.
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
                // Revocation uses only locally cached CRL/OCSP data and never touches the network (the product makes no extra egress); a live fetch also blocked launch when offline or the CA was slow.
                dwProvFlags = WTD_REVOCATION_CHECK_CHAIN | WTD_CACHE_ONLY_URL_RETRIEVAL,
                dwUIContext = 0,
                pSignatureSettings = IntPtr.Zero,
            };
            trustDataPtr = Marshal.AllocHGlobal((int)trustData.cbStruct);
            Marshal.StructureToPtr(trustData, trustDataPtr, fDeleteOld: false);

            int hr = NativeWinVerifyTrust(IntPtr.Zero, ref WINTRUST_ACTION_GENERIC_VERIFY_V2, trustDataPtr);

            // Always Close to release the WVT state. Read the native struct back first: VERIFY wrote its state handle into native memory, and re-marshaling the stale managed copy (handle still zero) would make CLOSE receive NULL and leak the state on every spawn.
            var afterVerify = Marshal.PtrToStructure<WinTrustData>(trustDataPtr);
            afterVerify.dwStateAction = WTD_STATEACTION_CLOSE;
            Marshal.StructureToPtr(afterVerify, trustDataPtr, fDeleteOld: true);
            _ = NativeWinVerifyTrust(IntPtr.Zero, ref WINTRUST_ACTION_GENERIC_VERIFY_V2, trustDataPtr);

            return InterpretResult(
                hr,
                expectedThumbprintHex,
                expectedSignerSubject,
                expectedSignerPublicKeySha256,
                path);
        }
        finally
        {
            if (trustDataPtr != IntPtr.Zero) Marshal.FreeHGlobal(trustDataPtr);
            if (fileInfoPtr != IntPtr.Zero)
            {
                Marshal.DestroyStructure<WinTrustFileInfo>(fileInfoPtr);
                Marshal.FreeHGlobal(fileInfoPtr);
            }
        }
    }

    private static IntegrityVerdict InterpretResult(
        int hr,
        string? expectedThumbprintHex,
        string? expectedSignerSubject,
        string? expectedSignerPublicKeySha256,
        string path)
    {
        // S_OK is Trusted, TRUST_E_NOSIGNATURE is Unsigned, anything else (revoked, tampered, expired) is Untrusted.
        const int TRUST_E_NOSIGNATURE = unchecked((int)0x800B0100);
        const int TRUST_E_BAD_DIGEST = unchecked((int)0x80096010);

        if (hr == 0)
        {
            // Optional cert pinning: the leaf certificate must match the supplied thumbprint.
            if (!string.IsNullOrWhiteSpace(expectedThumbprintHex)
                && !CertificateThumbprintMatches(path, expectedThumbprintHex))
            {
                DebugLog.Warn($"WinVerifyTrust: signed but thumbprint mismatch for {PathRedactor.Redact(path)}");
                return IntegrityVerdict.Untrusted;
            }
            if (!string.IsNullOrWhiteSpace(expectedSignerSubject)
                && !CertificateSubjectMatches(path, expectedSignerSubject))
            {
                DebugLog.Warn($"WinVerifyTrust: signed but publisher mismatch for {PathRedactor.Redact(path)}");
                return IntegrityVerdict.Untrusted;
            }
            if (!string.IsNullOrWhiteSpace(expectedSignerPublicKeySha256)
                && !string.Equals(
                    GetSignerPublicKeySha256(path),
                    NormalizeHex(expectedSignerPublicKeySha256),
                    StringComparison.OrdinalIgnoreCase))
            {
                DebugLog.Warn($"WinVerifyTrust: signed but signer public key mismatch for {PathRedactor.Redact(path)}");
                return IntegrityVerdict.Untrusted;
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

    // X509CertificateLoader cannot read the signer out of an Authenticode-signed binary;
    // SYSLIB0057's guidance for CreateFromSignedFile is a scoped suppression. The chain itself
    // is verified by WinVerifyTrust before any of these identity checks run.
#pragma warning disable SYSLIB0057
    private static System.Security.Cryptography.X509Certificates.X509Certificate SignerCertificate(string path) =>
        System.Security.Cryptography.X509Certificates.X509Certificate.CreateFromSignedFile(path);
#pragma warning restore SYSLIB0057

    private static bool CertificateThumbprintMatches(string path, string expectedHex)
    {
        try
        {
            using var cert = SignerCertificate(path);
            using var cert2 = new System.Security.Cryptography.X509Certificates.X509Certificate2(cert);
            var actualHex = cert2.Thumbprint;
            return string.Equals(actualHex, NormalizeHex(expectedHex), StringComparison.OrdinalIgnoreCase);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Cert thumbprint check failed: " + ex.Message);
            return false;
        }
    }

    private static bool CertificateSubjectMatches(string path, string expectedSubject)
    {
        try
        {
            using var cert = SignerCertificate(path);
            using var cert2 = new System.Security.Cryptography.X509Certificates.X509Certificate2(cert);
            return string.Equals(cert2.Subject, expectedSubject.Trim(), StringComparison.OrdinalIgnoreCase);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Cert subject check failed: " + ex.Message);
            return false;
        }
    }

    internal static string? GetSignerPublicKeySha256(string path)
    {
        try
        {
            using var cert = SignerCertificate(path);
            using var cert2 = new System.Security.Cryptography.X509Certificates.X509Certificate2(cert);
            var publicKeyIdentity = string.Join(
                '|',
                cert2.PublicKey.Oid.Value,
                Convert.ToBase64String(cert2.PublicKey.EncodedParameters?.RawData ?? []),
                Convert.ToBase64String(cert2.PublicKey.EncodedKeyValue.RawData));
            return Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(
                System.Text.Encoding.UTF8.GetBytes(publicKeyIdentity)));
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Cert public-key check failed: " + ex.Message);
            return null;
        }
    }

    private static string NormalizeHex(string value)
        => value.Replace(" ", "", StringComparison.Ordinal)
            .Replace(":", "", StringComparison.Ordinal)
            .Trim();

    private const uint WTD_UI_NONE = 2;
    private const uint WTD_REVOKE_WHOLECHAIN = 1;
    private const uint WTD_CHOICE_FILE = 1;
    private const uint WTD_STATEACTION_VERIFY = 1;
    private const uint WTD_STATEACTION_CLOSE = 2;
    private const uint WTD_REVOCATION_CHECK_CHAIN = 0x00000040;
    private const uint WTD_CACHE_ONLY_URL_RETRIEVAL = 0x00001000; // revocation: cached data only, no network

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

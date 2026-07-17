import AppKit
import FileIDShared

@MainActor
enum ModelLicenseGate {
    static func ensureAccepted(for kind: AIModelKind) -> Bool {
        guard let termsURL = kind.licenseTermsURL else { return true }
        if ModelLicenseAcceptance.isAccepted(for: kind) { return true }
        let key = "modelLicenseAccepted.\(kind.licensePolicyKey).\(ModelLicenseAcceptance.reviewedAt)"
        if UserDefaults.standard.bool(forKey: key) {
            do {
                try ModelLicenseAcceptance.recordAcceptance(for: kind)
                return true
            } catch {
                return showPersistenceFailure(error)
            }
        }

        while true {
            let alert = NSAlert()
            alert.alertStyle = .warning
            alert.messageText = "License acceptance required"
            alert.informativeText = "This optional download is governed by the \(kind.licenseName), not FileID's Apache-2.0 license. Review the terms before downloading. Acceptance is recorded only on this Mac."
            alert.addButton(withTitle: "Cancel")
            alert.addButton(withTitle: "I Accept and Download")
            alert.addButton(withTitle: "Review Full Terms")
            switch alert.runModal() {
            case .alertSecondButtonReturn:
                do {
                    try ModelLicenseAcceptance.recordAcceptance(for: kind)
                    UserDefaults.standard.set(true, forKey: key)
                    return true
                } catch {
                    return showPersistenceFailure(error)
                }
            case .alertThirdButtonReturn:
                NSWorkspace.shared.open(termsURL)
            default:
                return false
            }
        }
    }

    private static func showPersistenceFailure(_ error: Error) -> Bool {
        let alert = NSAlert()
        alert.alertStyle = .critical
        alert.messageText = "License acceptance was not saved"
        alert.informativeText = "FileID did not start the download because acceptance could not be stored locally. Check permissions for FileID's Application Support folder and try again.\n\n\(error.localizedDescription)"
        alert.addButton(withTitle: "OK")
        alert.runModal()
        return false
    }
}

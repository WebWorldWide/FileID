import FileIDShared

/// Decides whether a bulk-action reply answers one waiter. A reply carrying
/// the id of the command it answers goes only to the waiter that sent that
/// command; a reply without an id (an older engine), or a waiter that never
/// learned its command's id, falls back to the action-name prefix.
struct BulkResultMatcher: Sendable {
    let actionPrefix: String
    let sentID: String?

    init(actionPrefix: String, sentID: String? = nil) {
        self.actionPrefix = actionPrefix
        self.sentID = sentID?.isEmpty == true ? nil : sentID
    }

    func accepts(action: String, commandID: String?) -> Bool {
        if let commandID, !commandID.isEmpty, let sentID { return commandID == sentID }
        return action.hasPrefix(actionPrefix)
    }

    func accepts(_ result: BulkActionResult) -> Bool {
        accepts(action: result.action, commandID: result.commandID)
    }
}

import FileIDShared

/// Matches a reply to its waiter by command id, falling back to the action-name prefix for older engines.
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

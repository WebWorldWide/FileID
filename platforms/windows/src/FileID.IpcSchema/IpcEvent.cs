// IPCEvent envelope. Mirror of the Swift `IPCEvent { t, payload }` Codable
// struct. The `t` field is the timestamp the engine emitted the event,
// serialized as ISO8601 (System.Text.Json's default for DateTimeOffset).
//
// Single positional constructor only — System.Text.Json can't pick between
// multiple constructors without [JsonConstructor], and adding that to a
// positional record's synthesized constructor isn't ergonomic. Convenience
// "stamp now" path lives as a static factory.

namespace FileID.IpcSchema;

public sealed record IpcEvent(DateTimeOffset T, EventPayload Payload)
{
    /// <summary>
    /// Build an event whose timestamp is the current UTC time.
    /// </summary>
    public static IpcEvent Now(EventPayload payload) =>
        new(DateTimeOffset.UtcNow, payload);
}

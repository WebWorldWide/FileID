// IPCCommand envelope. Mirror of the Swift `IPCCommand { id, payload }`
// Codable struct. The `id` is an app-assigned UUID; the engine echoes it
// in any reply so the app can correlate.
//
// Single positional constructor only — System.Text.Json picks the
// canonical constructor unambiguously. Convenience "auto-uuid" path lives
// as a static factory.

namespace FileID.IpcSchema;

public sealed record IpcCommand(string Id, CommandPayload Payload)
{
    /// <summary>
    /// Build a command with an auto-generated UUID. Mirror of Swift's
    /// `IPCCommand(payload:)` default-id parameter.
    /// </summary>
    public static IpcCommand New(CommandPayload payload) =>
        new(Guid.NewGuid().ToString().ToLowerInvariant(), payload);
}

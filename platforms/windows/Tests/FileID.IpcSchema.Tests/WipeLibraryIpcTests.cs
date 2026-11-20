using FileID.IpcSchema;
using Xunit;

namespace FileID.IpcSchema.Tests;

// Both must round-trip in the externally-tagged shape the Swift/ Rust generators agree on, or the wipe flow silently breaks.
public class WipeLibraryIpcTests
{
    [Fact]
    public void WipeLibraryCommand_SerializesAsEmptyObject_AndRoundTrips()
    {
        var cmd = new IpcCommand(System.Guid.NewGuid().ToString(), new WipeLibraryCommand());
        var json = IpcCoder.Encode(cmd);
        // Empty-payload variant must be `{}`, never `null`.
        Assert.Contains("\"wipeLibrary\":{}", json.Replace(" ", ""));
        var back = IpcCoder.Decode<IpcCommand>(json);
        Assert.IsType<WipeLibraryCommand>(back.Payload);
    }

    [Fact]
    public void LibraryWipedEvent_Ok_RoundTrips()
    {
        var ev = new IpcEvent(System.DateTimeOffset.UtcNow, new LibraryWipedEvent(new LibraryWiped(true)));
        var json = IpcCoder.Encode(ev);
        Assert.Contains("\"_0\"", json);
        var back = IpcCoder.Decode<IpcEvent>(json);
        var payload = Assert.IsType<LibraryWipedEvent>(back.Payload);
        Assert.True(payload.Result.Ok);
        Assert.Null(payload.Result.Message);
    }

    [Fact]
    public void LibraryWipedEvent_Failure_CarriesMessage()
    {
        var ev = new IpcEvent(System.DateTimeOffset.UtcNow,
            new LibraryWipedEvent(new LibraryWiped(false, "disk full")));
        var json = IpcCoder.Encode(ev);
        var back = IpcCoder.Decode<IpcEvent>(json);
        var payload = Assert.IsType<LibraryWipedEvent>(back.Payload);
        Assert.False(payload.Result.Ok);
        Assert.Equal("disk full", payload.Result.Message);
    }
}

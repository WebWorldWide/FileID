using System.Text.Json;
using Xunit;

namespace FileID.IpcSchema.Tests;

public class HealthCheckIpcTests
{
    [Fact]
    public void CommandPreservesNonceAndExactRequestIDCasing()
    {
        const string nonce = "generation-4_probe-2";
        var json = IpcCoder.Encode(new IpcCommand("envelope-id", new HealthCheckCommand(nonce)));
        using var document = JsonDocument.Parse(json);
        var body = document.RootElement.GetProperty("payload").GetProperty("healthCheck");
        Assert.Equal(nonce, body.GetProperty("requestID").GetString());
        Assert.False(body.TryGetProperty("requestId", out _));
        Assert.Equal(nonce, Assert.IsType<HealthCheckCommand>(IpcCoder.Decode<IpcCommand>(json).Payload).RequestId);
    }

    [Fact]
    public void ReplyPreservesNonceAndProcessInSinglePositionalWrapper()
    {
        var result = new HealthCheckResult("generation-4_probe-2", 4242);
        var json = IpcCoder.Encode(IpcEvent.Now(new HealthCheckResultEvent(result)));
        using var document = JsonDocument.Parse(json);
        var body = document.RootElement.GetProperty("payload").GetProperty("healthCheckResult").GetProperty("_0");
        Assert.Equal(result.RequestId, body.GetProperty("requestID").GetString());
        Assert.Equal(result.Pid, body.GetProperty("pid").GetInt32());
        Assert.Equal(result, Assert.IsType<HealthCheckResultEvent>(IpcCoder.Decode<IpcEvent>(json).Payload).Result);
    }
}

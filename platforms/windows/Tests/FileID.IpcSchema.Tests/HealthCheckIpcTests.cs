using System.Text.Json;
using Xunit;

namespace FileID.IpcSchema.Tests;

public class HealthCheckIpcTests
{
    [Fact]
    public void CommandNonceUsesCanonicalRequestIDName()
    {
        var command = new HealthCheckCommand("nonce-a");
        var json = IpcCoder.Encode<CommandPayload>(command);
        Assert.Contains("\"healthCheck\":{\"requestID\":\"nonce-a\"}", json);
        Assert.Equal(command, Assert.IsType<HealthCheckCommand>(IpcCoder.Decode<CommandPayload>(json)));
    }

    [Fact]
    public void ResultWrapsExactNonceAndProcessId()
    {
        var result = new HealthCheckResult("nonce-a", 42);
        var json = IpcCoder.Encode(IpcEvent.Now(new HealthCheckResultEvent(result)));
        Assert.Contains("\"healthCheckResult\":{\"_0\":{\"requestID\":\"nonce-a\",\"pid\":42}}", json);
        Assert.Equal(result, Assert.IsType<HealthCheckResultEvent>(IpcCoder.Decode<IpcEvent>(json).Payload).Result);
    }

    [Theory]
    [InlineData("{\"healthCheck\":{}}")]
    [InlineData("{\"healthCheckResult\":{\"_0\":{\"pid\":42}}}")]
    [InlineData("{\"healthCheckResult\":{\"_0\":{\"requestID\":\"nonce-a\"}}}")]
    public void MissingIdentityFieldsAreRejected(string json)
    {
        if (json.StartsWith("{\"healthCheck\":", StringComparison.Ordinal))
            Assert.Throws<JsonException>(() => IpcCoder.Decode<CommandPayload>(json));
        else
            Assert.Throws<JsonException>(() => IpcCoder.Decode<EventPayload>(json));
    }
}

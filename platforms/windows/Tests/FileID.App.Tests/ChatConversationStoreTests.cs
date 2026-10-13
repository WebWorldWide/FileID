using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public sealed class ChatConversationStoreTests
{
    [Fact]
    public async Task ConversationIDPersistsAcrossPanelAndAppSessions()
    {
        var directory = Path.Combine(Path.GetTempPath(), "FileID-ChatConversationStore-" + Guid.NewGuid());
        var path = Path.Combine(directory, "conversation.txt");
        try
        {
            var first = await ChatConversationStore.LoadOrCreateAsync(path);
            Assert.True(Guid.TryParse(first, out _));
            Assert.Equal(first, await ChatConversationStore.LoadOrCreateAsync(path));
            Assert.Single(Directory.GetFiles(directory));
        }
        finally { if (Directory.Exists(directory)) Directory.Delete(directory, true); }
    }

    [Theory]
    [InlineData(128, true)]
    [InlineData(129, false)]
    public async Task BoundedIdentityReadAcceptsLimitAndPreservesOversizedValidLookingFile(int size, bool accepted)
    {
        var directory = Path.Combine(Path.GetTempPath(), "FileID-ChatConversationStore-" + Guid.NewGuid());
        var path = Path.Combine(directory, "conversation.txt");
        Directory.CreateDirectory(directory);
        var id = Guid.NewGuid().ToString();
        var contents = id.PadRight(size);
        try
        {
            await File.WriteAllTextAsync(path, contents);
            if (accepted) Assert.Equal(id, await ChatConversationStore.LoadOrCreateAsync(path));
            else await Assert.ThrowsAsync<InvalidOperationException>(() => ChatConversationStore.LoadOrCreateAsync(path));
            Assert.Equal(contents, await File.ReadAllTextAsync(path));
        }
        finally { Directory.Delete(directory, true); }
    }

    [Fact]
    public async Task LargeCorruptIdentityIsRejectedWithoutReplacingOrTruncatingFile()
    {
        var directory = Path.Combine(Path.GetTempPath(), "FileID-ChatConversationStore-" + Guid.NewGuid());
        var path = Path.Combine(directory, "conversation.txt");
        Directory.CreateDirectory(directory);
        const long size = 64 * 1024 * 1024;
        try
        {
            using (var file = File.Create(path))
            {
                file.SetLength(size);
                file.WriteByte(0x41);
            }
            var error = await Assert.ThrowsAsync<InvalidOperationException>(() => ChatConversationStore.LoadOrCreateAsync(path));
            Assert.Contains("size limit", error.Message, StringComparison.Ordinal);
            using var preserved = File.OpenRead(path);
            Assert.Equal(size, preserved.Length);
            Assert.Equal(0x41, preserved.ReadByte());
            Assert.Single(Directory.GetFiles(directory));
        }
        finally { Directory.Delete(directory, true); }
    }

    [Fact]
    public async Task CorruptConversationIDIsNotSilentlyReplacedOrHistoryLost()
    {
        var directory = Path.Combine(Path.GetTempPath(), "FileID-ChatConversationStore-" + Guid.NewGuid());
        var path = Path.Combine(directory, "conversation.txt");
        Directory.CreateDirectory(directory);
        try
        {
            await File.WriteAllTextAsync(path, "invalid saved ID");
            await Assert.ThrowsAsync<InvalidOperationException>(() => ChatConversationStore.LoadOrCreateAsync(path));
            Assert.Equal("invalid saved ID", await File.ReadAllTextAsync(path));
        }
        finally { Directory.Delete(directory, true); }
    }
}

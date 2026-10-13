using System.IO;
using System.Text;

namespace FileID.Services;

internal static class ChatConversationStore
{
    internal const int MaxIdentityBytes = 128;
    public static async Task<string> LoadOrCreateAsync(string path, CancellationToken ct = default)
    {
        if (File.Exists(path))
        {
            await using var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read,
                bufferSize: MaxIdentityBytes + 1, FileOptions.Asynchronous);
            var bytes = new byte[MaxIdentityBytes + 1];
            var count = 0;
            while (count < bytes.Length)
            {
                var read = await file.ReadAsync(bytes.AsMemory(count), ct).ConfigureAwait(false);
                if (read == 0) break;
                count += read;
            }
            if (count > MaxIdentityBytes)
                throw new InvalidOperationException("The saved chat conversation ID exceeds its size limit. Restore the conversation settings file before continuing; it has not been changed.");
            var saved = Encoding.UTF8.GetString(bytes, 0, count).TrimStart('\uFEFF').Trim();
            if (Guid.TryParse(saved, out var id)) return id.ToString();
            throw new InvalidOperationException("The saved chat conversation ID is invalid. Restore the conversation settings file before continuing.");
        }
        var conversationID = Guid.NewGuid().ToString();
        Directory.CreateDirectory(Path.GetDirectoryName(path) ?? throw new ArgumentException("Choose an absolute conversation settings path.", nameof(path)));
        var temporary = path + "." + Guid.NewGuid().ToString() + ".tmp";
        try
        {
            await File.WriteAllTextAsync(temporary, conversationID, ct).ConfigureAwait(false);
            File.Move(temporary, path, overwrite: false);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
        return conversationID;
    }
}

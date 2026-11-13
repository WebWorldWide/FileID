using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public sealed class EngineErrorTextTests
{
    [Theory]
    [InlineData("discovery_partial", "Some files were left out")]
    [InlineData("empty_folder", "No files to scan")]
    [InlineData("scan_failed", "The scan stopped")]
    [InlineData("db_unavailable", "The library couldn't be opened")]
    [InlineData("db_open_failed", "The library couldn't be opened")]
    [InlineData("db_newer_than_engine", "This library needs a newer FileID")]
    [InlineData("folder_access_denied", "FileID can't reach that folder")]
    [InlineData("command_decode_failed", "FileID couldn't do that")]
    [InlineData("plan_restructure_failed", "The folder plan didn't finish")]
    [InlineData("model_load_failed", "An AI model didn't load")]
    [InlineData("restructure_already_running", "A folder move is already running")]
    [InlineData("engine_exited", "FileID's helper stopped")]
    [InlineData("ipc_frame_too_large", "Too much to show at once")]
    [InlineData("prewarm_failed", "The AI model couldn't be used")]
    [InlineData("gpu_device_removed", "The graphics card stopped responding")]
    [InlineData("deep_analyze_failed", "Describing files stopped")]
    [InlineData("face_clustering_failed", "Grouping faces didn't finish")]
    [InlineData("a_kind_nobody_mapped", "Something went wrong")]
    [InlineData(null, "Something went wrong")]
    public void EveryKindHasAPlainTitle(string? kind, string expected)
    {
        Assert.Equal(expected, EngineErrorText.Title(kind));
    }

    [Theory]
    [InlineData("scan_failed", "Press Scan again. Files already read are skipped.")]
    [InlineData("db_failed", "Quit and reopen FileID. If it keeps happening, use Start over in the folder menu.")]
    [InlineData("db_newer_than_engine", "This library was made by a newer FileID. Update FileID.")]
    [InlineData("bookmark_unavailable", "Choose the folder again so FileID can open it.")]
    [InlineData("plan_restructure_failed", "Open Organize Folders and try again, or pick a smaller folder.")]
    [InlineData("purge_excluded_failed", "Nothing was lost. Scan again to finish tidying up.")]
    [InlineData("unknown_model", "Open Settings and install the model again.")]
    [InlineData("restructure_busy", "Wait for it to finish, then try again.")]
    [InlineData("ipc_frame_too_large", "That folder plan is too large to show. Try a smaller folder.")]
    public void KnownKindsIgnoreTheRawMessage(string kind, string expected)
    {
        Assert.Equal(expected, EngineErrorText.Text(kind, "rusqlite::Error(SqliteFailure(code=14))"));
    }

    [Fact]
    public void AnUnmappedKindShowsThePlainSentenceTheHelperSent()
    {
        Assert.Equal(
            "The model ran out of memory on this file. Try a smaller model in Settings.",
            EngineErrorText.Text("deep_analyze_failed", " The model ran out of memory on this file. Try a smaller model in Settings. "));
    }

    [Theory]
    [InlineData("rusqlite::Error: disk I/O error")]
    [InlineData("os error 5")]
    [InlineData("failed to open C:\\Users\\sam\\Pictures\\a.jpg")]
    [InlineData("HRESULT 0x887A0005")]
    [InlineData("thread 'main' panicked")]
    [InlineData("missing field `request_id`")]
    [InlineData("")]
    [InlineData(null)]
    public void ALogLineIsReplacedByAPlainSentence(string? message)
    {
        Assert.True(EngineErrorText.LooksTechnical(message));
        Assert.Equal(EngineErrorText.TryAgain, EngineErrorText.Text("a_kind_nobody_mapped", message));
        Assert.Equal("Fallback.", EngineErrorText.Plain(message, "Fallback."));
    }

    [Fact]
    public void AVeryLongMessageIsTreatedAsALogLine()
    {
        Assert.True(EngineErrorText.LooksTechnical(new string('a', 221)));
        Assert.False(EngineErrorText.LooksTechnical("That folder is on a drive that isn't plugged in."));
    }
}

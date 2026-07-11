// Regression for the Deep Analyze "Installed" badge: the wire model_kind is
// snake_case ("mistral_small_3_2") but the engine registry installs the
// weights under a dotted dir ("vlm\mistral-small-3.2"), so probing
// vlm\<kind> reported every installed VLM as missing — the app-side twin of
// the engine's vlm::find_weights bug. VlmWeightDirs is the one mapping.

using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public class VlmWeightDirsTests
{
    [Theory]
    [InlineData("mistral_small_3_2", "mistral-small-3.2")]
    [InlineData("qwen2_5_vl_7b", "qwen2.5-vl-7b")]
    [InlineData("gemma_3_4b", "gemma-3-4b")]
    public void WireKind_MapsToRegistryDir(string kind, string dir)
    {
        Assert.Equal(dir, VlmWeightDirs.DirNameFor(kind));
    }

    [Fact]
    public void UnknownKind_PassesThrough()
    {
        // A caller already holding the dir spelling must still resolve.
        Assert.Equal("mistral-small-3.2", VlmWeightDirs.DirNameFor("mistral-small-3.2"));
    }
}

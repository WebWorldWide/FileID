using System;
using System.IO;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public class VlmWeightDirsTests
{
    [Theory]
    [InlineData("qwen3_vl_4b", "qwen3-vl-4b")]
    [InlineData("qwen3_vl_8b", "qwen3-vl-8b")]
    [InlineData("qwen2_5_vl_7b", "qwen2.5-vl-7b")]
    public void ModelCard_OnlyReportsItsOwnCompleteWeightsInstalled(string kind, string dirName)
    {
        var modelsDir = Path.Combine(Path.GetTempPath(), "fileid-vlm-" + Guid.NewGuid());
        try
        {
            var selectedDir = Path.Combine(modelsDir, "vlm", dirName);
            var otherDir = Path.Combine(modelsDir, "vlm", "gemma-3-4b");
            Directory.CreateDirectory(selectedDir);
            Directory.CreateDirectory(otherDir);
            using (var file = File.Create(Path.Combine(otherDir, "model.gguf"))) file.SetLength(1_048_576);
            using (var file = File.Create(Path.Combine(otherDir, "mmproj.gguf"))) file.SetLength(1_048_576);
            Assert.False(VlmWeightDirs.WeightsPresent(modelsDir, kind));

            using (var file = File.Create(Path.Combine(selectedDir, "model.gguf"))) file.SetLength(1_048_576);
            Assert.False(VlmWeightDirs.WeightsPresent(modelsDir, kind));

            using (var file = File.Create(Path.Combine(selectedDir, "mmproj.gguf"))) file.SetLength(1_048_575);
            Assert.False(VlmWeightDirs.WeightsPresent(modelsDir, kind));
            using (var file = File.Create(Path.Combine(selectedDir, "mmproj.gguf"))) file.SetLength(1_048_576);
            Assert.True(VlmWeightDirs.WeightsPresent(modelsDir, kind));
        }
        finally
        {
            if (Directory.Exists(modelsDir)) Directory.Delete(modelsDir, recursive: true);
        }
    }
}

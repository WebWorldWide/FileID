using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public sealed class ModelLicenseGateTests
{
    [Theory]
    [InlineData("gemma_3_4b", "Gemma")]
    [InlineData("gemma_3_12b", "Gemma")]
    [InlineData("paligemma_3b", "Gemma")]
    [InlineData("cudnn_runtime_x64", "NVIDIA-cuDNN")]
    [InlineData("llama_runtime_cuda_x64", "NVIDIA-CUDA")]
    public void RestrictedModelKindsRequireExpectedPolicy(string modelKind, string policy)
    {
        Assert.Equal(policy, ModelLicenseGate.PolicyKeyForModelKind(modelKind));
    }

    [Theory]
    [InlineData("qwen2_5_vl_7b")]
    [InlineData("mistral_small_3_2")]
    [InlineData("ort_cuda_x64")]
    [InlineData("ort_openvino_x64")]
    [InlineData("mobileclip_s2")]
    public void PermissiveModelKindsDoNotRequireAcceptance(string modelKind)
    {
        Assert.Null(ModelLicenseGate.PolicyKeyForModelKind(modelKind));
    }
}

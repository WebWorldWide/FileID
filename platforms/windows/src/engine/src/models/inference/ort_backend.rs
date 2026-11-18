//! ONNX Runtime backend of the inference seam, built only with the `ort-accel`
//! feature: it binds the execution providers (CUDA, TensorRT, OpenVINO,
//! DirectML, QNN) that tract has no counterpart for.

use std::path::Path;

use anyhow::{Context, Result};
use ort::execution_providers::{
    cuda::CUDAExecutionProvider, directml::DirectMLExecutionProvider, openvino::OpenVINOExecutionProvider,
    qnn::QNNExecutionProvider, tensorrt::TensorRTExecutionProvider, ExecutionProviderDispatch,
};
use ort::session::{builder::GraphOptimizationLevel, Session, SessionInputValue};
use ort::value::Tensor;

use super::{Input, Output};
use crate::models::runtime::{bind_chain, classify_inference_error, ExecutionProvider, RuntimeProbe};

pub struct OrtSession {
    session: Session,
    input_names: Vec<String>,
    output_names: Vec<String>,
    first_input_shape: Vec<i64>,
    backend: &'static str,
}

impl OrtSession {
    pub fn backend(&self) -> &'static str {
        self.backend
    }

    pub fn input_names(&self) -> &[String] {
        &self.input_names
    }

    pub fn output_names(&self) -> &[String] {
        &self.output_names
    }

    pub fn first_input_shape(&self) -> Vec<i64> {
        self.first_input_shape.clone()
    }

    pub fn run(&mut self, inputs: Vec<(&str, Input)>) -> Result<Vec<Output>> {
        let mut values: Vec<(String, SessionInputValue)> = Vec::with_capacity(inputs.len());
        for (name, input) in inputs {
            let value: SessionInputValue = match input {
                Input::F32(array) => Tensor::from_array(array).with_context(|| format!("input tensor {name}"))?.into(),
                Input::I64(array) => Tensor::from_array(array).with_context(|| format!("input tensor {name}"))?.into(),
            };
            values.push((name.to_owned(), value));
        }
        let outputs = self.session.run(values).context("ORT session.run")?;
        self.output_names
            .iter()
            .map(|name| {
                let (shape, data) = outputs[name.as_str()].try_extract_tensor::<f32>().with_context(|| format!("ONNX output {name:?} as f32"))?;
                Ok(Output { name: name.clone(), shape: shape.iter().map(|dim| *dim as usize).collect(), data: data.to_vec() })
            })
            .collect()
    }
}

/// An ONNX Runtime session over this process's bind chain, or `None` when the
/// chain is CPU-only (tract serves that) or ONNX Runtime cannot start. A
/// device-removed failure is returned as an error: the GPU is gone for the
/// process.
pub(in crate::models) fn chain_session(label: &str, path: &Path) -> Result<Option<OrtSession>> {
    let probe = RuntimeProbe::shared();
    let chain = bind_chain(probe);
    let Some(first) = chain.first().copied().filter(|ep| *ep != ExecutionProvider::Cpu) else {
        return Ok(None);
    };
    let labels: Vec<&'static str> = chain.iter().map(|ep| ep.as_str()).collect();
    match build(label, path, &chain, probe.adapter_index) {
        Ok(session) => {
            tracing::info!(model = label, chain = ?labels, "EP priority chain registered");
            Ok(Some(OrtSession { backend: first.as_str(), ..session }))
        }
        Err(error) => {
            let error = classify_inference_error(error);
            if crate::models::runtime::error_has_device_removed_marker(&error) {
                return Err(error);
            }
            tracing::warn!(model = label, error = %format!("{error:#}"), "ONNX Runtime could not start; running on tract (CPU)");
            Ok(None)
        }
    }
}

fn build(label: &str, path: &Path, chain: &[ExecutionProvider], adapter_index: Option<u32>) -> Result<OrtSession> {
    crate::ort_runtime::commit_environment().context("ORT environment")?;
    let mut builder = configure(Session::builder().context("ORT session builder")?, chain.first().copied())
        .with_context(|| format!("configure session ({label})"))?;
    let providers = execution_providers_for_chain(chain, adapter_index);
    if !providers.is_empty() {
        builder = builder.with_execution_providers(providers).with_context(|| format!("register execution providers ({label})"))?;
    }
    let session = builder.commit_from_file(path).with_context(|| format!("ORT session commit ({label})"))?;
    let input_names: Vec<String> = session.inputs.iter().map(|input| input.name.clone()).collect();
    anyhow::ensure!(!input_names.is_empty(), "{label} ONNX has no inputs");
    let output_names = session.outputs.iter().map(|output| output.name.clone()).collect();
    let first_input_shape = session
        .inputs
        .first()
        .and_then(|input| input.input_type.tensor_shape())
        .map(|shape| shape.iter().copied().collect())
        .unwrap_or_default();
    Ok(OrtSession { session, input_names, output_names, first_input_shape, backend: "ort" })
}

/// Session tuning for the execution provider that will actually bind first.
///
/// - **Graph optimization**: Level3 (all) except QNN, whose Hexagon (HTP)
///   partitioner wants Basic (Level1): ORT's aggressive fusion otherwise emits
///   nodes QNN can't consume and silently forces a CPU fallback.
/// - **Intra-op threads**: one on a GPU/NPU provider (the accelerator does the
///   parallelism), every performance core otherwise.
fn configure(builder: ort::session::builder::SessionBuilder, first: Option<ExecutionProvider>) -> ort::Result<ort::session::builder::SessionBuilder> {
    let first = first.unwrap_or(ExecutionProvider::Cpu);
    let optimization = if first == ExecutionProvider::Qnn { GraphOptimizationLevel::Level1 } else { GraphOptimizationLevel::Level3 };
    let intra = if first == ExecutionProvider::Cpu { crate::platform::cpu_topology().p_cores.max(1) as usize } else { 1 };
    builder
        .with_log_level(ort::logging::LogLevel::Error)?
        .with_optimization_level(optimization)?
        .with_intra_threads(intra)
}

/// Resolve a priority chain of our `ExecutionProvider` variants into the
/// dispatch values ORT's `SessionBuilder::with_execution_providers` consumes.
///
/// CPU is ORT's implicit fallback, so it emits no dispatch: a GPU provider
/// that fails to bind degrades to CPU instead of failing session creation.
fn execution_providers_for_chain(chain: &[ExecutionProvider], adapter_index: Option<u32>) -> Vec<ExecutionProviderDispatch> {
    let mut out = Vec::with_capacity(chain.len());
    for ep in chain {
        match ep {
            ExecutionProvider::Cuda => out.push(CUDAExecutionProvider::default().build()),
            ExecutionProvider::TensorRt => out.push(TensorRTExecutionProvider::default().build()),
            // AUTO:GPU,CPU targets the GPU (or NPU once detected) and falls
            // back to the OpenVINO CPU plugin only when no accelerator binds,
            // instead of OpenVINO's default possibly landing on CPU silently.
            ExecutionProvider::OpenVino => out.push(OpenVINOExecutionProvider::default().with_device_type("AUTO:GPU,CPU").build()),
            ExecutionProvider::DirectMl => {
                // DirectML's `device_id` is the DXGI adapter index, the same
                // enumeration `probe_gpu_vendor` walks: pin the highest-VRAM
                // adapter, because the default (adapter 0) is often the iGPU on
                // hybrid boxes. CUDA/TensorRT use the CUDA ordinal and stay default.
                let mut directml = DirectMLExecutionProvider::default();
                if let Some(index) = adapter_index {
                    directml = directml.with_device_id(index as i32);
                }
                out.push(directml.build());
            }
            // Snapdragon: bind the Hexagon NPU (HTP) backend explicitly.
            // `QnnHtp.dll` ships in the QNN pack and is on the DLL search path;
            // without it the provider fails to load and the chain falls to DirectML.
            ExecutionProvider::Qnn => out.push(QNNExecutionProvider::default().with_backend_path("QnnHtp.dll").build()),
            ExecutionProvider::Cpu => {}
        }
    }
    out
}

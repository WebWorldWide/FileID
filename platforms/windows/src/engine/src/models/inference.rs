//! The inference seam every ONNX model wrapper runs through: a model loaded
//! from a path, named tensors in, tensors out.
//!
//! The backend is tract (pure Rust, CPU) everywhere. A build with the
//! `ort-accel` feature can bind ONNX Runtime instead when an accelerator
//! execution provider is available (see `runtime::commit_chain_session`).
//!
//! A compiled model is shared by every [`InferenceSession`] loaded from the
//! same file with the same input contract; each session owns only its run
//! state (scratch buffers), so a worker pool costs one copy of the weights.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use anyhow::{Context, Result};
use ndarray::ArrayD;
use tract_onnx::prelude::{
    tvec, f16, Datum, Framework, InferenceFact, InferenceModelExt, IntoRunnable, TDim, TValue, Tensor, TypedModel, TypedRunnableModel, TypedSimpleState,
};
use tract_onnx::tract_hir::infer::Factoid;

#[cfg(feature = "ort-accel")]
pub(super) mod ort_backend;

/// The backend label tract sessions report.
pub const TRACT_CPU: &str = "tract-cpu";

/// One dimension of a model input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dim {
    Fixed(usize),
    /// A dimension that varies between runs, named so that inputs which share
    /// it agree (BGE's sequence length).
    Symbol(&'static str),
    /// The batch axis: symbolic when the ONNX leaves it open, the ONNX's own
    /// size when it fixes it.
    Batch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dtype {
    F32,
    I64,
}

/// The shape and element type one model input is compiled for. `name` is the
/// ONNX input name; `None` stands for a model with a single input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSpec {
    pub name: Option<&'static str>,
    pub dtype: Dtype,
    pub dims: Vec<Dim>,
}

impl InputSpec {
    pub fn f32(dims: &[Dim]) -> Self {
        Self { name: None, dtype: Dtype::F32, dims: dims.to_vec() }
    }

    pub fn i64(dims: &[Dim]) -> Self {
        Self { name: None, dtype: Dtype::I64, dims: dims.to_vec() }
    }

    pub fn named(name: &'static str, dtype: Dtype, dims: &[Dim]) -> Self {
        Self { name: Some(name), dtype, dims: dims.to_vec() }
    }
}

pub enum Input {
    F32(ArrayD<f32>),
    I64(ArrayD<i64>),
}

impl<D: ndarray::Dimension> From<ndarray::Array<f32, D>> for Input {
    fn from(array: ndarray::Array<f32, D>) -> Self {
        Self::F32(array.into_dyn())
    }
}

impl<D: ndarray::Dimension> From<ndarray::Array<i64, D>> for Input {
    fn from(array: ndarray::Array<i64, D>) -> Self {
        Self::I64(array.into_dyn())
    }
}

pub struct Output {
    pub name: String,
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

struct CompiledModel {
    plan: Arc<TypedRunnableModel>,
    input_names: Vec<String>,
    output_names: Vec<String>,
    /// The first input's shape; `-1` marks a symbolic dimension.
    first_input_shape: Vec<i64>,
}

enum Backend {
    Tract { model: Arc<CompiledModel>, state: Box<TypedSimpleState> },
    #[cfg(feature = "ort-accel")]
    Ort(ort_backend::OrtSession),
}

pub struct InferenceSession {
    backend: Backend,
}

type CacheKey = (PathBuf, u64, Option<std::time::SystemTime>, String);

static COMPILED: parking_lot::Mutex<Option<HashMap<CacheKey, Weak<CompiledModel>>>> = parking_lot::Mutex::new(None);

impl InferenceSession {
    /// Load `path` on tract. Sessions of the same file and `specs` share one
    /// compiled model.
    pub fn load_cpu(label: &str, path: &Path, specs: &[InputSpec]) -> Result<Self> {
        let metadata = std::fs::metadata(path).with_context(|| format!("{label} weights {}", path.display()))?;
        let key = (path.to_path_buf(), metadata.len(), metadata.modified().ok(), format!("{specs:?}"));
        let mut cache = COMPILED.lock();
        let cache = cache.get_or_insert_with(HashMap::new);
        cache.retain(|_, model| model.strong_count() > 0);
        let model = match cache.get(&key).and_then(Weak::upgrade) {
            Some(model) => model,
            None => {
                let started = std::time::Instant::now();
                let model = Arc::new(compile(path, specs).with_context(|| format!("tract model load ({label})"))?);
                tracing::info!(model = label, backend = TRACT_CPU, load_ms = started.elapsed().as_millis() as u64, "model compiled");
                cache.insert(key, Arc::downgrade(&model));
                model
            }
        };
        let state = Box::new(model.plan.spawn().with_context(|| format!("tract run state ({label})"))?);
        Ok(Self { backend: Backend::Tract { model, state } })
    }

    #[cfg(feature = "ort-accel")]
    pub(super) fn from_ort(session: ort_backend::OrtSession) -> Self {
        Self { backend: Backend::Ort(session) }
    }

    /// `"tract-cpu"`, or the ONNX Runtime execution provider's name.
    pub fn backend(&self) -> &'static str {
        match &self.backend {
            Backend::Tract { .. } => TRACT_CPU,
            #[cfg(feature = "ort-accel")]
            Backend::Ort(session) => session.backend(),
        }
    }

    pub fn input_names(&self) -> &[String] {
        match &self.backend {
            Backend::Tract { model, .. } => &model.input_names,
            #[cfg(feature = "ort-accel")]
            Backend::Ort(session) => session.input_names(),
        }
    }

    pub fn output_names(&self) -> &[String] {
        match &self.backend {
            Backend::Tract { model, .. } => &model.output_names,
            #[cfg(feature = "ort-accel")]
            Backend::Ort(session) => session.output_names(),
        }
    }

    /// The first input's shape with `-1` for a dimension that varies.
    pub fn first_input_shape(&self) -> Vec<i64> {
        match &self.backend {
            Backend::Tract { model, .. } => model.first_input_shape.clone(),
            #[cfg(feature = "ort-accel")]
            Backend::Ort(session) => session.first_input_shape(),
        }
    }

    /// Run the model; the outputs come back in the ONNX's output order.
    pub fn run(&mut self, inputs: Vec<(&str, Input)>) -> Result<Vec<Output>> {
        super::runtime::ensure_gpu_inference_alive()?;
        let result = match &mut self.backend {
            Backend::Tract { model, state } => run_tract(model, state, inputs),
            #[cfg(feature = "ort-accel")]
            Backend::Ort(session) => session.run(inputs),
        };
        result.map_err(super::runtime::classify_inference_error)
    }
}

fn compile(path: &Path, specs: &[InputSpec]) -> Result<CompiledModel> {
    let mut model = tract_onnx::onnx().model_for_path(path)?;
    let outlets = model.input_outlets()?.to_vec();
    let mut input_names = Vec::with_capacity(outlets.len());
    let mut first_input_shape = Vec::new();
    for (index, outlet) in outlets.iter().enumerate() {
        let name = model.outlet_label(*outlet).map_or_else(|| model.node(outlet.node).name.clone(), str::to_owned);
        let spec = specs
            .iter()
            .find(|spec| spec.name == Some(name.as_str()))
            .or_else(|| specs.iter().find(|spec| spec.name.is_none()).filter(|_| outlets.len() == 1))
            .with_context(|| format!("no input contract for ONNX input {name:?}"))?;
        let original = model.input_fact(index)?.shape.clone();
        anyhow::ensure!(
            original.rank().concretize().is_none_or(|rank| rank as usize == spec.dims.len()),
            "ONNX input {name:?} has rank {:?}, expected {}",
            original.rank(),
            spec.dims.len()
        );
        let mut dims = Vec::with_capacity(spec.dims.len());
        for (axis, dim) in spec.dims.iter().enumerate() {
            dims.push(match dim {
                Dim::Fixed(size) => TDim::from(*size),
                Dim::Symbol(symbol) => model.sym(symbol).into(),
                Dim::Batch => match original.dim(axis).and_then(|fact| fact.concretize()).and_then(|dim| dim.to_i64().ok()) {
                    Some(size) => TDim::from(size as usize),
                    None => model.sym("B").into(),
                },
            });
        }
        if index == 0 {
            first_input_shape = dims.iter().map(|dim| dim.to_i64().unwrap_or(-1)).collect();
        }
        let datum = match spec.dtype {
            Dtype::F32 => f32::datum_type(),
            Dtype::I64 => i64::datum_type(),
        };
        model.set_input_fact(index, InferenceFact::dt_shape(datum, dims))?;
        input_names.push(name);
    }
    let typed = model.into_typed()?.into_decluttered()?;
    let typed = widen_half_precision(typed)?;
    let output_names = typed
        .output_outlets()?
        .iter()
        .map(|outlet| typed.outlet_label(*outlet).map_or_else(|| typed.node(outlet.node).name.clone(), str::to_owned))
        .collect();
    let plan = typed.into_optimized()?.into_runnable()?;
    Ok(CompiledModel { plan, input_names, output_names, first_input_shape })
}

/// RAM++ ships as an f16 graph. tract's CPU f16 kernels round more than ONNX
/// Runtime's f16 path does (tags near a threshold flip), so a graph with f16
/// values runs in f32: the same weights, with the arithmetic done at full width.
fn widen_half_precision(typed: TypedModel) -> Result<TypedModel> {
    let half = f16::datum_type();
    let has_half = typed.nodes().iter().any(|node| node.outputs.iter().any(|output| output.fact.datum_type == half));
    if !has_half {
        return Ok(typed);
    }
    let widen = tract_onnx::tract_core::transform::get_transform("f16_to_f32")?.context("tract has no f16_to_f32 transform")?;
    Ok(widen.transform_into(typed)?)
}

fn run_tract(model: &CompiledModel, state: &mut TypedSimpleState, mut inputs: Vec<(&str, Input)>) -> Result<Vec<Output>> {
    let mut values = tvec!();
    for name in &model.input_names {
        let position = inputs
            .iter()
            .position(|(given, _)| given == name)
            .with_context(|| format!("no tensor supplied for ONNX input {name:?}"))?;
        let tensor = match inputs.swap_remove(position).1 {
            Input::F32(array) => Tensor::from_shape(array.shape(), array.as_standard_layout().as_slice().context("contiguous f32 input")?)?,
            Input::I64(array) => Tensor::from_shape(array.shape(), array.as_standard_layout().as_slice().context("contiguous i64 input")?)?,
        };
        values.push(TValue::from(tensor));
    }
    let outputs = state.run(values)?;
    outputs
        .iter()
        .zip(&model.output_names)
        .map(|(value, name)| {
            let view = value.to_plain_array_view::<f32>().with_context(|| format!("ONNX output {name:?} as f32"))?;
            Ok(Output { name: name.clone(), shape: value.shape().to_vec(), data: view.iter().copied().collect() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_a_load_error_not_a_panic() {
        let error = InferenceSession::load_cpu("test", Path::new("/nonexistent/model.onnx"), &[InputSpec::f32(&[Dim::Fixed(1)])]).err().unwrap();
        assert!(format!("{error:#}").contains("test weights"));
    }
}

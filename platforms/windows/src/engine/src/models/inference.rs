//! The inference seam every ONNX model wrapper runs through: a model loaded
//! from a path, named tensors in, tensors out.
//!
//! The backend is tract (pure Rust, CPU), the same on every platform.
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

pub const TRACT_CPU: &str = "tract-cpu";

const MAX_DEFAULT_MATMUL_THREADS: usize = 16;

/// Threads one matmul may use: `FILEID_TRACT_THREADS` when set (`1` keeps tract
/// single-threaded), else the machine's performance cores, never more than
/// the CPUs this process may run on (cgroup quotas included) or 16. Applied
/// once, before the first model compiles, so every platform and every entry
/// point (engine, CLI, GTK app, tests) runs the same configuration.
pub fn matmul_threads() -> usize {
    static THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *THREADS.get_or_init(|| {
        let threads = std::env::var("FILEID_TRACT_THREADS")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_else(|| {
                let allowed = std::thread::available_parallelism().map_or(usize::MAX, usize::from);
                crate::platform::performance_core_count().min(allowed).min(MAX_DEFAULT_MATMUL_THREADS)
            })
            .max(1);
        if threads > 1 {
            tract_linalg::multithread::set_default_executor(tract_linalg::multithread::Executor::multithread_with_name(threads, "fileid-tract"));
        }
        threads
    })
}

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

pub struct InferenceSession {
    model: Arc<CompiledModel>,
    state: Box<TypedSimpleState>,
}

type CacheKey = (PathBuf, u64, Option<std::time::SystemTime>, String);

/// One slot per model file and contract, so loading one model never waits for
/// another's compile.
static COMPILED: parking_lot::Mutex<Option<HashMap<CacheKey, Arc<parking_lot::Mutex<Weak<CompiledModel>>>>>> =
    parking_lot::Mutex::new(None);

impl InferenceSession {
    /// Load `path` on tract. Sessions of the same file and `specs` share one
    /// compiled model.
    pub fn load(label: &str, path: &Path, specs: &[InputSpec]) -> Result<Self> {
        matmul_threads();
        let metadata = std::fs::metadata(path).with_context(|| format!("{label} weights {}", path.display()))?;
        let key = (path.to_path_buf(), metadata.len(), metadata.modified().ok(), format!("{specs:?}"));
        let slot = {
            let mut cache = COMPILED.lock();
            let cache = cache.get_or_insert_with(HashMap::new);
            cache.retain(|_, slot| Arc::strong_count(slot) > 1 || slot.lock().strong_count() > 0);
            cache.entry(key).or_default().clone()
        };
        let mut held = slot.lock();
        let model = match held.upgrade() {
            Some(model) => model,
            None => {
                let started = std::time::Instant::now();
                let model = Arc::new(compile(path, specs).with_context(|| format!("tract model load ({label})"))?);
                tracing::info!(model = label, backend = TRACT_CPU, load_ms = started.elapsed().as_millis() as u64, "model compiled");
                *held = Arc::downgrade(&model);
                model
            }
        };
        drop(held);
        let state = Box::new(model.plan.spawn().with_context(|| format!("tract run state ({label})"))?);
        Ok(Self { model, state })
    }

    pub fn input_names(&self) -> &[String] {
        &self.model.input_names
    }

    pub fn output_names(&self) -> &[String] {
        &self.model.output_names
    }

    /// The first input's shape with `-1` for a dimension that varies.
    pub fn first_input_shape(&self) -> Vec<i64> {
        self.model.first_input_shape.clone()
    }

    /// Run the model; the outputs come back in the ONNX's output order.
    pub fn run(&mut self, inputs: Vec<(&str, Input)>) -> Result<Vec<Output>> {
        run_tract(&self.model, &mut self.state, inputs)
    }
}

/// Load `path` and return the session with the ONNX's first input name.
pub fn load_session(label: &str, path: &Path, specs: &[InputSpec]) -> Result<(InferenceSession, String)> {
    let session = InferenceSession::load(label, path, specs)?;
    let input_name = session.input_names().first().with_context(|| format!("{label} ONNX has no inputs"))?.clone();
    Ok((session, input_name))
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
    widen.transform_into(typed)
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
        let error = InferenceSession::load("test", Path::new("/nonexistent/model.onnx"), &[InputSpec::f32(&[Dim::Fixed(1)])]).err().unwrap();
        assert!(format!("{error:#}").contains("test weights"));
    }
}

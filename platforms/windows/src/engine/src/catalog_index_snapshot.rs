//! Binary snapshot of the visual search graph and of the small set of changes
//! applied on top of it. Everything is little-endian.
//!
//! Graph file: `FIDGRAPH`, version u32, dimension u32, model length u32, model
//! bytes, point count u64, then the graph in its field order with every
//! sequence prefixed by a u64 length: ef_search u64; points (each a u64
//! dimension and that many f32); the base layer (64 u32 neighbours per point);
//! the upper layers (each a u64 size and 32 u32 neighbours per node); entries
//! (i64 file id, u64 length, fingerprint bytes).
//!
//! Delta file: `FIDDELTA`, version u32, dimension u32, added count u64, removed
//! count u64, added entries (i64 file id, 64 fingerprint bytes, dimension f32),
//! removed file ids (i64).

use super::Entry;
use crate::util::hnsw_index::Embedding;
use anyhow::{ensure, Result};
use instant_distance::HnswMap;
use serde::{de, ser, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufReader, BufWriter, Read, Write};

const GRAPH_MAGIC: &[u8; 8] = b"FIDGRAPH";
const DELTA_MAGIC: &[u8; 8] = b"FIDDELTA";
pub(super) const VERSION: u32 = 2;
const ZERO_WIDTH: usize = 64;
const UPPER_WIDTH: usize = 32;
const MAX_LAYERS: u64 = 32;
const MAX_MODEL_BYTES: usize = 200;
const FINGERPRINT_BYTES: usize = 64;
const HEADER_ALLOWANCE: u64 = 4096;

/// The largest graph file `points` vectors of `dimension` can produce. Upper
/// layers shrink geometrically and never hold more nodes than the base layer.
pub(super) const fn max_graph_bytes(dimension: usize, points: usize) -> u64 {
    let point = 8 + 4 * dimension as u64;
    let zero = 4 * ZERO_WIDTH as u64;
    let upper = 4 * UPPER_WIDTH as u64;
    let entry = 8 + 8 + FINGERPRINT_BYTES as u64;
    HEADER_ALLOWANCE + points as u64 * (point + zero + upper + entry)
}

pub(super) const fn max_delta_bytes(dimension: usize, added: usize, removed: usize) -> u64 {
    HEADER_ALLOWANCE + added as u64 * (8 + FINGERPRINT_BYTES as u64 + 4 * dimension as u64) + removed as u64 * 8
}

pub(super) struct Shape<'a> {
    pub(super) model: &'a str,
    pub(super) dimension: usize,
    pub(super) max_points: usize,
    pub(super) ef_search: u64,
    pub(super) valid: &'a dyn Fn(&[f32]) -> bool,
}

#[derive(Debug)]
struct CodecError(String);
impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CodecError {}
impl ser::Error for CodecError {
    fn custom<T: std::fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}
impl de::Error for CodecError {
    fn custom<T: std::fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}

struct HashingWriter<'a, W> {
    inner: W,
    hasher: Sha256,
    written: u64,
    stop: &'a dyn Fn() -> bool,
}
impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if (self.stop)() {
            return Err(std::io::Error::other("Visual index preparation cancelled"));
        }
        let written = self.inner.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        self.written += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn finish<W: Write>(out: BufWriter<HashingWriter<'_, W>>) -> Result<(String, u64)> {
    let out = out.into_inner().map_err(|error| error.into_error())?;
    Ok((hex::encode(out.hasher.finalize()), out.written))
}

/// Streams the graph to `out`; returns the SHA-256 of the bytes and their count.
pub(super) fn write_graph<W: Write>(
    out: W,
    model: &str,
    dimension: usize,
    graph: &HnswMap<Embedding, Entry>,
    stop: &dyn Fn() -> bool,
) -> Result<(String, u64)> {
    ensure!(model.len() <= MAX_MODEL_BYTES, "Visual index model name is too long");
    let mut out = BufWriter::with_capacity(1 << 16, HashingWriter { inner: out, hasher: Sha256::new(), written: 0, stop });
    out.write_all(GRAPH_MAGIC)?;
    out.write_all(&VERSION.to_le_bytes())?;
    out.write_all(&u32::try_from(dimension)?.to_le_bytes())?;
    out.write_all(&(model.len() as u32).to_le_bytes())?;
    out.write_all(model.as_bytes())?;
    out.write_all(&(graph.values.len() as u64).to_le_bytes())?;
    graph.serialize(&mut Flat { out: &mut out }).map_err(|error| anyhow::anyhow!(error.0))?;
    finish(out)
}

pub(super) struct Delta {
    pub(super) added: Vec<(Vec<f32>, Entry)>,
    pub(super) removed: Vec<i64>,
}

pub(super) fn write_delta<W: Write>(out: W, dimension: usize, delta: &Delta, stop: &dyn Fn() -> bool) -> Result<(String, u64)> {
    let mut out = BufWriter::with_capacity(1 << 16, HashingWriter { inner: out, hasher: Sha256::new(), written: 0, stop });
    out.write_all(DELTA_MAGIC)?;
    out.write_all(&VERSION.to_le_bytes())?;
    out.write_all(&u32::try_from(dimension)?.to_le_bytes())?;
    out.write_all(&(delta.added.len() as u64).to_le_bytes())?;
    out.write_all(&(delta.removed.len() as u64).to_le_bytes())?;
    for (vector, entry) in &delta.added {
        ensure!(vector.len() == dimension && entry.fingerprint.len() == FINGERPRINT_BYTES, "Invalid visual index change");
        out.write_all(&entry.file_id.to_le_bytes())?;
        out.write_all(entry.fingerprint.as_bytes())?;
        for value in vector {
            out.write_all(&value.to_le_bytes())?;
        }
    }
    for id in &delta.removed {
        out.write_all(&id.to_le_bytes())?;
    }
    finish(out)
}

struct Source<R> {
    input: R,
    hasher: Sha256,
    remaining: u64,
}
impl<R: Read> Source<R> {
    fn bytes(&mut self, buffer: &mut [u8]) -> Result<()> {
        ensure!(buffer.len() as u64 <= self.remaining, "Truncated visual index snapshot");
        self.input.read_exact(buffer)?;
        self.hasher.update(&*buffer);
        self.remaining -= buffer.len() as u64;
        Ok(())
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut buffer = [0u8; N];
        self.bytes(&mut buffer)?;
        Ok(buffer)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }
    /// Refuses a declared size the rest of the file cannot hold, before any
    /// allocation is sized from it.
    fn holds(&self, items: u64, each: u64) -> Result<usize> {
        let bytes = items.checked_mul(each).ok_or_else(|| anyhow::anyhow!("Invalid visual index size"))?;
        ensure!(bytes <= self.remaining, "Truncated visual index snapshot");
        Ok(usize::try_from(items)?)
    }
    fn floats(&mut self, scratch: &mut [u8], out: &mut Vec<f32>) -> Result<()> {
        self.bytes(scratch)?;
        out.extend(scratch.as_chunks::<4>().0.iter().map(|bytes| f32::from_le_bytes(*bytes)));
        Ok(())
    }
    fn neighbours(&mut self, nodes: usize, width: usize, out: &mut Vec<u32>) -> Result<()> {
        let mut scratch = vec![0u8; width * 4];
        for _ in 0..nodes {
            self.bytes(&mut scratch)?;
            for bytes in scratch.as_chunks::<4>().0 {
                let id = u32::from_le_bytes(*bytes);
                ensure!(id == u32::MAX || (id as usize) < nodes, "Graph neighbor out of bounds");
                out.push(id);
            }
        }
        Ok(())
    }
    fn end(mut self) -> Result<String> {
        ensure!(self.remaining == 0, "Visual index snapshot has trailing data");
        let mut probe = [0u8; 1];
        ensure!(self.input.read(&mut probe)? == 0, "Visual index snapshot has trailing data");
        Ok(hex::encode(self.hasher.finalize()))
    }
}

fn valid_fingerprint(bytes: &[u8]) -> bool {
    bytes.len() == FINGERPRINT_BYTES && bytes.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

struct Parts {
    ef_search: u64,
    dimension: usize,
    points: Vec<f32>,
    zero: Vec<u32>,
    layers: Vec<Vec<u32>>,
    ids: Vec<i64>,
    fingerprints: Vec<String>,
}

/// Reads and validates a graph file of `length` bytes in one pass. Nothing
/// from the file reaches the graph code until every bound and the checksum
/// hold, because searching indexes neighbours without checks.
pub(super) fn read_graph<R: Read>(input: R, length: u64, expected_hash: &str, shape: &Shape<'_>) -> Result<HnswMap<Embedding, Entry>> {
    let mut source = Source { input: BufReader::with_capacity(1 << 16, input), hasher: Sha256::new(), remaining: length };
    ensure!(&source.array::<8>()? == GRAPH_MAGIC, "Not a visual index snapshot");
    ensure!(source.u32()? == VERSION, "Unsupported visual index snapshot version");
    ensure!(source.u32()? as usize == shape.dimension, "Visual index snapshot dimension mismatch");
    let model_length = source.u32()? as usize;
    ensure!(model_length <= MAX_MODEL_BYTES, "Invalid visual index model");
    let mut model = vec![0u8; model_length];
    source.bytes(&mut model)?;
    ensure!(model == shape.model.as_bytes(), "Visual index snapshot model mismatch");
    let declared = source.u64()?;
    ensure!(declared <= shape.max_points as u64, "Invalid visual graph capacity");
    let ef_search = source.u64()?;
    ensure!(ef_search == shape.ef_search, "Invalid visual graph capacity");

    let dimension = shape.dimension;
    ensure!(source.u64()? == declared, "Invalid visual graph capacity");
    let count = source.holds(declared, 8 + 4 * dimension as u64)?;
    let mut points = Vec::with_capacity(count * dimension);
    let mut scratch = vec![0u8; dimension * 4];
    for index in 0..count {
        ensure!(source.u64()? == dimension as u64, "Invalid visual graph vector");
        source.floats(&mut scratch, &mut points)?;
        ensure!((shape.valid)(&points[index * dimension..]), "Invalid visual graph vector");
    }

    ensure!(source.u64()? == declared, "Invalid graph node count");
    source.holds(declared, 4 * ZERO_WIDTH as u64)?;
    let mut zero = Vec::with_capacity(count * ZERO_WIDTH);
    source.neighbours(count, ZERO_WIDTH, &mut zero)?;

    let layer_count = source.u64()?;
    ensure!(layer_count <= MAX_LAYERS, "Invalid graph layer count");
    let mut layers = Vec::with_capacity(layer_count as usize);
    let (mut capacity, mut upper_total) = (count, 0usize);
    for _ in 0..layer_count {
        let nodes = source.u64()?;
        ensure!(nodes <= capacity as u64 && (count == 0 || nodes > 0), "Invalid graph layer capacity");
        let nodes = source.holds(nodes, 4 * UPPER_WIDTH as u64)?;
        upper_total += nodes;
        ensure!(upper_total <= count, "Invalid graph layer capacity");
        let mut layer = Vec::with_capacity(nodes * UPPER_WIDTH);
        source.neighbours(nodes, UPPER_WIDTH, &mut layer)?;
        layers.push(layer);
        capacity = nodes;
    }

    ensure!(source.u64()? == declared, "Invalid visual graph entry");
    source.holds(declared, 8 + 8 + FINGERPRINT_BYTES as u64)?;
    let mut ids = Vec::with_capacity(count);
    let mut fingerprints = Vec::with_capacity(count);
    let mut seen = std::collections::HashSet::with_capacity(count);
    for _ in 0..count {
        let id = source.i64()?;
        ensure!(id > 0 && seen.insert(id), "Invalid visual graph entry");
        ensure!(source.u64()? == FINGERPRINT_BYTES as u64, "Invalid visual graph entry");
        let fingerprint = source.array::<FINGERPRINT_BYTES>()?;
        ensure!(valid_fingerprint(&fingerprint), "Invalid visual graph entry");
        ids.push(id);
        fingerprints.push(String::from_utf8(fingerprint.to_vec())?);
    }
    ensure!(source.end()? == expected_hash, "Visual graph checksum mismatch");

    let parts = Parts { ef_search, dimension, points, zero, layers, ids, fingerprints };
    let graph = HnswMap::<Embedding, Entry>::deserialize(Node::Map(&parts)).map_err(|error| anyhow::anyhow!(error.0))?;
    ensure!(graph.values.len() == count && graph.iter().count() == count, "Invalid visual graph capacity");
    Ok(graph)
}

pub(super) fn read_delta<R: Read>(input: R, length: u64, expected_hash: &str, shape: &Shape<'_>) -> Result<Delta> {
    let mut source = Source { input: BufReader::with_capacity(1 << 16, input), hasher: Sha256::new(), remaining: length };
    ensure!(&source.array::<8>()? == DELTA_MAGIC, "Not a visual index change file");
    ensure!(source.u32()? == VERSION, "Unsupported visual index change version");
    ensure!(source.u32()? as usize == shape.dimension, "Visual index change dimension mismatch");
    let (added, removed) = (source.u64()?, source.u64()?);
    ensure!(added <= shape.max_points as u64 && removed <= shape.max_points as u64, "Invalid visual index change count");
    let dimension = shape.dimension;
    let added = source.holds(added, 8 + FINGERPRINT_BYTES as u64 + 4 * dimension as u64)?;
    let mut delta = Delta { added: Vec::with_capacity(added), removed: Vec::new() };
    let mut scratch = vec![0u8; dimension * 4];
    let mut seen = std::collections::HashSet::with_capacity(added);
    for _ in 0..added {
        let id = source.i64()?;
        ensure!(id > 0 && seen.insert(id), "Invalid visual index change");
        let fingerprint = source.array::<FINGERPRINT_BYTES>()?;
        ensure!(valid_fingerprint(&fingerprint), "Invalid visual index change");
        let mut vector = Vec::with_capacity(dimension);
        source.floats(&mut scratch, &mut vector)?;
        ensure!((shape.valid)(&vector), "Invalid visual index change");
        delta.added.push((vector, Entry { file_id: id, fingerprint: String::from_utf8(fingerprint.to_vec())? }));
    }
    let removed = source.holds(removed, 8)?;
    delta.removed.reserve(removed);
    let mut gone = std::collections::HashSet::with_capacity(removed);
    for _ in 0..removed {
        let id = source.i64()?;
        ensure!(id > 0 && gone.insert(id), "Invalid visual index change");
        delta.removed.push(id);
    }
    ensure!(source.end()? == expected_hash, "Visual index change checksum mismatch");
    Ok(delta)
}

/// Writes a value's primitives in field order; sequences carry a u64 length,
/// fixed-size arrays and structs carry nothing.
struct Flat<'a, W: Write> {
    out: &'a mut W,
}
impl<W: Write> Flat<'_, W> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), CodecError> {
        self.out.write_all(bytes).map_err(|error| CodecError(error.to_string()))
    }
}
fn unsupported<T>() -> Result<T, CodecError> {
    Err(CodecError("Unsupported value in visual index snapshot".into()))
}
impl<'s, W: Write> ser::Serializer for &'s mut Flat<'_, W> {
    type Ok = ();
    type Error = CodecError;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = ser::Impossible<(), CodecError>;
    type SerializeTupleVariant = ser::Impossible<(), CodecError>;
    type SerializeMap = ser::Impossible<(), CodecError>;
    type SerializeStruct = Self;
    type SerializeStructVariant = ser::Impossible<(), CodecError>;

    fn serialize_u32(self, value: u32) -> Result<(), CodecError> {
        self.put(&value.to_le_bytes())
    }
    fn serialize_u64(self, value: u64) -> Result<(), CodecError> {
        self.put(&value.to_le_bytes())
    }
    fn serialize_i64(self, value: i64) -> Result<(), CodecError> {
        self.put(&value.to_le_bytes())
    }
    fn serialize_f32(self, value: f32) -> Result<(), CodecError> {
        self.put(&value.to_le_bytes())
    }
    fn serialize_str(self, value: &str) -> Result<(), CodecError> {
        self.put(&(value.len() as u64).to_le_bytes())?;
        self.put(value.as_bytes())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, value: &T) -> Result<(), CodecError> {
        value.serialize(self)
    }
    fn serialize_seq(self, length: Option<usize>) -> Result<Self, CodecError> {
        let Some(length) = length else { return unsupported() };
        self.put(&(length as u64).to_le_bytes())?;
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, CodecError> {
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, CodecError> {
        Ok(self)
    }

    fn serialize_bool(self, _: bool) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_i8(self, _: i8) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_i16(self, _: i16) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_i32(self, _: i32) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_u8(self, _: u8) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_u16(self, _: u16) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_f64(self, _: f64) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_char(self, _: char) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_none(self) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_some<T: ?Sized + Serialize>(self, _: &T) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_unit(self) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(self, _: &'static str, _: u32, _: &'static str, _: &T) -> Result<(), CodecError> {
        unsupported()
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeTupleStruct, CodecError> {
        unsupported()
    }
    fn serialize_tuple_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Self::SerializeTupleVariant, CodecError> {
        unsupported()
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, CodecError> {
        unsupported()
    }
    fn serialize_struct_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Self::SerializeStructVariant, CodecError> {
        unsupported()
    }
}
impl<W: Write> ser::SerializeSeq for &mut Flat<'_, W> {
    type Ok = ();
    type Error = CodecError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), CodecError> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), CodecError> {
        Ok(())
    }
}
impl<W: Write> ser::SerializeTuple for &mut Flat<'_, W> {
    type Ok = ();
    type Error = CodecError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), CodecError> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), CodecError> {
        Ok(())
    }
}
impl<W: Write> ser::SerializeStruct for &mut Flat<'_, W> {
    type Ok = ();
    type Error = CodecError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, _: &'static str, value: &T) -> Result<(), CodecError> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), CodecError> {
        Ok(())
    }
}

/// The validated arrays presented to the graph's deserializer in field order.
#[derive(Clone, Copy)]
enum Node<'a> {
    Map(&'a Parts),
    Hnsw(&'a Parts),
    Points(&'a Parts),
    Entries(&'a Parts),
    Vector(&'a [f32]),
    Nodes(&'a [u32], usize),
    Neighbours(&'a [u32]),
    Layers(&'a [Vec<u32>]),
    Entry(i64, &'a str),
    U64(u64),
    U32(u32),
    F32(f32),
    I64(i64),
    Str(&'a str),
}
impl<'a> Node<'a> {
    fn length(&self) -> usize {
        match *self {
            Node::Map(_) | Node::Entry(..) => 2,
            Node::Hnsw(_) => 4,
            Node::Points(parts) | Node::Entries(parts) => parts.ids.len(),
            Node::Vector(values) => values.len(),
            Node::Nodes(values, width) => values.len() / width,
            Node::Neighbours(values) => values.len(),
            Node::Layers(layers) => layers.len(),
            Node::U64(_) | Node::U32(_) | Node::F32(_) | Node::I64(_) | Node::Str(_) => 0,
        }
    }
    fn child(&self, index: usize) -> Node<'a> {
        match *self {
            Node::Map(parts) => [Node::Hnsw(parts), Node::Entries(parts)][index],
            Node::Hnsw(parts) => [
                Node::U64(parts.ef_search),
                Node::Points(parts),
                Node::Nodes(&parts.zero, ZERO_WIDTH),
                Node::Layers(&parts.layers),
            ][index],
            Node::Points(parts) => Node::Vector(&parts.points[index * parts.dimension..(index + 1) * parts.dimension]),
            Node::Entries(parts) => Node::Entry(parts.ids[index], &parts.fingerprints[index]),
            Node::Vector(values) => Node::F32(values[index]),
            Node::Nodes(values, width) => Node::Neighbours(&values[index * width..(index + 1) * width]),
            Node::Neighbours(values) => Node::U32(values[index]),
            Node::Layers(layers) => Node::Nodes(&layers[index], UPPER_WIDTH),
            Node::Entry(id, fingerprint) => [Node::I64(id), Node::Str(fingerprint)][index],
            Node::U64(_) | Node::U32(_) | Node::F32(_) | Node::I64(_) | Node::Str(_) => unreachable!("scalars have no children"),
        }
    }
}
struct Children<'a> {
    node: Node<'a>,
    next: usize,
}
impl<'de> de::SeqAccess<'de> for Children<'de> {
    type Error = CodecError;
    fn next_element_seed<T: de::DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>, CodecError> {
        if self.next >= self.node.length() {
            return Ok(None);
        }
        self.next += 1;
        seed.deserialize(self.node.child(self.next - 1)).map(Some)
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.node.length() - self.next)
    }
}
impl<'de> de::Deserializer<'de> for Node<'de> {
    type Error = CodecError;
    fn deserialize_any<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value, CodecError> {
        match self {
            Node::U64(value) => visitor.visit_u64(value),
            Node::U32(value) => visitor.visit_u32(value),
            Node::F32(value) => visitor.visit_f32(value),
            Node::I64(value) => visitor.visit_i64(value),
            Node::Str(value) => visitor.visit_borrowed_str(value),
            node => visitor.visit_seq(Children { node, next: 0 }),
        }
    }
    fn deserialize_newtype_struct<V: de::Visitor<'de>>(self, _: &'static str, visitor: V) -> Result<V::Value, CodecError> {
        visitor.visit_newtype_struct(self)
    }
    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option unit
        unit_struct seq tuple tuple_struct map struct enum identifier ignored_any
    }
}

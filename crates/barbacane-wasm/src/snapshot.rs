//! The memory a plugin's `init` leaves, replayed into each new instance.
//!
//! `init(config)` runs once per plugin and config. Its effect on linear memory
//! is kept as the chunks that differ from a fresh instance's memory, and every
//! later instance starts fresh with those chunks written back, so it begins
//! where `init` left off and with nothing an earlier request wrote.

use wasmparser::{KnownCustom, Name, NameSectionReader, Operator, Parser, Payload};

use crate::error::WasmError;
use crate::instance::PluginInstance;

/// Granularity of the comparison, in bytes.
const CHUNK: usize = 4096;

/// Size of a WASM page, in bytes.
const PAGE: usize = 65536;

/// What `init` changed in linear memory.
#[derive(Debug)]
pub(crate) struct MemorySnapshot {
    /// Memory size after `init`, in bytes.
    size: usize,
    /// Offset and content of each chunk `init` changed.
    chunks: Vec<(usize, Box<[u8]>)>,
}

impl MemorySnapshot {
    /// The chunks of `initialised` that differ from `fresh`. Memory past the
    /// end of `fresh` was grown by `init`, so it is compared with zeros.
    pub(crate) fn diff(fresh: &[u8], initialised: &[u8]) -> Self {
        let chunks = initialised
            .chunks(CHUNK)
            .enumerate()
            .filter_map(|(index, chunk)| {
                let offset = index * CHUNK;
                let changed = match fresh.get(offset..offset + chunk.len()) {
                    Some(before) => before != chunk,
                    None => chunk.iter().any(|&b| b != 0),
                };
                changed.then(|| (offset, Box::from(chunk)))
            })
            .collect();
        Self {
            size: initialised.len(),
            chunks,
        }
    }

    /// Bring a fresh instance's memory to the state `init` left.
    pub(crate) fn restore(&self, instance: &mut PluginInstance) -> Result<(), WasmError> {
        let current = instance.memory().len();
        if self.size > current {
            instance
                .grow_memory(((self.size - current) / PAGE) as u64)
                .map_err(|e| WasmError::MemoryRestore(e.to_string()))?;
        }
        let memory = instance.memory_mut();
        for (offset, bytes) in &self.chunks {
            memory[*offset..offset + bytes.len()].copy_from_slice(bytes);
        }
        Ok(())
    }

    /// Bytes held.
    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.chunks.iter().map(|(_, b)| b.len()).sum()
    }
}

/// Whether a module's state after `init` is all in its linear memory, so a
/// fresh instance with the snapshot written back is the instance `init` left.
///
/// That holds when the module imports no global, defines no mutable global but
/// the stack pointer (which a call leaves as it found it), and changes no table
/// or passive data segment. The stack pointer is the global the `name` section
/// calls `__stack_pointer`, as LLVM names it: another mutable global, such as
/// an allocator's heap pointer, would restart at its initial value over the
/// memory `init` allocated. A module that fails any of this runs `init` for
/// every instance.
pub(crate) fn replayable(wasm: &[u8]) -> bool {
    // Imported globals are refused below, so defined globals are numbered from 0.
    let mut mutable_globals = Vec::new();
    let mut defined_globals = 0u32;
    let mut stack_pointer = None;
    for payload in Parser::new(0).parse_all(wasm) {
        let Ok(payload) = payload else {
            return false;
        };
        match payload {
            Payload::ImportSection(imports) => {
                for import in imports.into_imports() {
                    match import {
                        Ok(import) if matches!(import.ty, wasmparser::TypeRef::Global(_)) => {
                            return false
                        }
                        Ok(_) => {}
                        Err(_) => return false,
                    }
                }
            }
            Payload::GlobalSection(globals) => {
                for global in globals {
                    let Ok(global) = global else {
                        return false;
                    };
                    if global.ty.mutable {
                        mutable_globals.push(defined_globals);
                    }
                    defined_globals += 1;
                }
            }
            Payload::CustomSection(section) => {
                if let KnownCustom::Name(names) = section.as_known() {
                    match stack_pointer_index(names) {
                        Some(index) => stack_pointer = index,
                        None => return false,
                    }
                }
            }
            Payload::CodeSectionEntry(body) => {
                let Ok(mut operators) = body.get_operators_reader() else {
                    return false;
                };
                while !operators.eof() {
                    match operators.read() {
                        Ok(
                            Operator::TableSet { .. }
                            | Operator::TableGrow { .. }
                            | Operator::TableFill { .. }
                            | Operator::TableCopy { .. }
                            | Operator::TableInit { .. }
                            | Operator::ElemDrop { .. }
                            | Operator::MemoryInit { .. }
                            | Operator::DataDrop { .. },
                        ) => return false,
                        Ok(_) => {}
                        Err(_) => return false,
                    }
                }
            }
            _ => {}
        }
    }
    mutable_globals
        .iter()
        .all(|&index| stack_pointer == Some(index))
}

/// The index of the global a `name` section calls `__stack_pointer`:
/// `Some(None)` when it names none, `None` when the section is malformed.
fn stack_pointer_index(names: NameSectionReader<'_>) -> Option<Option<u32>> {
    let mut found = None;
    for name in names {
        if let Name::Global(globals) = name.ok()? {
            for naming in globals {
                let naming = naming.ok()?;
                if naming.name == "__stack_pointer" {
                    found = Some(naming.index);
                }
            }
        }
    }
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every official plugin that is built starts from a snapshot.
    #[test]
    fn every_built_official_plugin_is_replayable() {
        let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        let mut checked = 0;
        for entry in std::fs::read_dir(&plugins).expect("plugins directory") {
            let dir = entry.expect("entry").path();
            let name = dir.file_name().expect("name").to_string_lossy().to_string();
            let Ok(wasm) = std::fs::read(dir.join(format!("{name}.wasm"))) else {
                continue;
            };
            assert!(replayable(&wasm), "{name} is not replayable");
            checked += 1;
        }
        eprintln!("{checked} official plugins checked");
    }
    fn module(body: &str) -> Vec<u8> {
        wat::parse_str(format!("(module {body} (memory 1))")).expect("valid WAT")
    }

    #[test]
    fn a_module_whose_state_is_in_memory_is_replayable() {
        assert!(replayable(&module("")));
        assert!(replayable(&module(
            "(global $__stack_pointer (mut i32) (i32.const 0))"
        )));
        assert!(replayable(&module(
            "(global i32 (i32.const 0)) (global $__stack_pointer (mut i32) (i32.const 0))"
        )));
        assert!(replayable(&module(
            "(func (memory.fill (i32.const 0) (i32.const 0) (i32.const 8)))"
        )));
    }

    #[test]
    fn a_module_with_state_outside_memory_is_not() {
        for body in [
            // A mutable global the name section does not call the stack pointer
            "(global (mut i32) (i32.const 0))",
            "(global $heap (mut i32) (i32.const 1024))",
            "(global $__stack_pointer (mut i32) (i32.const 0)) (global $heap (mut i32) (i32.const 0))",
            r#"(import "env" "g" (global i32))"#,
            "(table 1 funcref) (func (table.set (i32.const 0) (ref.null func)))",
            "(table 1 funcref) (func (drop (table.grow (ref.null func) (i32.const 1))))",
            "(data \"x\") (func (data.drop 0))",
            "(data \"x\") (func (memory.init 0 (i32.const 0) (i32.const 0) (i32.const 1)))",
        ] {
            assert!(!replayable(&module(body)), "{body}");
        }
    }

    #[test]
    fn a_snapshot_holds_only_the_chunks_init_changed() {
        let fresh = vec![0u8; 4 * CHUNK];
        let mut initialised = fresh.clone();
        initialised[CHUNK + 5] = 1;
        initialised.extend_from_slice(&[0u8; CHUNK]);
        initialised.extend_from_slice(&[9u8; CHUNK]);
        let snapshot = MemorySnapshot::diff(&fresh, &initialised);
        assert_eq!(snapshot.size, 6 * CHUNK);
        assert_eq!(
            snapshot.chunks.iter().map(|(o, _)| *o).collect::<Vec<_>>(),
            vec![CHUNK, 5 * CHUNK]
        );
        assert_eq!(snapshot.held(), 2 * CHUNK);
    }

    /// A snapshot larger than the memory a fresh instance may grow to cannot
    /// be restored, and says so.
    #[test]
    fn a_snapshot_beyond_the_memory_limit_is_a_restore_error() {
        use crate::limits::PluginLimits;
        let limits = PluginLimits {
            max_memory_bytes: 2 * PAGE,
            ..PluginLimits::default()
        };
        let engine = crate::WasmEngine::with_limits(limits.clone()).expect("engine");
        let wasm = wat::parse_str(
            r#"(module
                (memory (export "memory") 1)
                (func (export "alloc") (param i32) (result i32) (i32.const 1024))
                (func (export "init") (param i32 i32) (result i32) (i32.const 0)))"#,
        )
        .expect("valid WAT");
        let module = engine
            .compile(&wasm, "small".into(), "0.0.0".into(), false)
            .expect("compiles");
        let mut instance = PluginInstance::new(&module, limits).expect("instantiates");

        let mut initialised = vec![0u8; 3 * PAGE];
        initialised[2 * PAGE] = 1;
        let snapshot = MemorySnapshot::diff(&vec![0u8; PAGE], &initialised);
        let error = snapshot.restore(&mut instance).expect_err("cannot grow");
        assert!(matches!(error, WasmError::MemoryRestore(_)), "{error:?}");
    }
}
